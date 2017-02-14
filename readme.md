## What is Bark?

`Bark<T>` is a pointer type for reference-counted data similar to `Arc<T>` or `Rc<T>`.

Unlike `Arc`, `Bark` only uses atomic operations when crossing threads, or when all `Bark`s
on a thread are gone.

This means that `Bark` is as cheap as an `Rc` when doing thread-local clones and drops, but once
you send one to another thread, it'll start tracking that thread seperately and correctly updates
the cross-thread reference count!
