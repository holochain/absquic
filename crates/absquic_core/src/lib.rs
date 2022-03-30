#![deny(missing_docs)]
#![deny(warnings)]
#![deny(unsafe_code)]
//! Absquic_core quic state-machine abstraction

/// Re-exported dependencies
pub mod deps {
    pub use bytes;
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

/// Absquic result type
pub type AqResult<T> = std::result::Result<T, one_err::OneErr>;

/// Absquic box future alias type
pub type AqBoxFut<'lt, T> =
    std::pin::Pin<Box<dyn std::future::Future<Output = T> + 'lt + Send>>;

pub mod backend;
pub mod connection;
pub mod endpoint;
pub mod runtime;
pub mod stream;
pub mod util;
