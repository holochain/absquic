//! Absquic_core runtime types

use crate::ep::*;
use crate::udp::*;
use crate::*;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

// move chan types into here
pub use crate::runtime::ChanResult;
pub use crate::runtime::ChannelClosed;

/// Get the next item from a futures_core::Stream
pub async fn stream_recv<S: futures_core::Stream>(
    s: Pin<&mut S>,
) -> Option<S::Item> {
    struct X<'lt, S: futures_core::Stream>(Pin<&'lt mut S>);

    impl<'lt, S: futures_core::Stream> Future for X<'lt, S> {
        type Output = Option<S::Item>;

        fn poll(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
        ) -> Poll<Self::Output> {
            futures_core::Stream::poll_next(self.0.as_mut(), cx)
        }
    }

    X(s).await
}

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
    ) -> (DynOnceSend<T>, AqFut<'static, Option<T>>);

    /// Creates a channel for multiple data transfer
    ///
    /// Note: until generic associated types are stable
    /// we have to make do with dynamic dispatch here
    fn channel<T: 'static + Send>() -> (
        DynMultiSend<T, <<Self as Rt>::Semaphore as Semaphore>::GuardTy>,
        DynMultiRecv<T, <<Self as Rt>::Semaphore as Semaphore>::GuardTy>,
    );
}

/// Helper trait for getting an endpoint builder for a particular runtime
pub trait RtEndpointBuilder<R: Rt> {
    /// Get an endpoint builder for this particular runtime
    fn bind<U, E>(udp: U, ep: E) -> E::BindFut
    where
        U: UdpFactory,
        E: EpFactory;
}

impl<R: Rt> RtEndpointBuilder<R> for R {
    #[inline(always)]
    fn bind<U, E>(udp: U, ep: E) -> E::BindFut
    where
        U: UdpFactory,
        E: EpFactory,
    {
        ep.bind::<R, U>(udp)
    }
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
}

/// MultiSend
pub trait MultiSend<T: 'static + Send>: 'static + Send + Sync {
    /// The guard type for this semaphore
    type GuardTy: SemaphoreGuard;

    /// Send
    fn send(&self, t: T, g: Self::GuardTy) -> ChanResult<()>;
}

/// MultiSend
pub type DynMultiSend<T, G> =
    Arc<dyn MultiSend<T, GuardTy = G> + 'static + Send + Sync>;

/// MultiRecv
pub type DynMultiRecv<T, G> =
    Pin<Box<dyn futures_core::Stream<Item = (T, G)> + 'static + Send>>;
