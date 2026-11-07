use crate::sync::{Condvar, Mutex, MutexBlocking, MutexSpin, Semaphore};
use crate::task::{block_current_and_run_next, current_process, current_task};
use crate::timer::{add_timer, get_time_ms};
use alloc::sync::Arc;
/// sleep syscall
pub fn sys_sleep(ms: usize) -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_sleep",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        current_task()
            .unwrap()
            .inner_exclusive_access()
            .res
            .as_ref()
            .unwrap()
            .tid
    );
    let expire_ms = get_time_ms() + ms;
    let task = current_task().unwrap();
    add_timer(expire_ms, task);
    block_current_and_run_next();
    0
}
/// mutex create syscall
pub fn sys_mutex_create(blocking: bool) -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_mutex_create",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        current_task()
            .unwrap()
            .inner_exclusive_access()
            .res
            .as_ref()
            .unwrap()
            .tid
    );
    let process = current_process();
    let mutex: Option<Arc<dyn Mutex>> = if !blocking {
        Some(Arc::new(MutexSpin::new()))
    } else {
        Some(Arc::new(MutexBlocking::new()))
    };
    let mut process_inner = process.inner_exclusive_access();
    if let Some(id) = process_inner
        .mutex_list
        .iter()
        .enumerate()
        .find(|(_, item)| item.is_none())
        .map(|(id, _)| id)
    {
        process_inner.mutex_list[id] = mutex;
        id as isize
    } else {
        process_inner.mutex_list.push(mutex);
        process_inner.mutex_list.len() as isize - 1
    }
}
/// mutex lock syscall
pub fn sys_mutex_lock(mutex_id: usize) -> isize {
    let process = current_process();
    let task = current_task().unwrap();
    let inner = process.inner_exclusive_access();
    let Some(mutex) = inner.mutex_list.get(mutex_id).and_then(|slot| slot.clone()) else {
        return -1;
    };
    if inner.deadlock_detect {
        let mut owner = mutex.owner();
        let mut visited = alloc::vec::Vec::new();
        while let Some(thread) = owner {
            let id = Arc::as_ptr(&thread);
            if Arc::ptr_eq(&thread, &task) || visited.contains(&id) {
                return -0xdead;
            }
            visited.push(id);
            let waiting = thread.inner_exclusive_access().waiting_mutex;
            owner = waiting
                .and_then(|id| inner.mutex_list.get(id))
                .and_then(|slot| slot.as_ref())
                .and_then(|lock| lock.owner());
        }
    }
    drop(inner);
    task.inner_exclusive_access().waiting_mutex = Some(mutex_id);
    mutex.lock();
    task.inner_exclusive_access().waiting_mutex = None;
    0
}
/// mutex unlock syscall
pub fn sys_mutex_unlock(mutex_id: usize) -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_mutex_unlock",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        current_task()
            .unwrap()
            .inner_exclusive_access()
            .res
            .as_ref()
            .unwrap()
            .tid
    );
    let process = current_process();
    let process_inner = process.inner_exclusive_access();
    let mutex = Arc::clone(process_inner.mutex_list[mutex_id].as_ref().unwrap());
    drop(process_inner);
    drop(process);
    mutex.unlock();
    0
}
/// semaphore create syscall
pub fn sys_semaphore_create(res_count: usize) -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_semaphore_create",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        current_task()
            .unwrap()
            .inner_exclusive_access()
            .res
            .as_ref()
            .unwrap()
            .tid
    );
    let process = current_process();
    let mut process_inner = process.inner_exclusive_access();
    let id = if let Some(id) = process_inner
        .semaphore_list
        .iter()
        .enumerate()
        .find(|(_, item)| item.is_none())
        .map(|(id, _)| id)
    {
        process_inner.semaphore_list[id] = Some(Arc::new(Semaphore::new(res_count)));
        id
    } else {
        process_inner
            .semaphore_list
            .push(Some(Arc::new(Semaphore::new(res_count))));
        process_inner.semaphore_list.len() - 1
    };
    id as isize
}
/// semaphore up syscall
pub fn sys_semaphore_up(sem_id: usize) -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_semaphore_up",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        current_task()
            .unwrap()
            .inner_exclusive_access()
            .res
            .as_ref()
            .unwrap()
            .tid
    );
    let process = current_process();
    let process_inner = process.inner_exclusive_access();
    let sem = Arc::clone(process_inner.semaphore_list[sem_id].as_ref().unwrap());
    drop(process_inner);
    sem.up();
    0
}
/// semaphore down syscall
pub fn sys_semaphore_down(sem_id: usize) -> isize {
    let process = current_process();
    let task = current_task().unwrap();
    let inner = process.inner_exclusive_access();
    let Some(sem) = inner.semaphore_list.get(sem_id).and_then(|slot| slot.clone()) else {
        return -1;
    };
    if inner.deadlock_detect && sem.inner.exclusive_access().count <= 0 {
        // ChatGPT assisted with the resource-release simulation.
        let threads = inner.tasks.iter().flatten().collect::<alloc::vec::Vec<_>>();
        let width = inner.semaphore_list.len();
        let mut available = alloc::vec![0usize; width];
        let mut held = alloc::vec![alloc::vec![0usize; width]; threads.len()];
        let mut waiting = alloc::vec![None; threads.len()];
        let current = threads.iter().position(|thread| Arc::ptr_eq(thread, &task)).unwrap();

        for (id, semaphore) in inner.semaphore_list.iter().enumerate() {
            let Some(semaphore) = semaphore else { continue };
            let state = semaphore.inner.exclusive_access();
            available[id] = state.count.max(0) as usize;
            for owner in &state.holders {
                if let Some(index) = threads.iter().position(|thread| {
                    core::ptr::eq(Arc::as_ptr(thread), owner.as_ptr())
                }) {
                    held[index][id] += 1;
                }
            }
            for waiter in &state.wait_queue {
                if let Some(index) = threads.iter().position(|thread| Arc::ptr_eq(thread, waiter)) {
                    waiting[index] = Some(id);
                }
            }
        }

        waiting[current] = Some(sem_id);
        let mut finished = held.iter()
            .map(|resources| resources.iter().all(|&count| count == 0))
            .collect::<alloc::vec::Vec<_>>();
        loop {
            let next = (0..threads.len()).find(|&index| {
                !finished[index]
                    && waiting[index].map_or(true, |id| available[id] != 0)
            });
            let Some(index) = next else { break };
            finished[index] = true;
            for (free, &count) in available.iter_mut().zip(&held[index]) {
                *free += count;
            }
        }
        if !finished[current] {
            return -0xdead;
        }
    }
    drop(inner);
    sem.down();
    0
}
/// condvar create syscall
pub fn sys_condvar_create() -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_condvar_create",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        current_task()
            .unwrap()
            .inner_exclusive_access()
            .res
            .as_ref()
            .unwrap()
            .tid
    );
    let process = current_process();
    let mut process_inner = process.inner_exclusive_access();
    let id = if let Some(id) = process_inner
        .condvar_list
        .iter()
        .enumerate()
        .find(|(_, item)| item.is_none())
        .map(|(id, _)| id)
    {
        process_inner.condvar_list[id] = Some(Arc::new(Condvar::new()));
        id
    } else {
        process_inner
            .condvar_list
            .push(Some(Arc::new(Condvar::new())));
        process_inner.condvar_list.len() - 1
    };
    id as isize
}
/// condvar signal syscall
pub fn sys_condvar_signal(condvar_id: usize) -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_condvar_signal",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        current_task()
            .unwrap()
            .inner_exclusive_access()
            .res
            .as_ref()
            .unwrap()
            .tid
    );
    let process = current_process();
    let process_inner = process.inner_exclusive_access();
    let condvar = Arc::clone(process_inner.condvar_list[condvar_id].as_ref().unwrap());
    drop(process_inner);
    condvar.signal();
    0
}
/// condvar wait syscall
pub fn sys_condvar_wait(condvar_id: usize, mutex_id: usize) -> isize {
    trace!(
        "kernel:pid[{}] tid[{}] sys_condvar_wait",
        current_task().unwrap().process.upgrade().unwrap().getpid(),
        current_task()
            .unwrap()
            .inner_exclusive_access()
            .res
            .as_ref()
            .unwrap()
            .tid
    );
    let process = current_process();
    let process_inner = process.inner_exclusive_access();
    let condvar = Arc::clone(process_inner.condvar_list[condvar_id].as_ref().unwrap());
    let mutex = Arc::clone(process_inner.mutex_list[mutex_id].as_ref().unwrap());
    drop(process_inner);
    condvar.wait(mutex);
    0
}
/// Enable or disable deadlock detection for the current process.
pub fn sys_enable_deadlock_detect(enabled: usize) -> isize {
    match enabled {
        0 | 1 => {
            current_process().inner_exclusive_access().deadlock_detect = enabled == 1;
            0
        }
        _ => -1,
    }
}
