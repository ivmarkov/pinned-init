use core::{
    cell::{Cell, UnsafeCell},
    ops::{Deref, DerefMut},
    pin::Pin,
    sync::atomic::{AtomicBool, Ordering},
};
use std::{
    sync::Arc,
    thread::{current, park, sleep, Builder, Thread},
    time::Duration,
};

use pinned_init::*;
#[allow(unused_attributes)]
pub mod linked_list;
use linked_list::*;

pub struct SpinLock {
    inner: AtomicBool,
}

impl SpinLock {
    #[inline]
    pub fn acquire(&self) -> SpinLockGuard<'_> {
        while self
            .inner
            .compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {}
        SpinLockGuard(self)
    }

    #[inline]
    pub fn new() -> Self {
        Self {
            inner: AtomicBool::new(false),
        }
    }
}

pub struct SpinLockGuard<'a>(&'a SpinLock);

impl Drop for SpinLockGuard<'_> {
    #[inline]
    fn drop(&mut self) {
        self.0.inner.store(false, Ordering::Release);
    }
}

#[pin_project]
pub struct Mutex<T> {
    #[pin]
    wait_list: ListHead,
    spin_lock: SpinLock,
    locked: Cell<bool>,
    data: UnsafeCell<T>,
}

impl<T> Mutex<T> {
    #[inline]
    pub fn new(val: T) -> impl PinInit<Self> {
        pin_init!(Self {
            wait_list: ListHead::new(),
            spin_lock: SpinLock::new(),
            locked: Cell::new(false),
            data: UnsafeCell::new(val),
        })
    }

    #[inline]
    pub fn lock(&self) -> MutexGuard<'_, T> {
        let mut sguard = self.spin_lock.acquire();
        if self.locked.get() {
            stack_init!(let wait_entry = WaitEntry::insert_new(&self.wait_list));
            let wait_entry = match wait_entry {
                Ok(w) => w,
                Err(e) => match e {},
            };
            while self.locked.get() {
                drop(sguard);
                park();
                sguard = self.spin_lock.acquire();
            }
            drop(wait_entry);
        }
        self.locked.set(true);
        MutexGuard { mtx: self }
    }
}

unsafe impl<T: Send> Send for Mutex<T> {}
unsafe impl<T: Send> Sync for Mutex<T> {}

pub struct MutexGuard<'a, T> {
    mtx: &'a Mutex<T>,
}

impl<'a, T> Drop for MutexGuard<'a, T> {
    #[inline]
    fn drop(&mut self) {
        let sguard = self.mtx.spin_lock.acquire();
        self.mtx.locked.set(false);
        if let Some(list_field) = self.mtx.wait_list.next() {
            let wait_entry = list_field.as_ptr().cast::<WaitEntry>();
            unsafe { (*wait_entry).thread.unpark() };
        }
        drop(sguard);
    }
}

impl<'a, T> Deref for MutexGuard<'a, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &Self::Target {
        unsafe { &*self.mtx.data.get() }
    }
}

impl<'a, T> DerefMut for MutexGuard<'a, T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.mtx.data.get() }
    }
}

#[pin_project]
#[repr(C)]
struct WaitEntry {
    #[pin]
    wait_list: ListHead,
    thread: Thread,
}

impl WaitEntry {
    #[inline]
    fn insert_new(list: &ListHead) -> impl PinInit<Self> + '_ {
        pin_init!(Self {
            thread: current(),
            wait_list: ListHead::insert_prev(list),
        })
    }
}

fn main() {
    let mtx: Pin<Arc<Mutex<usize>>> = Arc::pin_init(Mutex::new(0)).unwrap();
    let mut handles = vec![];
    let thread_count = 20;
    let workload = 1_000_000;
    for i in 0..thread_count {
        let mtx = mtx.clone();
        handles.push(
            Builder::new()
                .name(format!("worker #{i}"))
                .spawn(move || {
                    for _ in 0..workload {
                        *mtx.lock() += 1;
                    }
                    println!("{i} halfway");
                    sleep(Duration::from_millis((i as u64) * 10));
                    for _ in 0..workload {
                        *mtx.lock() += 1;
                    }
                    println!("{i} finished");
                })
                .expect("should not fail"),
        );
    }
    for h in handles {
        h.join().expect("thread paniced");
    }
    println!("{:?}", &*mtx.lock());
    assert_eq!(*mtx.lock(), workload * thread_count * 2);
}
