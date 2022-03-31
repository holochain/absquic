#![deny(missing_docs)]
#![deny(warnings)]
#![deny(unsafe_code)]
//! Absquic_core quic state-machine abstraction

/// Re-exported dependencies
pub mod deps {
    pub use bytes;
    pub use futures_core;
    pub use one_err;
    pub use parking_lot;
}

#[cfg(loom)]
mod loom_sync;

#[cfg(loom)]
pub(crate) mod sync {
    pub(crate) use super::loom_sync::*;
}

#[cfg(not(loom))]
pub(crate) mod sync {
    pub(crate) use parking_lot::Mutex;
    pub(crate) use std::sync::atomic;
    pub(crate) use std::sync::Arc;

    #[cfg(test)]
    pub(crate) use futures::executor::block_on;
    #[cfg(test)]
    pub(crate) use std::thread;
}

use std::future::Future;
use std::task::Context;
use std::task::Poll;

/// Absquic result type
pub type AqResult<T> = std::result::Result<T, one_err::OneErr>;

/// Absquic box future alias type
type AqBoxFut<'lt, T> = std::pin::Pin<Box<dyn Future<Output = T> + 'lt + Send>>;

/// An absquic generic future
#[must_use = "futures do nothing unless you `.await` or poll them"]
pub struct AqFut<'lt, T>(AqBoxFut<'lt, T>);

impl<'lt, T> AqFut<'lt, T> {
    /// Construct a new absquic backend unit future from a generic future
    pub fn new<F>(f: F) -> Self
    where
        F: Future<Output = T> + 'lt + Send,
    {
        Self(Box::pin(f))
    }
}

impl<'lt, T> Future for AqFut<'lt, T> {
    type Output = T;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Self::Output> {
        Future::poll(self.0.as_mut(), cx)
    }
}

/// An absquic unit future
pub type AqUnitFut = AqFut<'static, ()>;

pub mod backend;
pub mod connection;
pub mod endpoint;
pub mod runtime;
pub mod stream;
pub mod util;
