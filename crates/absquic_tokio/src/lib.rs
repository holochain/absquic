#![deny(missing_docs)]
#![deny(warnings)]
#![deny(unsafe_code)]
//! Absquic_tokio tokio runtime and udp backend

/// Re-exported dependencies
pub mod deps {
    pub use absquic_core;
    pub use tokio;
}

#[cfg(feature = "tokio_runtime")]
pub mod tokio_runtime;

#[cfg(feature = "tokio_runtime")]
#[doc(inline)]
pub use tokio_runtime::TokioRt;

#[cfg(feature = "tokio_udp")]
pub mod tokio_udp;

#[cfg(feature = "tokio_udp")]
#[doc(inline)]
pub use tokio_udp::TokioUdpFactory;
