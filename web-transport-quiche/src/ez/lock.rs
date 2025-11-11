use std::sync::Arc;
use std::{
    ops::{Deref, DerefMut},
    sync::{Mutex, MutexGuard},
};

// Debug wrapper for Arc<Mutex<T>> that prints lock/unlock operations
pub(crate) struct Lock<T> {
    inner: Arc<Mutex<T>>,
    name: &'static str,
}

impl<T> Clone for Lock<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            name: self.name,
        }
    }
}

impl<T> Lock<T> {
    pub fn new(value: T, name: &'static str) -> Self {
        Self {
            inner: Arc::new(Mutex::new(value)),
            name,
        }
    }

    pub fn lock(&self) -> LockGuard<'_, T> {
        println!(
            "LOCK: acquiring {} @ {:?}",
            self.name,
            std::thread::current().id()
        );
        let guard = self.inner.lock().unwrap();
        println!(
            "LOCK: acquired {} @ {:?}",
            self.name,
            std::thread::current().id()
        );
        LockGuard {
            guard,
            name: self.name,
        }
    }
}

pub(crate) struct LockGuard<'a, T> {
    guard: MutexGuard<'a, T>,
    name: &'static str,
}

impl<'a, T> Drop for LockGuard<'a, T> {
    fn drop(&mut self) {
        println!(
            "LOCK: dropping {} @ {:?}",
            self.name,
            std::thread::current().id()
        );
    }
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
