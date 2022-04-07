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
//! use absquic::*;
//! use absquic::quinn::*;
//! use absquic::quinn_udp::*;
//!
//! // get a in-process cached local self-signed ephemeral tls certificate pair
//! let (cert, pk) = dev_utils::localhost_self_signed_tls_cert();
//!
//! let (udp_backend, max_gso_provider) = QuinnUdpBackendFactory::new(
//!     ([127, 0, 0, 1], 0).into(), // bind only to localhost
//!     None,                       // use default max_udp_size
//! );
//!
//! let quic_backend = QuinnQuicBackendFactory::new(
//!     // from quinn udp backend
//!     max_gso_provider,
//!
//!     // defaults
//!     QuinnEndpointConfig::default(),
//!
//!     // simple server using the ephemeral self-signed cert
//!     Some(dev_utils::simple_server_config(cert, pk)),
//!
//!     // trusting client that accepts any and all tls certs
//!     dev_utils::trusting_client_config(),
//! );
//!
//! // bind the backends to get our endpoint handle and event receiver
//! let (mut endpoint, _evt_recv) = TokioRuntime::build_endpoint(
//!     udp_backend,
//!     quic_backend,
//! ).await.unwrap();
//!
//! // we can now make calls on the endpoint handle
//! assert!(endpoint.local_address().await.is_ok());
//!
//! // and we could receive events on the event receiver like
//! // while let Some(evt) = _evt_recv.recv().await {}
//! # }
//! ```

/// Re-exported dependencies
pub mod deps {
    pub use absquic_core;

    #[cfg(feature = "absquic_quinn")]
    pub use absquic_quinn;

    #[cfg(feature = "absquic_quinn_udp")]
    pub use absquic_quinn_udp;
}

pub use absquic_core::connection::Connection;
pub use absquic_core::connection::ConnectionEvt;
pub use absquic_core::endpoint::Endpoint;
pub use absquic_core::endpoint::EndpointEvt;
pub use absquic_core::runtime::MultiReceiver;
pub use absquic_core::runtime::MultiSender;
pub use absquic_core::runtime::OnceSender;
pub use absquic_core::stream::ReadStream;
pub use absquic_core::stream::WriteStream;
pub use absquic_core::AqFut;
pub use absquic_core::AqResult;

#[cfg(feature = "tokio_runtime")]
pub use absquic_core::tokio_runtime::TokioRuntime;

/// `feature = "absquic_quinn"` quic backend types
#[cfg(feature = "absquic_quinn")]
pub mod quinn {
    pub use absquic_quinn::{
        dev_utils, disable_gso_provider, MaxGsoProvider, QuinnClientConfig,
        QuinnEndpointConfig, QuinnQuicBackendFactory, QuinnServerConfig,
    };
}

/// `feature = "absquic_quinn_udp"` udp backend types
#[cfg(feature = "absquic_quinn_udp")]
pub mod quinn_udp {
    pub use absquic_quinn_udp::{MaxGsoProvider, QuinnUdpBackendFactory};
}
