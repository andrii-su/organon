pub mod config;
pub mod entity;
pub mod git;
pub mod graph;
pub mod ignore;
pub mod lifecycle;
pub mod scanner;
pub mod watcher;
pub mod workspace;

use std::sync::{Mutex, MutexGuard};

/// Lock a mutex, recovering from poisoning instead of propagating a panic.
///
/// The graph mutex is shared by the long-running watch daemon. If any thread
/// panics while holding the lock (poisoning it), the default `.lock().unwrap()`
/// would make *every* subsequent lock panic and take the whole daemon down.
/// Recovering the inner data keeps the daemon alive; a single failed operation
/// is preferable to a cascading crash.
pub trait LockRecover<T> {
    fn lock_recover(&self) -> MutexGuard<'_, T>;
}

impl<T> LockRecover<T> for Mutex<T> {
    fn lock_recover(&self) -> MutexGuard<'_, T> {
        self.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}
