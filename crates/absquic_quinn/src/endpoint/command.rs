use super::*;

pin_project_lite::pin_project! {
    pub struct CommandDriver<Runtime: AsyncRuntime> {
        client_config: Arc<quinn_proto::ClientConfig>,
        ep_cmd_send: MultiSenderPoll<EpCmd>,
        udp_packet_send: MultiSenderPoll<OutUdpPacket>,
        #[pin]
        ep_cmd_recv: futures_util::stream::SelectAll<EpCmdRecv>,
        ep_cmd_recv_closed: bool,
        udp_cmd_send: MultiSenderPoll<UdpBackendCmd>,
        udp_cmd_send_closed: bool,
        _p: std::marker::PhantomData<Runtime>,
    }
}

impl<Runtime: AsyncRuntime> CommandDriver<Runtime> {
    pub fn new(
        client_config: Arc<quinn_proto::ClientConfig>,
        ep_cmd_send: MultiSender<EpCmd>,
        udp_packet_send: MultiSender<OutUdpPacket>,
        ep_cmd_recv: futures_util::stream::SelectAll<EpCmdRecv>,
        udp_cmd_send: MultiSender<UdpBackendCmd>,
    ) -> Self {
        Self {
            client_config,
            ep_cmd_send: MultiSenderPoll::new(ep_cmd_send),
            udp_packet_send: MultiSenderPoll::new(udp_packet_send),
            ep_cmd_recv,
            ep_cmd_recv_closed: false,
            udp_cmd_send: MultiSenderPoll::new(udp_cmd_send),
            udp_cmd_send_closed: false,
            _p: std::marker::PhantomData,
        }
    }

    pub fn poll(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        endpoint: &mut quinn_proto::Endpoint,
        connections: &mut ConMap,
        more_work: &mut bool,
        want_close: &mut bool,
    ) -> AqResult<()> {
        use futures_util::stream::Stream;

        let mut this = self.project();

        if *this.ep_cmd_recv_closed {
            *want_close = true;
            return Ok(());
        }

        let mut udp_send = None;

        for _ in 0..32 {
            if udp_send.is_none() && !*this.udp_cmd_send_closed {
                match this.udp_cmd_send.poll_acquire(cx) {
                    Poll::Pending => break,
                    Poll::Ready(Err(_)) => {
                        *this.udp_cmd_send_closed = true;
                    }
                    Poll::Ready(Ok(sender)) => {
                        udp_send = Some(sender);
                    }
                }
            }

            match this.ep_cmd_recv.as_mut().poll_next(cx) {
                Poll::Pending => break,
                Poll::Ready(None) => {
                    *this.ep_cmd_recv_closed = true;
                    *want_close = true;
                    return Ok(());
                }
                Poll::Ready(Some(evt)) => {
                    *more_work = true;
                    match evt {
                        EpCmd::EpEvt { hnd, evt, rsp } => {
                            let res = endpoint.handle_event(hnd, evt);
                            rsp.send(res);
                        }
                        EpCmd::EpCmd(EndpointCmd::GetLocalAddress(
                            sender,
                            _,
                        )) => {
                            if let Some(udp_sender) = udp_send.take() {
                                udp_sender.send(
                                    UdpBackendCmd::GetLocalAddress(sender),
                                );
                            } else {
                                sender.send(Err("SocketDisconnected".into()));
                            }
                        }
                        EpCmd::EpCmd(EndpointCmd::Connect {
                            sender,
                            addr,
                            server_name,
                            ..
                        }) => {
                            match endpoint.connect(
                                this.client_config.as_ref().clone(),
                                addr,
                                &server_name,
                            ) {
                                Err(err) => {
                                    sender.send(Err(one_err::OneErr::new(err)))
                                }
                                Ok((hnd, con)) => {
                                    let (con, con_cmd_send, con_recv) =
                                        <ConnectionDriver<Runtime>>::spawn(
                                            hnd,
                                            con,
                                            this.ep_cmd_send.as_inner().clone(),
                                            this.udp_packet_send
                                                .as_inner()
                                                .clone(),
                                        );
                                    connections.insert(
                                        hnd,
                                        MultiSenderPoll::new(con_cmd_send),
                                    );
                                    sender.send(Ok((con, con_recv)));
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(())
    }
}
