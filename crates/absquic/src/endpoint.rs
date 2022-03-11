//! absquic Endpoint

use crate::connection::*;
use crate::driver::*;
use crate::types::*;
use crate::AqResult;
use crate::OutChan;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Context;
use std::task::Poll;

/// Events associated with an endpoint
pub enum EndpointEvt {
    /// An incoming connection
    Connection(Connection, ConnectionEvtSrc),
}

/// Stream of incoming endpoint events
pub struct EndpointEvtSrc(OutChan<EndpointEvt>);

impl EndpointEvtSrc {
    /// get the next incoming endpoint event
    #[inline(always)]
    pub fn poll_recv(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Option<EndpointEvt>> {
        self.0.poll_recv(cx)
    }

    /// get the next incoming endpoint event
    pub async fn recv(&mut self) -> Option<EndpointEvt> {
        struct X<'lt>(&'lt mut EndpointEvtSrc);

        impl Future for X<'_> {
            type Output = Option<EndpointEvt>;

            fn poll(
                mut self: Pin<&mut Self>,
                cx: &mut Context<'_>,
            ) -> Poll<Self::Output> {
                self.0.poll_recv(cx)
            }
        }

        X(self).await
    }
}

/// Quic Endpoint
#[derive(Clone)]
pub struct Endpoint(DriverCore, quinn_proto::ClientConfig);

impl Endpoint {
    /// create a new endpoint
    pub fn new<B: Backend + 'static + Send>(
        schedule_timeout: DynScheduledFactory,
        config: quinn_proto::EndpointConfig,
        server_config: Option<quinn_proto::ServerConfig>,
        client_config: quinn_proto::ClientConfig,
        backend: B,
    ) -> (Self, EndpointDriver, EndpointEvtSrc) {
        let backend: Box<dyn Backend + 'static + Send> = Box::new(backend);
        let endpoint = quinn_proto::Endpoint::new(
            Arc::new(config),
            server_config.map(Arc::new),
        );
        let (core, evt_src) =
            DriverCore::new(schedule_timeout, backend, endpoint);
        let evt_src = EndpointEvtSrc(evt_src);
        let driver = EndpointDriver::new(&core);
        let this = Self(core.clone(), client_config);
        (this, driver, evt_src)
    }

    /// get the current local address this endpoint is bound to
    pub fn local_addr(&self) -> AqResult<SocketAddr> {
        self.0.local_addr()
    }

    /// open a new outgoing connection
    pub fn connect(
        &self,
        remote: SocketAddr,
        server_name: &str,
    ) -> AqResult<(Connection, ConnectionEvtSrc)> {
        self.0.connect(self.1.clone(), remote, server_name)
    }
}
