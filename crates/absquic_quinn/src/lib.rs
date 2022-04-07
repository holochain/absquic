#![deny(missing_docs)]
#![deny(warnings)]
#![deny(unsafe_code)]
//! Absquic backend powered by quinn-proto

use absquic_core::backend::*;
use absquic_core::connection::backend::*;
use absquic_core::connection::*;
use absquic_core::deps::{bytes, one_err};
use absquic_core::endpoint::backend::*;
use absquic_core::endpoint::*;
use absquic_core::runtime::*;
use absquic_core::stream::backend::*;
use absquic_core::stream::*;
use absquic_core::*;
use futures_util::stream::StreamExt;
use std::collections::HashMap;
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Context;
use std::task::Poll;

/// Re-exported dependencies
pub mod deps {
    pub use absquic_core;
    pub use quinn_proto;
    pub use rustls;
}

fn uniq() -> usize {
    use std::sync::atomic;
    static UNIQ: atomic::AtomicUsize = atomic::AtomicUsize::new(0);
    UNIQ.fetch_add(1, atomic::Ordering::Relaxed)
}

pub use quinn_proto::ClientConfig as QuinnClientConfig;
pub use quinn_proto::EndpointConfig as QuinnEndpointConfig;
pub use quinn_proto::ServerConfig as QuinnServerConfig;

// buffer size for inter-endpoint/connection channels
const CHAN_CAP: usize = 64;

// buffer size for read / write streams - arbitrary, needs experiments
const BYTES_CAP: usize = 1024 * 64;

#[cfg(any(test, feature = "dev_utils"))]
pub mod dev_utils;

mod stream;
pub(crate) use stream::*;

mod connection;
pub(crate) use connection::*;

mod endpoint;
pub(crate) use endpoint::*;

/// MaxGsoProvider provider used by absquic_quinn
pub type MaxGsoProvider = Arc<dyn Fn() -> usize + 'static + Send + Sync>;

/// In the absence of a better provider, this default will disable
/// gso, by always just returning `1`
pub fn disable_gso_provider() -> MaxGsoProvider {
    #[inline(always)]
    fn disable() -> usize {
        1
    }
    Arc::new(disable)
}

/// Absquic backend powered by quinn-proto
pub struct QuinnQuicBackendFactory {
    max_gso_provider: MaxGsoProvider,
    endpoint_config: Arc<quinn_proto::EndpointConfig>,
    server_config: Option<Arc<quinn_proto::ServerConfig>>,
    client_config: Arc<quinn_proto::ClientConfig>,
}

impl QuinnQuicBackendFactory {
    /// Construct a new absquic driver factory backed by quinn-proto
    pub fn new(
        max_gso_provider: MaxGsoProvider,
        endpoint_config: QuinnEndpointConfig,
        server_config: Option<QuinnServerConfig>,
        client_config: QuinnClientConfig,
    ) -> Self {
        Self {
            max_gso_provider,
            endpoint_config: Arc::new(endpoint_config),
            server_config: server_config.map(Arc::new),
            client_config: Arc::new(client_config),
        }
    }
}

impl QuicBackendFactory for QuinnQuicBackendFactory {
    fn bind<Runtime: AsyncRuntime, Udp: UdpBackendFactory>(
        self,
        udp_backend: Udp,
    ) -> AqFut<
        'static,
        AqResult<(MultiSender<EndpointCmd>, MultiReceiver<EndpointEvt>)>,
    > {
        let Self {
            max_gso_provider,
            endpoint_config,
            server_config,
            client_config,
        } = self;
        AqFut::new(async move {
            let (udp_cmd_send, udp_packet_send, udp_packet_recv) =
                udp_backend.bind::<Runtime>().await?;

            let endpoint =
                quinn_proto::Endpoint::new(endpoint_config, server_config);

            Ok(<EndpointDriver<Runtime>>::spawn(
                max_gso_provider,
                client_config,
                endpoint,
                udp_cmd_send,
                udp_packet_send,
                udp_packet_recv,
            ))
        })
    }
}
