use std::sync::Arc;
use std::{
    ops::{Deref, DerefMut},
    sync::{Mutex, MutexGuard},
};

/// Debug wrapper for Arc<Mutex<T>> that prints lock/unlock operations
/// TODO Remove this when deadlocks are no more.
pub(super) struct Lock<T> {
    inner: Arc<Mutex<T>>,
}

impl<T> Clone for Lock<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<T> Lock<T> {
    pub fn new(value: T) -> Self {
        Self {
            inner: Arc::new(Mutex::new(value)),
        }
    }

    pub fn lock(&self) -> LockGuard<'_, T> {
        let guard = self.inner.lock().unwrap();
        LockGuard { guard }
    }
}

pub(super) struct LockGuard<'a, T> {
    guard: MutexGuard<'a, T>,
}

impl<'a, T> Deref for LockGuard<'a, T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.guard
    }
}

impl<'a, T> DerefMut for LockGuard<'a, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.guard
    }
}
