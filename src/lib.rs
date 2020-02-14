#![feature(shared)]

use std::cell::Cell;
use std::ptr::Shared;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A Bark Pointer.
///
/// In order to send a Bark<T> across threads, you must first aquire a BarkSend by calling `sendable()` on your `Bark`.
pub struct Bark<T: ?Sized> {
    // Thread-local ref count
    thread: Shared<Cell<usize>>,
    inner: Shared<BarkInner<T>>,
}

struct BarkInner<T: ?Sized> {
    // Cross-thread ref count
    cross: AtomicUsize,
    // Possibly unsized value
    value: T,
}

/// A Bark Pointer that can be sent across threads.
///
///In order to use this value again, call the `promote()` method.
pub struct BarkSend<T: ?Sized + Send + Sync> {
    inner: Shared<BarkInner<T>>,
}

impl<T: ?Sized> BarkInner<T> {
    fn incr_cross(&self) -> usize {
        self.cross.fetch_add(1 as usize, Ordering::Release)
    }

    fn decr_cross(&self) -> usize {
        self.cross.fetch_sub(1 as usize, Ordering::Release)
    }
}

impl<T: ?Sized> Bark<T> {
    /// Create a new Bark
    pub fn new(value: T) -> Bark<T>
    where
        T: Sized,
    {
        let thread = Box::new(Cell::new(1usize));
        let inner = Box::new(BarkInner {
            cross: AtomicUsize::new(1usize),
            value: value,
        });

        unsafe {
            Bark {
                thread: Shared::new(Box::into_raw(thread)),
                inner: Shared::new(Box::into_raw(inner)),
            }
        }
    }

    /// Creates a BarkSend which can be sent across thread boundaries
    pub fn sendable(&self) -> BarkSend<T>
    where
        T: Send + Sync,
    {
        unsafe { &**self.inner }.incr_cross();
        BarkSend {
            inner: self.inner.clone(),
        }
    }

    fn decr_thread(&self) -> usize {
        unsafe {
            let prev = self.thread.as_ref().unwrap().get();
            let new = prev - 1;
            self.thread.as_mut().unwrap().set(new);
            prev
        }
    }

    fn incr_thread(&self) {
        unsafe {
            let prev = self.thread.as_ref().unwrap().get();
            let new = prev + 1;
            self.thread.as_mut().unwrap().set(new);
        }
    }
}

impl<T: ?Sized + Send + Sync> BarkSend<T> {
    /// Turns a `BarkSend<T>` back into a `Bark<T>`
    pub fn promote(self) -> Bark<T> {
        let thread = Box::new(Cell::new(1usize));

        unsafe {
            Bark {
                thread: Shared::new(Box::into_raw(thread)),
                inner: self.inner.clone(),
            }
        }
    }
}

unsafe impl<T: ?Sized + Sync> Sync for Bark<T> {}
unsafe impl<T: ?Sized + Sync + Send> Send for BarkSend<T> {}

impl<T: ?Sized> Clone for Bark<T> {
    fn clone(&self) -> Bark<T> {
        self.incr_thread();
        Bark {
            thread: self.thread.clone(),
            inner: self.inner.clone(),
        }
    }
}

impl<T: ?Sized> Drop for Bark<T> {
    fn drop(&mut self) {
        use std::mem::drop;
        // If we are the last Bark on this thread
        if self.decr_thread() == 1 {
            unsafe {
                // deallocate
                drop(Box::from_raw(*self.thread));

                // If we are the last Bark in the universe
                if (&**self.inner).decr_cross() == 1 {
                    drop(Box::from_raw(*self.inner));
                }
            }
        }
    }
}

impl<T: ?Sized + Send + Sync> Drop for BarkSend<T> {
    fn drop(&mut self) {
        unsafe {
            // If we are the last Bark in the universe
            if (&**self.inner).decr_cross() == 1 {
                drop(Box::from_raw(*self.inner));
            }
        }
    }
}

impl<T: ?Sized> std::ops::Deref for Bark<T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        unsafe { &((**self.inner).value) }
    }
}
