//! `Shard<T>`: a part of a `World` that can be borrowed on its own (design-p5-p6.md §2, §4).
//!
//! A shim of the PyO3 crate reaches the world through a shared borrow of its `WorldCore` and then
//! borrows one part mutably: a namespace's slot, or the entities. Two shims may hold two parts at
//! once; the same part twice is an `Error::Runtime`, never a panic, as a second `try_borrow_mut` of
//! a Python object's cell is a `RuntimeError` today. A kernel that holds the whole world (`&mut
//! World`) reaches a part with `get_mut`, which checks nothing.
//!
//! PyO3 0.23 requires every `#[pyclass]` to be `Sync`, so the flag is atomic, as in
//! `atomic_refcell`. All the borrows happen under the GIL, so the flag is never contended.

use std::cell::UnsafeCell;
use std::marker::PhantomData;
use std::ops::{Deref, DerefMut};
use std::ptr::NonNull;
use std::sync::atomic::{AtomicIsize, Ordering};

use crate::error::{Error, Result};

/// The text of a part borrowed twice.
pub const BORROWED: &str = "a part of the world is borrowed already";

const WRITING: isize = -1;

pub struct Shard<T> {
    /// 0: free; n > 0: n shared borrows; `WRITING`: one mutable borrow.
    flag: AtomicIsize,
    value: UnsafeCell<T>,
}

// SAFETY: the flag gives out either one `&mut T` or any number of `&T`, as a RwLock does, so a
// `Shard<T>` may be shared between threads when `T` may be sent and shared.
unsafe impl<T: Send + Sync> Sync for Shard<T> {}

impl<T: Default> Default for Shard<T> {
    fn default() -> Self {
        Self::new(T::default())
    }
}

impl<T> Shard<T> {
    pub const fn new(value: T) -> Self {
        Self { flag: AtomicIsize::new(0), value: UnsafeCell::new(value) }
    }

    /// The value, for the one who holds the whole: no guard can be alive.
    #[inline]
    pub fn get_mut(&mut self) -> &mut T {
        self.value.get_mut()
    }

    pub fn into_inner(self) -> T {
        self.value.into_inner()
    }

    /// A shared borrow, or `Error::Runtime(BORROWED)` while the part is borrowed mutably.
    #[inline]
    pub fn try_borrow(&self) -> Result<ShardRef<'_, T>> {
        let mut current = self.flag.load(Ordering::Relaxed);
        loop {
            if current < 0 || current == isize::MAX {
                return Err(borrowed());
            }
            match self.flag.compare_exchange_weak(current, current + 1, Ordering::Acquire, Ordering::Relaxed) {
                Ok(_) => break,
                Err(found) => current = found,
            }
        }
        // SAFETY: the flag now counts this borrow, so no `&mut T` exists until it is released.
        let value = unsafe { NonNull::new_unchecked(self.value.get()) };
        Ok(ShardRef { flag: &self.flag, value, _borrow: PhantomData })
    }

    /// A mutable borrow, or `Error::Runtime(BORROWED)` while the part is borrowed at all.
    #[inline]
    pub fn try_borrow_mut(&self) -> Result<ShardMut<'_, T>> {
        if self.flag.compare_exchange(0, WRITING, Ordering::Acquire, Ordering::Relaxed).is_err() {
            return Err(borrowed());
        }
        // SAFETY: the flag says this is the only borrow until it is released.
        let value = unsafe { NonNull::new_unchecked(self.value.get()) };
        Ok(ShardMut { flag: &self.flag, value, _borrow: PhantomData })
    }

    /// Whether any borrow is alive now.
    pub fn is_borrowed(&self) -> bool {
        self.flag.load(Ordering::Relaxed) != 0
    }
}

#[cold]
fn borrowed() -> Error {
    Error::Runtime(BORROWED.to_owned())
}

/// A shared borrow of a `Shard`, or of a part of its value (`map`).
pub struct ShardRef<'a, T: ?Sized> {
    flag: &'a AtomicIsize,
    value: NonNull<T>,
    _borrow: PhantomData<&'a T>,
}

impl<'a, T: ?Sized> ShardRef<'a, T> {
    /// The same borrow, of a part of the value.
    pub fn map<U: ?Sized>(this: Self, f: impl FnOnce(&T) -> &U) -> ShardRef<'a, U> {
        let flag = this.flag;
        // SAFETY: the borrow is alive, and moves to the result: `this` is forgotten, not released.
        let value = NonNull::from(f(unsafe { this.value.as_ref() }));
        std::mem::forget(this);
        ShardRef { flag, value, _borrow: PhantomData }
    }

    /// `map`, or the error `f` gives; the borrow is released then.
    pub fn try_map<U: ?Sized>(this: Self, f: impl FnOnce(&T) -> Result<&U>) -> Result<ShardRef<'a, U>> {
        // SAFETY: the borrow is alive while `this` is.
        let value = NonNull::from(f(unsafe { this.value.as_ref() })?);
        let flag = this.flag;
        std::mem::forget(this);
        Ok(ShardRef { flag, value, _borrow: PhantomData })
    }
}

impl<T: ?Sized> Deref for ShardRef<'_, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        // SAFETY: the flag counts this borrow.
        unsafe { self.value.as_ref() }
    }
}

impl<T: ?Sized> Drop for ShardRef<'_, T> {
    #[inline]
    fn drop(&mut self) {
        self.flag.fetch_sub(1, Ordering::Release);
    }
}

/// The mutable borrow of a `Shard`, or of a part of its value (`map`).
pub struct ShardMut<'a, T: ?Sized> {
    flag: &'a AtomicIsize,
    value: NonNull<T>,
    _borrow: PhantomData<&'a mut T>,
}

impl<'a, T: ?Sized> ShardMut<'a, T> {
    /// The same borrow, of a part of the value.
    pub fn map<U: ?Sized>(mut this: Self, f: impl FnOnce(&mut T) -> &mut U) -> ShardMut<'a, U> {
        let flag = this.flag;
        // SAFETY: the borrow is alive, and moves to the result: `this` is forgotten, not released.
        let value = NonNull::from(f(unsafe { this.value.as_mut() }));
        std::mem::forget(this);
        ShardMut { flag, value, _borrow: PhantomData }
    }

    /// `map`, or the error `f` gives; the borrow is released then.
    pub fn try_map<U: ?Sized>(mut this: Self, f: impl FnOnce(&mut T) -> Result<&mut U>) -> Result<ShardMut<'a, U>> {
        // SAFETY: the borrow is alive while `this` is.
        let value = NonNull::from(f(unsafe { this.value.as_mut() })?);
        let flag = this.flag;
        std::mem::forget(this);
        Ok(ShardMut { flag, value, _borrow: PhantomData })
    }
}

impl<T: ?Sized> Deref for ShardMut<'_, T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        // SAFETY: the flag says this is the only borrow.
        unsafe { self.value.as_ref() }
    }
}

impl<T: ?Sized> DerefMut for ShardMut<'_, T> {
    #[inline]
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: the flag says this is the only borrow.
        unsafe { self.value.as_mut() }
    }
}

impl<T: ?Sized> Drop for ShardMut<'_, T> {
    #[inline]
    fn drop(&mut self) {
        self.flag.store(0, Ordering::Release);
    }
}
