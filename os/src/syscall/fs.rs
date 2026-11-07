use crate::fs::{make_pipe, open_file, OpenFlags, Stat, ROOT_INODE};
use crate::mm::{translated_byte_buffer, translated_refmut, translated_str, PTEFlags, PageTable, UserBuffer, VirtAddr};
use crate::task::{current_process, current_task, current_user_token};
use alloc::sync::Arc;
/// write syscall
pub fn sys_write(fd: usize, buf: *const u8, len: usize) -> isize {
    trace!(
        "kernel:pid[{}] sys_write",
        current_task().unwrap().process.upgrade().unwrap().getpid()
    );
    let token = current_user_token();
    let process = current_process();
    let inner = process.inner_exclusive_access();
    if fd >= inner.fd_table.len() {
        return -1;
    }
    if let Some(file) = &inner.fd_table[fd] {
        if !file.writable() {
            return -1;
        }
        let file = file.clone();
        // release current task TCB manually to avoid multi-borrow
        drop(inner);
        file.write(UserBuffer::new(translated_byte_buffer(token, buf, len))) as isize
    } else {
        -1
    }
}
/// read syscall
pub fn sys_read(fd: usize, buf: *const u8, len: usize) -> isize {
    trace!(
        "kernel:pid[{}] sys_read",
        current_task().unwrap().process.upgrade().unwrap().getpid()
    );
    let token = current_user_token();
    let process = current_process();
    let inner = process.inner_exclusive_access();
    if fd >= inner.fd_table.len() {
        return -1;
    }
    if let Some(file) = &inner.fd_table[fd] {
        let file = file.clone();
        if !file.readable() {
            return -1;
        }
        // release current task TCB manually to avoid multi-borrow
        drop(inner);
        trace!("kernel: sys_read .. file.read");
        file.read(UserBuffer::new(translated_byte_buffer(token, buf, len))) as isize
    } else {
        -1
    }
}
/// open sys
pub fn sys_open(path: *const u8, flags: u32) -> isize {
    trace!(
        "kernel:pid[{}] sys_open",
        current_task().unwrap().process.upgrade().unwrap().getpid()
    );
    let process = current_process();
    let token = current_user_token();
    let path = translated_str(token, path);
    if let Some(inode) = open_file(path.as_str(), OpenFlags::from_bits(flags).unwrap()) {
        let mut inner = process.inner_exclusive_access();
        let fd = inner.alloc_fd();
        inner.fd_table[fd] = Some(inode);
        fd as isize
    } else {
        -1
    }
}
/// close syscall
pub fn sys_close(fd: usize) -> isize {
    trace!(
        "kernel:pid[{}] sys_close",
        current_task().unwrap().process.upgrade().unwrap().getpid()
    );
    let process = current_process();
    let mut inner = process.inner_exclusive_access();
    if fd >= inner.fd_table.len() {
        return -1;
    }
    if inner.fd_table[fd].is_none() {
        return -1;
    }
    inner.fd_table[fd].take();
    0
}
/// pipe syscall
pub fn sys_pipe(pipe: *mut usize) -> isize {
    trace!(
        "kernel:pid[{}] sys_pipe",
        current_task().unwrap().process.upgrade().unwrap().getpid()
    );
    let process = current_process();
    let token = current_user_token();
    let mut inner = process.inner_exclusive_access();
    let (pipe_read, pipe_write) = make_pipe();
    let read_fd = inner.alloc_fd();
    inner.fd_table[read_fd] = Some(pipe_read);
    let write_fd = inner.alloc_fd();
    inner.fd_table[write_fd] = Some(pipe_write);
    *translated_refmut(token, pipe) = read_fd;
    *translated_refmut(token, unsafe { pipe.add(1) }) = write_fd;
    0
}
/// dup syscall
pub fn sys_dup(fd: usize) -> isize {
    trace!(
        "kernel:pid[{}] sys_dup",
        current_task().unwrap().process.upgrade().unwrap().getpid()
    );
    let process = current_process();
    let mut inner = process.inner_exclusive_access();
    if fd >= inner.fd_table.len() {
        return -1;
    }
    if inner.fd_table[fd].is_none() {
        return -1;
    }
    let new_fd = inner.alloc_fd();
    inner.fd_table[new_fd] = Some(Arc::clone(inner.fd_table[fd].as_ref().unwrap()));
    new_fd as isize
}

// ChatGPT assisted with porting filesystem syscalls to ch8.
fn user_byte(table: &PageTable, address: usize, permission: PTEFlags) -> Option<*mut u8> {
    if address == 0 || address >= (1usize << 38) {
        return None;
    }
    let va = VirtAddr::from(address);
    let pte = table.translate(va.floor())?;
    if !pte.flags().contains(PTEFlags::V | PTEFlags::U | permission) {
        return None;
    }
    Some(pte.ppn().get_bytes_array().as_mut_ptr().wrapping_add(va.page_offset()))
}

fn user_name(ptr: *const u8) -> Option<alloc::string::String> {
    let table = PageTable::from_token(current_user_token());
    let mut bytes = [0u8; 28];
    for (offset, byte) in bytes.iter_mut().enumerate() {
        let address = (ptr as usize).checked_add(offset)?;
        *byte = unsafe { user_byte(&table, address, PTEFlags::R)?.read() };
        if *byte == 0 {
            return core::str::from_utf8(&bytes[..offset]).ok().map(Into::into);
        }
    }
    None
}

pub fn sys_fstat(fd: usize, st: *mut Stat) -> isize {
    let task = current_process();
    let file = {
        let inner = task.inner_exclusive_access();
        inner.fd_table.get(fd).and_then(|slot| slot.as_ref()).cloned()
    };
    let Some(stat) = file.and_then(|file| file.stat()) else {
        return -1;
    };
    let table = PageTable::from_token(current_user_token());
    let bytes = stat.to_bytes();
    let mut destinations = [core::ptr::null_mut::<u8>(); core::mem::size_of::<Stat>()];
    for (offset, destination) in destinations.iter_mut().enumerate() {
        let Some(address) = (st as usize).checked_add(offset) else {
            return -1;
        };
        let Some(ptr) = user_byte(&table, address, PTEFlags::W) else {
            return -1;
        };
        *destination = ptr;
    }
    for (destination, byte) in destinations.into_iter().zip(bytes) {
        unsafe { destination.write(byte) };
    }
    0
}

pub fn sys_linkat(old_name: *const u8, new_name: *const u8) -> isize {
    let (Some(old), Some(new)) = (user_name(old_name), user_name(new_name)) else {
        return -1;
    };
    if ROOT_INODE.link(&old, &new) { 0 } else { -1 }
}

pub fn sys_unlinkat(name: *const u8) -> isize {
    let Some(name) = user_name(name) else {
        return -1;
    };
    if ROOT_INODE.unlink(&name) { 0 } else { -1 }
}
