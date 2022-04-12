#![deny(missing_docs)]
#![deny(warnings)]
#![deny(unsafe_code)]
//! Absquic quic transport library
//!
//! ### Quickstart
//!
//! ```
//! # #[tokio::main(flavor = "multi_thread")]
//! # async fn main() {
//! # }
//! ```

/// Re-exported dependencies
pub mod deps {
    pub use absquic_core;

    #[cfg(any(feature = "tokio_runtime", feature = "tokio_udp"))]
    pub use absquic_tokio;
}

#[doc(inline)]
pub use absquic_core::{
    ep::{DynEp, DynEpRecv, EpEvt},
    con::{DynCon, DynConRecv, ConEvt},
};

#[cfg(feature = "tokio_runtime")]
#[doc(inline)]
pub use absquic_tokio::tokio_runtime::TokioRt;

#[cfg(feature = "tokio_udp")]
#[doc(inline)]
pub use absquic_tokio::tokio_udp::TokioUdpFactory;
