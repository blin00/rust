// Copyright 2016 Amanieu d'Antras
//
// Licensed under the Apache License, Version 2.0, <LICENSE-APACHE or
// http://apache.org/licenses/LICENSE-2.0> or the MIT license <LICENSE-MIT or
// http://opensource.org/licenses/MIT>, at your option. This file may not be
// copied, modified, or distributed except according to those terms.

use crate::sys::mutex::{Mutex, TOKEN_HANDOFF, TOKEN_NORMAL};
use core::{
    ptr,
    sync::atomic::{AtomicPtr, Ordering},
};
use crate::time::{Duration, Instant};
use crate::sys::parking_lot_core::{self, ParkResult, RequeueOp, UnparkResult, DEFAULT_PARK_TOKEN};

#[inline]
fn to_deadline(timeout: Duration) -> Option<Instant> {
    Instant::now().checked_add(timeout)
}

pub struct Condvar {
    state: AtomicPtr<Mutex>,
}

pub type MovableCondvar = Condvar;

unsafe impl Send for Condvar {}
unsafe impl Sync for Condvar {}

impl Condvar {
    pub const fn new() -> Condvar {
        Condvar {
            state: AtomicPtr::new(ptr::null_mut()),
        }
    }

    #[inline]
    pub unsafe fn init(&mut self) {}

    #[inline]
    pub unsafe fn wait(&self, mutex: &Mutex) {
        self.wait_until_internal(mutex, None);
    }

    #[inline]
    pub unsafe fn wait_timeout(&self, mutex: &Mutex, dur: Duration) -> bool {
        let deadline = to_deadline(dur);
        self.wait_until_internal(mutex, deadline)
    }

    #[inline]
    pub unsafe fn notify_one(&self) {
        // Nothing to do if there are no waiting threads
        let state = self.state.load(Ordering::Relaxed);
        if state.is_null() {
            return;
        }
        self.notify_one_slow(state);
    }

    #[inline]
    pub unsafe fn notify_all(&self) {
        // Nothing to do if there are no waiting threads
        let state = self.state.load(Ordering::Relaxed);
        if state.is_null() {
            return;
        }

        self.notify_all_slow(state);
    }

    pub unsafe fn destroy(&self) {}

    #[cold]
    unsafe fn notify_one_slow(&self, mutex: *mut Mutex) -> bool {
        // Unpark one thread and requeue the rest onto the mutex
        let from = self as *const _ as usize;
        let to = mutex as usize;
        let validate = || {
            // Make sure that our atomic state still points to the same
            // mutex. If not then it means that all threads on the current
            // mutex were woken up and a new waiting thread switched to a
            // different mutex. In that case we can get away with doing
            // nothing.
            if self.state.load(Ordering::Relaxed) != mutex {
                return RequeueOp::Abort;
            }

            // Unpark one thread if the mutex is unlocked, otherwise just
            // requeue everything to the mutex. This is safe to do here
            // since unlocking the mutex when the parked bit is set requires
            // locking the queue. There is the possibility of a race if the
            // mutex gets locked after we check, but that doesn't matter in
            // this case.
            if (*mutex).mark_parked_if_locked() {
                RequeueOp::RequeueOne
            } else {
                RequeueOp::UnparkOne
            }
        };
        let callback = |_op, result: UnparkResult| {
            // Clear our state if there are no more waiting threads
            if !result.have_more_threads {
                self.state.store(ptr::null_mut(), Ordering::Relaxed);
            }
            TOKEN_NORMAL
        };
        let res = parking_lot_core::unpark_requeue(from, to, validate, callback);

        res.unparked_threads + res.requeued_threads != 0
    }

    #[cold]
    unsafe fn notify_all_slow(&self, mutex: *mut Mutex) -> usize {
        // Unpark one thread and requeue the rest onto the mutex
        let from = self as *const _ as usize;
        let to = mutex as usize;
        let validate = || {
            // Make sure that our atomic state still points to the same
            // mutex. If not then it means that all threads on the current
            // mutex were woken up and a new waiting thread switched to a
            // different mutex. In that case we can get away with doing
            // nothing.
            if self.state.load(Ordering::Relaxed) != mutex {
                return RequeueOp::Abort;
            }

            // Clear our state since we are going to unpark or requeue all
            // threads.
            self.state.store(ptr::null_mut(), Ordering::Relaxed);

            // Unpark one thread if the mutex is unlocked, otherwise just
            // requeue everything to the mutex. This is safe to do here
            // since unlocking the mutex when the parked bit is set requires
            // locking the queue. There is the possibility of a race if the
            // mutex gets locked after we check, but that doesn't matter in
            // this case.
            if (*mutex).mark_parked_if_locked() {
                RequeueOp::RequeueAll
            } else {
                RequeueOp::UnparkOneRequeueRest
            }
        };
        let callback = |op, result: UnparkResult| {
            // If we requeued threads to the mutex, mark it as having
            // parked threads. The RequeueAll case is already handled above.
            if op == RequeueOp::UnparkOneRequeueRest && result.requeued_threads != 0 {
                (*mutex).mark_parked();
            }
            TOKEN_NORMAL
        };
        let res = parking_lot_core::unpark_requeue(from, to, validate, callback);

        res.unparked_threads + res.requeued_threads
    }

    unsafe fn wait_until_internal(&self, mutex: &Mutex, timeout: Option<Instant>) -> bool {
        let result;
        let mut bad_mutex = false;
        let mut requeued = false;
        {
            let addr = self as *const _ as usize;
            let lock_addr = mutex as *const _ as *mut _;
            let validate = || {
                // Ensure we don't use two different mutexes with the same
                // Condvar at the same time. This is done while locked to
                // avoid races with notify_one
                let state = self.state.load(Ordering::Relaxed);
                if state.is_null() {
                    self.state.store(lock_addr, Ordering::Relaxed);
                } else if state != lock_addr {
                    bad_mutex = true;
                    return false;
                }
                true
            };
            let before_sleep = || {
                // Unlock the mutex before sleeping...
                mutex.unlock();
            };
            let timed_out = |k, was_last_thread| {
                // If we were requeued to a mutex, then we did not time out.
                // We'll just park ourselves on the mutex again when we try
                // to lock it later.
                requeued = k != addr;

                // If we were the last thread on the queue then we need to
                // clear our state. This is normally done by the
                // notify_{one,all} functions when not timing out.
                if !requeued && was_last_thread {
                    self.state.store(ptr::null_mut(), Ordering::Relaxed);
                }
            };
            result = parking_lot_core::park(
                addr,
                validate,
                before_sleep,
                timed_out,
                DEFAULT_PARK_TOKEN,
                timeout,
            );
        }

        // Panic if we tried to use multiple mutexes with a Condvar. Note
        // that at this point the MutexGuard is still locked. It will be
        // unlocked by the unwinding logic.
        if bad_mutex {
            panic!("attempted to use a condition variable with more than one mutex");
        }

        // ... and re-lock it once we are done sleeping
        if result != ParkResult::Unparked(TOKEN_HANDOFF) {
            mutex.lock();
        }

        result.is_unparked() || requeued
    }
}
