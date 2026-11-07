//! Process management syscalls
use crate::task::{change_program_brk, exit_current_and_run_next, suspend_current_and_run_next};

#[repr(C)]
#[derive(Debug)]
pub struct TimeVal {
    pub sec: usize,
    pub usec: usize,
}

/// task exits and submit an exit code
pub fn sys_exit(_exit_code: i32) -> ! {
    trace!("kernel: sys_exit");
    exit_current_and_run_next();
    panic!("Unreachable in sys_exit!");
}

/// current task gives up resources for other tasks
pub fn sys_yield() -> isize {
    trace!("kernel: sys_yield");
    suspend_current_and_run_next();
    0
}

/// YOUR JOB: get time with second and microsecond
/// HINT: You might reimplement it with virtual memory management.
/// HINT: What if [`TimeVal`] is splitted by two pages ?
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
/// Read or write a user byte, or query the current task's syscall count.
pub fn sys_trace(trace_request: usize, id: usize, data: usize) -> isize {
    use crate::mm::{PTEFlags, PageTable, VirtAddr};
    use crate::task::{current_syscall_count, current_user_token};

    trace!("kernel: sys_trace");

    let permission = match trace_request {
        0 => PTEFlags::R,
        1 => PTEFlags::W,
        2 => return current_syscall_count(id).map_or(-1, |count| count as isize),
        _ => return -1,
    };

    // Reject non-canonical Sv39 addresses before VirtAddr truncates them.
    if (((id << 25) as isize) >> 25) as usize != id {
        return -1;
    }

    let va = VirtAddr::from(id);
    let page_table = PageTable::from_token(current_user_token());
    let Some(pte) = page_table.translate(va.floor()) else {
        return -1;
    };
    if !pte.flags().contains(PTEFlags::V | PTEFlags::U | permission) {
        return -1;
    }

    let byte = &mut pte.ppn().get_bytes_array()[va.page_offset()];
    if trace_request == 0 {
        unsafe { core::ptr::read_volatile(byte) as isize }
    } else {
        unsafe { core::ptr::write_volatile(byte, data as u8) };
        0
    }
}

/// Map pages into the current task's address space.
pub fn sys_mmap(start: usize, len: usize, prot: usize) -> isize {
    if crate::task::mmap_current(start, len, prot) {
        0
    } else {
        -1
    }
}

/// Remove pages mapped by mmap from the current task.
pub fn sys_munmap(start: usize, len: usize) -> isize {
    if crate::task::munmap_current(start, len) {
        0
    } else {
        -1
    }
}
/// change data segment size
pub fn sys_sbrk(size: i32) -> isize {
    trace!("kernel: sys_sbrk");
    if let Some(old_brk) = change_program_brk(size) {
        old_brk as isize
    } else {
        -1
    }
}
