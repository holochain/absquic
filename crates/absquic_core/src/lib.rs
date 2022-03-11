#![deny(missing_docs)]
#![deny(warnings)]
#![deny(unsafe_code)]
//! absquic_core quic state-machine abstraction

/// re-exported dependencies
pub mod deps {
    pub use bytes;
    pub use one_err;
    pub use parking_lot;
}

/// absquic result type
pub type AqResult<T> = std::result::Result<T, one_err::OneErr>;

pub mod util;

pub mod stream;

pub mod connection;

pub mod endpoint;
