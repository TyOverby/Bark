#![feature(shared)]

use std::ptr::Shared;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::cell::Cell;

pub struct Bark<T: ?Sized> {
    thread: Shared<Cell<usize>>,
    inner: Shared<BarkInner<T>>,
}

struct BarkInner<T: ?Sized> {
    cross: AtomicUsize,
    value: T,
}

pub struct BarkSend<T: ?Sized> {
    inner: Shared<BarkInner<T>>,
}

impl <T: ?Sized> BarkInner<T> {
    fn mod_cross(&self, p: isize) -> usize {
        if p < 0 {
            self.cross.fetch_sub(-p as usize, Ordering::Release)
        } else {
            self.cross.fetch_add(p as usize, Ordering::Release)
        }
    }
}

impl <T: ?Sized> Bark<T> {
    pub fn new(value: T) -> Bark<T> where T: Sized {
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

    pub fn sendable(&self) -> BarkSend<T> {
        unsafe{&**self.inner}.mod_cross(1);
        BarkSend {
            inner: self.inner.clone()
        }
    }

    fn mod_thread(&self, p: isize) -> usize {
        unsafe { 
            let prev = self.thread.as_ref().unwrap().get();
            let new = (prev as isize + p as isize) as usize;
            self.thread.as_mut().unwrap().set(new);
            new
        }
    }

}

impl <T: ?Sized> BarkSend<T> {
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

unsafe impl <T: ?Sized + Sync> Sync for Bark<T> {}
unsafe impl <T: ?Sized + Sync + Send> Send for BarkSend<T> {}

impl <T: ?Sized> Clone for Bark<T> {
    fn clone(&self) -> Bark<T> {
        self.mod_thread(1);
        Bark {
            thread: self.thread.clone(),
            inner: self.inner.clone(),
        }
    }
}

impl <T: ?Sized> Drop for Bark<T> {
    fn drop(&mut self) {
        use std::mem::drop;
        // If we are the last Bark on this thread
        if self.mod_thread(-1) == 0 {
            unsafe {
                // deallocate
                drop(Box::from_raw(*self.thread));
            }

            
            // If we are the last Bark in the universe
            if unsafe {&**self.inner}.mod_cross(-1) == 0 {
                unsafe {
                    drop(Box::from_raw(*self.inner));
                }
            }
        }
    }
}

impl <T: ?Sized> Drop for BarkSend<T> {
    fn drop(&mut self) {
        // If we are the last Bark in the universe
        if (unsafe {&**self.inner}).mod_cross(-1) == 0 {
            unsafe {
                drop(Box::from_raw(*self.inner));
            }
        }
    }
}

impl<T: ?Sized> std::ops::Deref for Bark<T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        unsafe {
            &((**self.inner).value)
        }
    }
}
