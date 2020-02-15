#![feature(dropck_eyepatch, cell_update)]

use std::{
    alloc::{self, Layout},
    cell::Cell,
    fmt, mem,
    pin::Pin,
    ptr::{self, NonNull},
    sync::atomic::{self, AtomicUsize, Ordering},
};

const MAX_REFCOUNT: usize = isize::max_value() as usize;

/// A Bark Pointer.
///
/// In order to send a Bark<T> across threads, you must first aquire a BarkSend by calling `sendable()` on your `Bark`.
pub struct Bark<T: ?Sized> {
    // Thread-local ref count
    thread: NonNull<Cell<usize>>,
    inner: NonNull<BarkInner<T>>,
}

struct BarkInner<T: ?Sized> {
    // Cross-thread ref count
    cross: AtomicUsize,
    // Possibly unsized value
    value: T,
}

/// A Bark Pointer that can be sent across threads.
///
/// In order to use this value again, call the `promote()` method.
pub struct BarkSend<T: ?Sized> {
    inner: NonNull<BarkInner<T>>,
}

impl<T: ?Sized> Bark<T> {
    /// Create a new Bark.
    #[inline]
    pub fn new(value: T) -> Bark<T>
    where
        T: Sized,
    {
        let thread = Box::new(Cell::new(1usize));
        let inner = Box::new(BarkInner {
            cross: AtomicUsize::new(1usize),
            value: value,
        });

        // TODO: When `Box::into_raw_non_null` is stabilized, switch over.
        unsafe {
            Bark {
                thread: NonNull::new_unchecked(Box::into_raw(thread)),
                inner: NonNull::new_unchecked(Box::into_raw(inner)),
            }
        }
    }

    /// Creates a BarkSend which can be sent across thread boundaries.
    #[inline]
    pub fn sendable(&self) -> BarkSend<T>
    where
        T: Send + Sync,
    {
        // We know our refcount is at least 1, so the memory ordering
        // of this refcount increment doesn't particularly matter...
        // If something on another thread checks it, then it's definitely
        // already > 1. So if we're only on one thread, we can guarantee
        // nothing weird is going to happen out of order anyways because
        // of the single-thread guarantee of instruction reordering; and
        // if we're on multiple, then there's no way that this can cause
        // other threads to mistakenly see a value of our cross-thread
        // refcount that matters, since we're definitely at 2 or more,
        // and `1` is the only significant refcount value. I guess 0
        // is also significant, but only in that if we have 0 outside
        // of the situation in `Drop` or `try_unwrap`, something has gone
        // HORRIBLY wrong...
        let old = self.inner().cross.fetch_add(1, Ordering::Relaxed);

        // Oh, we do have to account for possible overflows, though.
        // The maximum refcount is `isize::max_value()`, matching with the std `Arc` limit.
        // If we go over it, abort, because seriously, what the hell are you doing?
        if old > MAX_REFCOUNT {
            ::std::process::abort();
        }

        BarkSend {
            inner: self.inner.clone(),
        }
    }

    #[inline]
    pub fn is_unique(&mut self) -> bool {
        // The `Acquire` ordering on this load ensures that we're seeing an up-to-date
        // count on our cross-thread refcount, which helps to ensure that we don't have
        // a stale > 1 cross-thread refcount. (? does the `LoadStore` barrier property
        // also do something useful here?)
        self.thread().get() == 1 && self.inner().cross.load(Ordering::Acquire) == 1
    }

    #[inline]
    fn inner(&self) -> &BarkInner<T> {
        // If we have a live `Bark`, the data inside must be valid,
        // and it's fine giving out an immutable reference to it
        // because it's `Sync`.
        unsafe { self.inner.as_ref() }
    }

    #[inline]
    pub fn get_mut(this: &mut Self) -> Option<&mut T> {
        if this.is_unique() {
            unsafe { Some(Bark::get_mut_unchecked(this)) }
        } else {
            None
        }
    }

    #[inline]
    pub unsafe fn get_mut_unchecked(this: &mut Self) -> &mut T {
        &mut this.inner.as_mut().value
    }

    #[inline]
    fn thread(&self) -> &Cell<usize> {
        // Same safety rationale as `.inner()`.
        unsafe { self.thread.as_ref() }
    }
}

impl<T> Bark<T> {
    /// Constructs a new `Pin<Bark<T>>`. If `T` does not implement `Unpin`,
    /// then `data` will be pinned in memory and unable to be moved.
    #[inline]
    pub fn pin(data: T) -> Pin<Bark<T>> {
        unsafe { Pin::new_unchecked(Bark::new(data)) }
    }
}

impl<T: Clone> Bark<T> {
    #[inline]
    pub fn make_mut(this: &mut Self) -> &mut T {
        if !this.is_unique() {
            *this = Bark::new((**this).clone());
        }

        unsafe { Self::get_mut_unchecked(this) }
    }

    #[inline]
    pub fn try_unwrap(this: Self) -> Result<T, Self> {
        // `drop` contains an explanation of the atomics. The
        // atomic operations here are ripped straight from the
        // Rust stdlib's `Arc`, so they should be sound.
        //
        // This one's a bit neat, though. `compare_exchange` has
        // an ordering for comparison success and an ordering for
        // comparison failure. In the case of comparison success,
        // we want an `Ordering::Release` in order to sync with
        // the `Acquire` fence following this if statement. But
        // if the comparison fails, we're not going to drop
        // anything, so we never hit the fence and can just get
        // out of here.
        if this.thread().get() == 1
            && this
                .inner()
                .cross
                .compare_exchange(1, 0, Ordering::Release, Ordering::Relaxed)
                .is_err()
        {
            return Err(this);
        }

        atomic::fence(Ordering::Acquire);

        // The `Release` on our cross-thread refcount prevents any of the reads/writes
        // our `Arc` after the cmpxchg.
        //
        // Meanwhile, in the case of the cmpxchg succeeding, the subsequent `Acquire`
        // fence turns the `cmpxchg` into a barrier which prevents any of the
        // reads/writes which happen below, from moving up above the barrier.
        //
        // Interestingly, according to the Rust documentation, using `AcqRel` in the
        // case of `compare_and_exchange` will prevent any form of relaxed accesses.
        // Using `Ordering::Release`, we ensure that all reads/writes occur before
        // the cmpxchg barrier, but semantically the successful load comes after;
        // This means that it can be reordered around reads/writes that come after.
        // So `AcqRel` on the `cmpxchg` causes the consequent load to have `Acquire`
        // semantics, which is unnecessary here.
        //
        // TODO: Is this actually the case? I know this pattern is valid because it's
        // what Rust's `Arc` uses and I trust `Arc` since it's been formally verified;
        // but my analysis may be off. Is the "load" part of the cmpxchg actually
        // something we don't care about, or is the returned value considered separate
        // from the information about whether or not we actually succeeded in the compare?
        //
        // According to https://www.felixcloutier.com/x86/cmpxchg, on Intel x86/64,
        // the success information is in the ZF flag and the returned value *is* separate;
        // so at least on x86, this may be significant, if the processor separates the
        // two after decoding the instruction. Cool!

        unsafe {
            // We read out the value which was previously inside, without moving it.
            // Then, we carefully deallocate the box which previously held the value
            // and our cross-thread refcount, *without* dropping its contents. We have
            // to do this manually; the code here is based on the example found in
            // the Rust docs for `Box::into_raw`:
            //
            // https://doc.rust-lang.org/std/boxed/struct.Box.html#method.into_raw
            let elem = ptr::read(&this.inner().value as *const T);

            // This is due to `Box`'s layout. Again, see the `Box::into_raw` example.
            alloc::dealloc(this.inner.as_ptr() as *mut u8, Layout::new::<T>());

            // There's nothing we need to pull out of the thread-local refcount, so
            // we can just convert it back to a `Box` and let it go.
            let _ = Box::from_raw(this.thread.as_ptr());

            // We definitely do not want to run our `Drop` implementation now; it'd
            // cause a use-after-free.
            mem::forget(this);

            Ok(elem)
        }
    }
}

impl<T: ?Sized> BarkSend<T> {
    /// Turns a `BarkSend<T>` back into a `Bark<T>`.
    #[inline]
    pub fn promote(self) -> Bark<T> {
        let thread = Box::new(Cell::new(1usize));

        unsafe {
            Bark {
                thread: NonNull::new_unchecked(Box::into_raw(thread)),
                inner: self.inner.clone(),
            }
        }
    }

    #[inline]
    fn inner(&self) -> &BarkInner<T> {
        // Same safety rationale as `Bark::inner`.
        unsafe { self.inner.as_ref() }
    }
}

unsafe impl<T: ?Sized + Sync> Sync for Bark<T> {}
unsafe impl<T: ?Sized + Sync + Send> Send for BarkSend<T> {}

impl<T: ?Sized> Clone for Bark<T> {
    #[inline]
    fn clone(&self) -> Bark<T> {
        let thread = self.thread().get();

        // We do *not* want to overflow this. Look, if you have this many
        // `Bark`s floating around, what *are* you doing? Wtf.
        if self.thread().update(|i| i + 1) > MAX_REFCOUNT {
            ::std::process::abort();
        }

        self.thread().set(thread + 1);

        Bark {
            thread: self.thread,
            inner: self.inner,
        }
    }
}

unsafe impl<#[may_dangle] T: ?Sized> Drop for Bark<T> {
    #[inline]
    fn drop(&mut self) {
        if self.thread().update(|i| i - 1) != 1 {
            return;
        }

        unsafe {
            // deallocate
            mem::drop(Box::from_raw(self.thread.as_ptr()));
        }

        // If we are the last Bark in the universe
        if self.inner().cross.fetch_sub(1 as usize, Ordering::Release) != 1 {
            return;
        }

        // Taken from the `Arc` implementation in `std::sync::Arc`:
        //
        // This fence is needed to prevent reordering of use of the data and
        // deletion of the data.  Because it is marked `Release`, the decreasing
        // of the reference count synchronizes with this `Acquire` fence. This
        // means that use of the data happens before decreasing the reference
        // count, which happens before this fence, which happens before the
        // deletion of the data.
        //
        // As explained in the [Boost documentation][1],
        //
        // > It is important to enforce any possible access to the object in one
        // > thread (through an existing reference) to *happen before* deleting
        // > the object in a different thread. This is achieved by a "release"
        // > operation after dropping a reference (any access to the object
        // > through this reference must obviously happened before), and an
        // > "acquire" operation before deleting the object.
        //
        // In particular, while the contents of an Arc are usually immutable, it's
        // possible to have interior writes to something like a Mutex<T>. Since a
        // Mutex is not acquired when it is deleted, we can't rely on its
        // synchronization logic to make writes in thread A visible to a destructor
        // running in thread B.
        //
        // Also note that the Acquire fence here could probably be replaced with an
        // Acquire load, which could improve performance in highly-contended
        // situations. See [2].
        //
        // [1]: (www.boost.org/doc/libs/1_55_0/doc/html/atomic/usage_examples.html)
        // [2]: (https://github.com/rust-lang/rust/pull/41714)
        atomic::fence(Ordering::Acquire);

        unsafe {
            mem::drop(Box::from_raw(self.inner.as_ptr()));
        }
    }
}

unsafe impl<#[may_dangle] T: ?Sized> Drop for BarkSend<T> {
    #[inline]
    fn drop(&mut self) {
        // If we are the last Bark in the universe
        if self.inner().cross.fetch_sub(1 as usize, Ordering::Release) != 1 {
            return;
        }

        // See the implementation of `Drop` for `Bark` for an explanation of
        // the memory ordering here.
        atomic::fence(Ordering::Acquire);

        unsafe {
            mem::drop(Box::from_raw(self.inner.as_ptr()));
        }
    }
}

impl<T: ?Sized> std::ops::Deref for Bark<T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        unsafe { &self.inner.as_ref().value }
    }
}

impl<T: ?Sized> std::ops::Deref for BarkSend<T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        unsafe { &self.inner.as_ref().value }
    }
}

impl<T: ?Sized + fmt::Debug> fmt::Debug for Bark<T> {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        (**self).fmt(f)
    }
}

impl<T: ?Sized + fmt::Debug> fmt::Debug for BarkSend<T> {
    #[inline]
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        (**self).fmt(f)
    }
}
