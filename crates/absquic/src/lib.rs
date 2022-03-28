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
//! use absquic_quinn::dev_utils;
//!
//! // get a in-process cached local self-signed ephemeral tls certificate pair
//! let (cert, pk) = dev_utils::local_ephem_tls_cert();
//!
//! // construct a new endpoint factory using the quinn backend types
//! let endpoint_factory = EndpointFactory::new(
//!     // use the quinn-udp udp backend
//!     QuinnUdpBackendFactory::new(
//!         ([127, 0, 0, 1], 0).into(), // bind only to localhost
//!         None,                       // use default max_udp_size
//!     ),
//!
//!     // use the quinn-proto powered backend driver
//!     QuinnDriverFactory::new(
//!         // defaults
//!         QuinnEndpointConfig::default(),
//!
//!         // simple server using the ephemeral self-signed cert
//!         Some(dev_utils::simple_server_config(cert, pk)),
//!
//!         // trusting client that accepts any and all tls certs
//!         dev_utils::trusting_client_config(),
//!     ),
//! );
//!
//! // bind the actual udp port
//! let (mut endpoint, _evt_recv, driver) = endpoint_factory
//!     .bind(TokioTimeoutsScheduler::new())
//!     .await
//!     .unwrap();
//!
//! // spawn the backend driver future
//! tokio::task::spawn(driver);
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

pub use absquic_core::endpoint::Endpoint;
pub use absquic_core::endpoint::EndpointEvt;
pub use absquic_core::endpoint::EndpointFactory;
pub use absquic_core::endpoint::EndpointRecv;

pub use absquic_core::connection::Connection;
pub use absquic_core::connection::ConnectionEvt;
pub use absquic_core::connection::ConnectionRecv;

pub use absquic_core::stream::ReadStream;
pub use absquic_core::stream::WriteStream;

#[cfg(feature = "absquic_quinn")]
pub use absquic_quinn::{
    QuinnClientConfig, QuinnDriverFactory, QuinnEndpointConfig,
    QuinnServerConfig,
};

#[cfg(feature = "absquic_quinn_udp")]
pub use absquic_quinn_udp::QuinnUdpBackendFactory;

#[cfg(feature = "tokio_time")]
mod tokio_time;
#[cfg(feature = "tokio_time")]
pub use tokio_time::*;
