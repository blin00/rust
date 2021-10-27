/*
A gutted copy of https://github.com/mvdnes/spin-rs/blob/501060db3e88e23fd94e8797e035cc74acd49eb5/src/rw_lock.rs

The MIT License (MIT)

Copyright (c) 2014 Mathijs van de Nes

Permission is hereby granted, free of charge, to any person obtaining a copy
of this software and associated documentation files (the "Software"), to deal
in the Software without restriction, including without limitation the rights
to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is
furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all
copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
SOFTWARE.
*/

use core::sync::atomic::{AtomicUsize, Ordering};
use crate::thread::yield_now as relax;

#[repr(transparent)]
pub struct RawRwLock {
    lock: AtomicUsize,
}

const READER: usize = 1 << 1;
const WRITER: usize = 1;

// Same unsafe impls as `std::sync::RwLock`
unsafe impl Send for RawRwLock {}
unsafe impl Sync for RawRwLock {}

impl RawRwLock {
    #[inline]
    pub const fn new() -> RawRwLock {
        RawRwLock {
            lock: AtomicUsize::new(0),
        }
    }

    #[inline]
    pub fn try_lock_shared(&self) -> bool {
        let value = self.lock.fetch_add(READER, Ordering::Acquire);
        if value & WRITER != 0 {
            // Lock is taken, undo.
            self.lock.fetch_sub(READER, Ordering::Release);
            false
        } else {
            true
        }
    }

    #[inline]
    pub fn lock_shared(&self) {
        loop {
            match self.try_lock_shared() {
                true => return,
                false => relax(),
            }
        }
    }

    #[inline(always)]
    fn try_lock_exclusive_internal(&self, strong: bool) -> bool {
        compare_exchange(
            &self.lock,
            0,
            WRITER,
            Ordering::Acquire,
            Ordering::Relaxed,
            strong,
        )
        .is_ok()
    }

    #[inline]
    pub fn lock_exclusive(&self) {
        loop {
            match self.try_lock_exclusive_internal(false) {
                true => return,
                false => relax(),
            }
        }
    }

    #[inline(always)]
    pub fn try_lock_exclusive(&self) -> bool {
        self.try_lock_exclusive_internal(true)
    }

    #[inline]
    pub fn unlock_shared(&self) {
        debug_assert!(self.lock.load(Ordering::Relaxed) & !WRITER > 0);
        self.lock.fetch_sub(READER, Ordering::Release);
    }

    #[inline]
    pub fn unlock_exclusive(&self) {
        debug_assert_eq!(self.lock.load(Ordering::Relaxed) & !WRITER, 0);
        self.lock.fetch_and(!WRITER, Ordering::Release);
    }
}

impl Default for RawRwLock {
    fn default() -> RawRwLock {
        Self::new()
    }
}

#[inline(always)]
fn compare_exchange(
    atomic: &AtomicUsize,
    current: usize,
    new: usize,
    success: Ordering,
    failure: Ordering,
    strong: bool,
) -> Result<usize, usize> {
    if strong {
        atomic.compare_exchange(current, new, success, failure)
    } else {
        atomic.compare_exchange_weak(current, new, success, failure)
    }
}
