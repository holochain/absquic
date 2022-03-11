#![deny(missing_docs)]
#![deny(warnings)]
#![deny(unsafe_code)]
//! absquic generic driver for arbitrary QUIC backend udp implementations

/// re-exported dependencies
pub mod dependencies {
    pub use bytes;
    pub use one_err;
    pub use parking_lot;
    pub use quinn_proto;
}

/// absquic result type
pub type AqResult<T> = std::result::Result<T, one_err::OneErr>;

pub mod types;

mod out_chan;
use out_chan::*;

pub mod driver;

pub mod stream;

pub mod connection;

pub mod endpoint;
pub use endpoint::Endpoint;