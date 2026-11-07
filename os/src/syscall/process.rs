//! Process management syscalls
use alloc::sync::Arc;

use crate::{
    loader::get_app_data_by_name,
    mm::{translated_refmut, translated_str},
    task::{
        add_task, current_task, current_user_token, exit_current_and_run_next,
        suspend_current_and_run_next,
    },
};

#[repr(C)]
#[derive(Debug)]
pub struct TimeVal {
    pub sec: usize,
    pub usec: usize,
}

/// task exits and submit an exit code
pub fn sys_exit(exit_code: i32) -> ! {
    trace!("kernel:pid[{}] sys_exit", current_task().unwrap().pid.0);
    exit_current_and_run_next(exit_code);
    panic!("Unreachable in sys_exit!");
}

/// current task gives up resources for other tasks
pub fn sys_yield() -> isize {
    trace!("kernel:pid[{}] sys_yield", current_task().unwrap().pid.0);
    suspend_current_and_run_next();
    0
}

pub fn sys_getpid() -> isize {
    trace!("kernel: sys_getpid pid:{}", current_task().unwrap().pid.0);
    current_task().unwrap().pid.0 as isize
}

pub fn sys_fork() -> isize {
    trace!("kernel:pid[{}] sys_fork", current_task().unwrap().pid.0);
    let current_task = current_task().unwrap();
    let new_task = current_task.fork();
    let new_pid = new_task.pid.0;
    // modify trap context of new_task, because it returns immediately after switching
    let trap_cx = new_task.inner_exclusive_access().get_trap_cx();
    // we do not have to move to next instruction since we have done it before
    // for child process, fork returns 0
    trap_cx.x[10] = 0;
    // add new task to scheduler
    add_task(new_task);
    new_pid as isize
}

pub fn sys_exec(path: *const u8) -> isize {
    trace!("kernel:pid[{}] sys_exec", current_task().unwrap().pid.0);
    let token = current_user_token();
    let path = translated_str(token, path);
    if let Some(data) = get_app_data_by_name(path.as_str()) {
        let task = current_task().unwrap();
        task.exec(data);
        0
    } else {
        -1
    }
}

/// If there is not a child process whose pid is same as given, return -1.
/// Else if there is a child process but it is still running, return -2.
pub fn sys_waitpid(pid: isize, exit_code_ptr: *mut i32) -> isize {
    trace!("kernel::pid[{}] sys_waitpid [{}]", current_task().unwrap().pid.0, pid);
    let task = current_task().unwrap();
    // find a child process

    // ---- access current PCB exclusively
    let mut inner = task.inner_exclusive_access();
    if !inner
        .children
        .iter()
        .any(|p| pid == -1 || pid as usize == p.getpid())
    {
        return -1;
        // ---- release current PCB
    }
    let pair = inner.children.iter().enumerate().find(|(_, p)| {
        // ++++ temporarily access child PCB exclusively
        p.inner_exclusive_access().is_zombie() && (pid == -1 || pid as usize == p.getpid())
        // ++++ release child PCB
    });
    if let Some((idx, _)) = pair {
        let child = inner.children.remove(idx);
        // confirm that child will be deallocated after being removed from children list
        assert_eq!(Arc::strong_count(&child), 1);
        let found_pid = child.getpid();
        // ++++ temporarily access child PCB exclusively
        let exit_code = child.inner_exclusive_access().exit_code;
        // ++++ release child PCB
        *translated_refmut(inner.memory_set.token(), exit_code_ptr) = exit_code;
        found_pid as isize
    } else {
        -2
    }
    // ---- release current PCB automatically
}

/// Write the current time to a checked user buffer, including across pages.
// ChatGPT assisted with the ch4 implementation reused here.
pub fn sys_get_time(ts: *mut TimeVal, _tz: usize) -> isize {
    use crate::mm::{PTEFlags, PageTable, VirtAddr};
    use crate::task::current_user_token;
    use crate::timer::get_time_us;

    trace!("kernel: sys_get_time");

    let start = ts as usize;
    if start == 0 {
        return -1;
    }

    let now = get_time_us();
    let sec = (now / 1_000_000).to_ne_bytes();
    let usec = (now % 1_000_000).to_ne_bytes();

    let page_table = PageTable::from_token(current_user_token());
    let mut destinations = [core::ptr::null_mut::<u8>(); core::mem::size_of::<TimeVal>()];

    for (offset, destination) in destinations.iter_mut().enumerate() {
        let Some(addr) = start.checked_add(offset) else {
            return -1;
        };
        // Sv39 要求地址的高位是第 38 位的符号扩展。
        if (((addr << 25) as isize) >> 25) as usize != addr {
            return -1;
        }

        let va = VirtAddr::from(addr);
        let Some(pte) = page_table.translate(va.floor()) else {
            return -1;
        };
        if !pte.flags().contains(PTEFlags::V | PTEFlags::U | PTEFlags::W) {
            return -1;
        }
        *destination = pte.ppn().get_bytes_array().as_mut_ptr().wrapping_add(va.page_offset());
    }

    for (destination, byte) in destinations.into_iter().zip(sec.into_iter().chain(usec)) {
        // 上面已验证该地址位于当前任务有效、可写的用户页。
        unsafe { destination.write(byte) };
    }
    0
}

/// Map anonymous pages in the current process.
pub fn sys_mmap(start: usize, len: usize, prot: usize) -> isize {
    let task = current_task().unwrap();
    let mut inner = task.inner_exclusive_access();
    if inner.memory_set.mmap(start, len, prot) {
        0
    } else {
        -1
    }
}

/// Unmap anonymous pages in the current process.
pub fn sys_munmap(start: usize, len: usize) -> isize {
    let task = current_task().unwrap();
    let mut inner = task.inner_exclusive_access();
    if inner.memory_set.munmap(start, len) {
        0
    } else {
        -1
    }
}

/// change data segment size
pub fn sys_sbrk(size: i32) -> isize {
    trace!("kernel:pid[{}] sys_sbrk", current_task().unwrap().pid.0);
    if let Some(old_brk) = current_task().unwrap().change_program_brk(size) {
        old_brk as isize
    } else {
        -1
    }
}

/// Load an executable directly into a new child process.
// ChatGPT assisted with reusing the process constructor and linking the child.
pub fn sys_spawn(path: *const u8) -> isize {
    let path = translated_str(current_user_token(), path);
    let Some(data) = get_app_data_by_name(path.as_str()) else {
        return -1;
    };

    let parent = current_task().unwrap();
    let child = Arc::new(crate::task::TaskControlBlock::new(data));
    child.inner_exclusive_access().parent = Some(Arc::downgrade(&parent));
    parent.inner_exclusive_access().children.push(Arc::clone(&child));

    let pid = child.getpid();
    add_task(child);
    pid as isize
}

/// Set the current process's scheduling priority.
// ChatGPT assisted with the priority validation and scheduler changes.
pub fn sys_set_priority(prio: isize) -> isize {
    if prio < 2 {
        return -1;
    }
    current_task().unwrap().inner_exclusive_access().priority = prio as usize;
    prio
}
