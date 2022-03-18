#![deny(missing_docs)]
#![deny(warnings)]
#![deny(unsafe_code)]
//! absquic backend powered by quinn-proto

use absquic_core::backend::*;
use absquic_core::endpoint::backend::*;
use absquic_core::endpoint::*;
use absquic_core::util::*;
use absquic_core::*;
use std::collections::VecDeque;
use std::future::Future;
use std::sync::Arc;
use std::task::Context;
use std::task::Poll;

const BUF_CAP: usize = 64;

/// absquic backend powered by quinn-proto
pub struct QuinnDriverFactory {
    endpoint_config: Arc<quinn_proto::EndpointConfig>,
    server_config: Option<Arc<quinn_proto::ServerConfig>>,
    client_config: Arc<quinn_proto::ClientConfig>,
}

impl QuinnDriverFactory {
    /// construct a new absquic driver factory backed by quinn-proto
    pub fn new(
        endpoint_config: quinn_proto::EndpointConfig,
        server_config: Option<quinn_proto::ServerConfig>,
        client_config: quinn_proto::ClientConfig,
    ) -> Self {
        Self {
            endpoint_config: Arc::new(endpoint_config),
            server_config: server_config.map(Arc::new),
            client_config: Arc::new(client_config),
        }
    }
}

impl BackendDriverFactory for QuinnDriverFactory {
    fn construct_endpoint(
        &self,
        udp_backend: Arc<dyn UdpBackendFactory>,
    ) -> AqBoxFut<
        'static,
        AqResult<(Sender<EndpointCmd>, Receiver<EndpointEvt>, BackendDriver)>,
    > {
        let endpoint_config = self.endpoint_config.clone();
        let server_config = self.server_config.clone();
        let client_config = self.client_config.clone();
        Box::pin(async move {
            let (udp_send, udp_recv, udp_driver) = udp_backend.bind().await?;

            let endpoint =
                quinn_proto::Endpoint::new(endpoint_config, server_config);

            let (cmd_send, cmd_recv) = channel(BUF_CAP);
            let (evt_send, evt_recv) = channel(BUF_CAP);

            let driver = QuinnDriver {
                udp_driver,
                udp_send,
                udp_send_buf: VecDeque::with_capacity(BUF_CAP),
                udp_recv,
                client_config,
                endpoint,
                cmd_recv,
                evt_send,
            };

            let driver = BackendDriver::new(driver);

            Ok((cmd_send, evt_recv, driver))
        })
    }
}

#[derive(Clone, Copy)]
enum Disposition {
    /// it's safe to return pending, i.e. we have wakers registered
    /// everywhere needed to ensure continued function of the driver
    PendOk,

    /// we need another poll loop to continue safely
    MoreWork,
}

impl Disposition {
    pub fn merge(&mut self, oth: Self) {
        use Disposition::*;
        *self = match (*self, oth) {
            (MoreWork, _) => MoreWork,
            (_, MoreWork) => MoreWork,
            (PendOk, PendOk) => PendOk,
        }
    }
}

struct QuinnDriver {
    udp_driver: BackendDriver,
    udp_send: DynUdpBackendSender,
    udp_send_buf: VecDeque<OutUdpPacket>,
    udp_recv: DynUdpBackendReceiver,
    client_config: Arc<quinn_proto::ClientConfig>,
    endpoint: quinn_proto::Endpoint,
    cmd_recv: Receiver<EndpointCmd>,
    evt_send: Sender<EndpointEvt>,
}

impl Future for QuinnDriver {
    type Output = ();

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Self::Output> {
        match self.poll_inner(cx) {
            Err(e) => {
                tracing::error!("{:?}", e);
                Poll::Ready(())
            }
            Ok(r) => r,
        }
    }
}

impl QuinnDriver {
    pub fn poll_inner(&mut self, cx: &mut Context<'_>) -> AqResult<Poll<()>> {
        for _ in 0..32 {
            // first poll the udp driver --
            // if we don't have a udp driver we don't have an endpoint
            match std::pin::Pin::new(&mut self.udp_driver).poll(cx) {
                Poll::Pending => (),
                Poll::Ready(_) => return Err("UdpDriverEnded".into()),
            }

            let now = std::time::Instant::now();

            // order matters significantly here
            // consider carefully before changing
            let mut disp = self.poll_cmd_recv(cx)?;
            disp.merge(self.poll_udp_recv(cx, now)?);
            disp.merge(self.buffer_endpoint_transmits());
            // connections here
            disp.merge(self.poll_udp_send(cx)?);

            match disp {
                Disposition::PendOk => return Ok(Poll::Pending),
                Disposition::MoreWork => (),
            }
        }

        // we're not done, but neither are we pending...
        // need to trigger the waker, and try again
        cx.waker().wake_by_ref();
        Ok(Poll::Pending)
    }

    pub fn poll_cmd_recv(
        &mut self,
        cx: &mut Context<'_>,
    ) -> AqResult<Disposition> {
        use EndpointCmd::*;
        loop {
            match self.cmd_recv.poll_recv(cx) {
                Poll::Pending => return Ok(Disposition::PendOk),
                Poll::Ready(None) => return Err("CmdRecvEnded".into()),
                Poll::Ready(Some(cmd)) => match cmd {
                    GetLocalAddress(sender) => {
                        let recv = self.udp_send.local_addr();
                        recv.forward(sender);
                    }
                    Connect {
                        sender,
                        addr,
                        server_name,
                    } => {
                        match self.endpoint.connect(
                            (*self.client_config).clone(),
                            addr,
                            &server_name,
                        ) {
                            Err(e) => sender.send(Err(format!("{:?}", e).into())),
                            Ok((_hnd, _con)) => {
                                // TODO - establish connection
                                todo!()
                            }
                        }
                    }
                }
            }
        }
    }

    pub fn poll_udp_recv(
        &mut self,
        cx: &mut Context<'_>,
        now: std::time::Instant,
    ) -> AqResult<Disposition> {
        loop {
            match self.udp_recv.poll_recv(cx) {
                Poll::Pending => return Ok(Disposition::PendOk),
                Poll::Ready(None) => return Err("UdpRecvEnded".into()),
                Poll::Ready(Some(packet)) => {
                    if let Some((_hnd, evt)) = self.endpoint.handle(
                        now,
                        packet.src_addr,
                        packet.dst_ip,
                        packet
                            .ecn
                            .map(|ecn| {
                                quinn_proto::EcnCodepoint::from_bits(ecn)
                            })
                            .flatten(),
                        packet.data,
                    ) {
                        use quinn_proto::DatagramEvent::*;
                        match evt {
                            ConnectionEvent(_evt) => {
                                // TODO - forward to connection
                            }
                            NewConnection(_con) => {
                                // TODO - create connection
                            }
                        }
                    }
                }
            }
        }
    }

    pub fn buffer_endpoint_transmits(&mut self) -> Disposition {
        let mut did_work = false;

        while self.udp_send_buf.len() < BUF_CAP {
            if let Some(transmit) = self.endpoint.poll_transmit() {
                did_work = true;
                self.udp_send_buf.push_back(OutUdpPacket {
                    dst_addr: transmit.destination,
                    src_ip: transmit.src_ip,
                    segment_size: transmit.segment_size,
                    ecn: transmit.ecn.map(|ecn| ecn as u8),
                    data: transmit.contents,
                });
            }
        }

        if did_work {
            Disposition::MoreWork
        } else {
            Disposition::PendOk
        }
    }

    pub fn poll_udp_send(
        &mut self,
        cx: &mut Context<'_>,
    ) -> AqResult<Disposition> {
        let mut did_work = false;

        while !self.udp_send_buf.is_empty() {
            match self.udp_send.poll_send(cx) {
                Poll::Pending => return Ok(Disposition::PendOk),
                Poll::Ready(Err(e)) => return Err(e),
                Poll::Ready(Ok(permit)) => {
                    did_work = true;
                    permit(self.udp_send_buf.pop_front().unwrap());
                }
            }
        }

        if did_work {
            Ok(Disposition::MoreWork)
        } else {
            Ok(Disposition::PendOk)
        }
    }
}
