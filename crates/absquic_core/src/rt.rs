//! Absquic_core runtime types

use crate::*;
use std::future::Future;
use std::sync::Arc;

/// Abstraction for a particular async / futures runtime
pub trait Rt: 'static + Send + Sync {
    /// A completion handle to a spawned task.
    /// The task will continue even if this handle is dropped.
    /// this item should implement Future, but should NOT be #\[must_use\]
    type SpawnFut: Future<Output = ()> + 'static + Send;

    /// Sleep future return type
    type SleepFut: Future<Output = ()> + 'static + Send;

    /// Semaphore type
    type Semaphore: Semaphore;

    /// Spawn a task to execute the given future, return a future
    /// that will complete when the task completes, but will not
    /// affect the task if dropped / cancelled
    fn spawn<F>(f: F) -> Self::SpawnFut
    where
        F: Future<Output = ()> + 'static + Send;

    /// Returns a future that will complete at the specified instant,
    /// or immediately if the instant is already past
    fn sleep(until: std::time::Instant) -> Self::SleepFut;

    /// Create an async semaphore instance
    fn semaphore(limit: usize) -> Self::Semaphore;

    /// Creates a slot for a single data transfer
    ///
    /// Note: until generic associated types are stable
    /// we have to make do with dynamic dispatch here
    fn one_shot<T: 'static + Send>(
    ) -> (DynOnceSend<T>, BoxFut<'static, Option<T>>);

    /// Creates a channel for multiple data transfer
    ///
    /// Note: until generic associated types are stable
    /// we have to make do with dynamic dispatch here
    fn channel<T: 'static + Send>() -> (DynMultiSend<T>, BoxRecv<'static, T>);
}

/// OnceSend
pub type DynOnceSend<T> = Box<dyn FnOnce(T) + 'static + Send>;

/// Guard indicating we have a semaphore permit
pub trait SemaphoreGuard: 'static + Send {}

/// Async semaphore abstraction
pub trait Semaphore: 'static + Send + Sync {
    /// The guard type for this semaphore
    type GuardTy: SemaphoreGuard;

    /// The aquire future result type
    type AcquireFut: Future<Output = Self::GuardTy> + 'static + Send;

    /// Acquire a semaphore permit guard
    fn acquire(&self) -> Self::AcquireFut;

    /// Attempt to acquire an avaliable semaphore permit guard without blocking
    fn try_acquire(&self) -> Option<Self::GuardTy>;
}

/// MultiSend
pub trait MultiSend<T: 'static + Send>: 'static + Send + Sync {
    /// Send
    fn send(&self, t: T) -> Result<()>;
}

/// MultiSend
pub type DynMultiSend<T> = Arc<dyn MultiSend<T> + 'static + Send + Sync>;
