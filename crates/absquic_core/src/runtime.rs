//! Absquic_core runtime types

use crate::util::*;
use crate::*;
use std::future::Future;
use std::task::Context;
use std::task::Poll;

/// Abstraction for a particular async / futures runtime
pub trait AsyncRuntime: 'static + Send + Sync {
    /// Spawn a task to execute the future, returning a future
    /// that will complete when the task completes, but will not
    /// affect the task if dropped / cancelled
    fn spawn(&self, fut: AqUnitFut) -> AqUnitFut;

    /// Returns a driver that will complete at the specified instant,
    /// or immediately if the instant is already past
    fn sleep(&self, time: std::time::Instant) -> AqUnitFut;
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
