#![deny(missing_docs)]
#![deny(warnings)]
#![deny(unsafe_code)]
//! tx3_quic generic driver for arbitrary QUIC backend udp implementations

/// re-exported dependencies
pub mod dependencies {
    pub use bytes;
    pub use one_err;
    pub use parking_lot;
    pub use quinn_proto;
}

/// tx3_quic result type
pub type Tx3Result<T> = std::result::Result<T, one_err::OneErr>;

pub mod types;

mod out_chan;
use out_chan::*;

pub mod driver;

pub mod connection;

pub mod endpoint;
pub use endpoint::Endpoint;
