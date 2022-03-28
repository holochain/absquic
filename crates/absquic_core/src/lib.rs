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

/// Absquic result type
pub type AqResult<T> = std::result::Result<T, one_err::OneErr>;

/// Absquic box future alias type
pub type AqBoxFut<'lt, T> =
    std::pin::Pin<Box<dyn std::future::Future<Output = T> + 'lt + Send>>;

pub mod util;

pub mod stream;

pub mod connection;

pub mod endpoint;

pub mod backend;
