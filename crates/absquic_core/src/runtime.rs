//! Absquic_core runtime types

use crate::util::*;
use crate::*;
use std::future::Future;
use std::sync::Arc;
use std::task::Context;
use std::task::Poll;

/// Abstraction for a particular async / futures runtime
pub trait AsyncRuntimeBackend: 'static + Send + Sync {
    /// Spawn a task to execute the given future, return a future
    /// that will complete when the task completes, but will not
    /// affect the task if dropped / cancelled
    fn spawn(&self, fut: AqBoxFut<'static, ()>) -> AqBoxFut<'static, ()>;

    /// Returns a future that will complete at the specified instant,
    /// or immediately if the instant is already past
    fn sleep(&self, time: std::time::Instant) -> AqBoxFut<'static, ()>;
}

/// Abstraction for a particular async / futures runtime
#[derive(Clone)]
pub struct AsyncRuntime(Arc<dyn AsyncRuntimeBackend + 'static + Send + Sync>);

impl AsyncRuntime {
    /// Construct a new AsyncRuntime instance
    pub fn new<B: AsyncRuntimeBackend>(b: B) -> Self {
        Self(Arc::new(b))
    }

    /// Spawn a task into the runtime
    pub fn spawn<R, F>(&self, f: F) -> SpawnHnd<R>
    where
        R: 'static + Send,
        F: Future<Output = AqResult<R>> + 'static + Send,
    {
        let (s, r) = one_shot_channel();
        let fut = self.0.spawn(Box::pin(async move {
            s.send(f.await);
        }));
        SpawnHnd(Box::pin(async move {
            fut.await;
            r.await.map_err(|_| one_err::OneErr::new("TaskCancelled"))
        }))
    }

    /// Returns a future that will complete at the specified instant,
    /// or immediately if the instant is already past
    pub fn sleep(
        &self,
        time: std::time::Instant,
    ) -> impl Future<Output = ()> + 'static + Send + Unpin {
        self.0.sleep(time)
    }
}

/// A completion handle to a spawned task.
/// The task will continue even if this handle is dropped.
/// Note, this struct implements Future but is intentionally NOT #\[must_use\]
pub struct SpawnHnd<R: 'static + Send>(pub AqBoxFut<'static, AqResult<R>>);

impl<R: 'static + Send> Future for SpawnHnd<R> {
    type Output = AqResult<R>;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Self::Output> {
        std::pin::Pin::new(&mut self.0).poll(cx)
    }
}
