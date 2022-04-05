use super::*;

const CON_BUF_LIM: usize = 128;

type NewConQueue =
    VecDeque<(quinn_proto::ConnectionHandle, quinn_proto::Connection)>;

type ConEvtMap = HashMap<
    quinn_proto::ConnectionHandle,
    VecDeque<quinn_proto::ConnectionEvent>,
>;

pin_project_lite::pin_project! {
    pub struct UdpInDriver<Runtime: AsyncRuntime> {
        ep_cmd_send: MultiSender<EpCmd>,
        udp_packet_send: MultiSender<OutUdpPacket>,
        evt_send: MultiSender<EndpointEvt>,
        evt_send_closed: bool,
        udp_packet_recv: MultiReceiver<UdpBackendEvt>,
        udp_packet_recv_closed: bool,
        new_con_buf: NewConQueue,
        con_evt_buf: ConEvtMap,
        _p: std::marker::PhantomData<Runtime>,
    }
}

impl<Runtime: AsyncRuntime> UdpInDriver<Runtime> {
    pub fn new(
        ep_cmd_send: MultiSender<EpCmd>,
        udp_packet_send: MultiSender<OutUdpPacket>,
        evt_send: MultiSender<EndpointEvt>,
        udp_packet_recv: MultiReceiver<UdpBackendEvt>,
    ) -> Self {
        Self {
            ep_cmd_send,
            udp_packet_send,
            evt_send,
            evt_send_closed: false,
            udp_packet_recv,
            udp_packet_recv_closed: false,
            new_con_buf: VecDeque::with_capacity(CON_BUF_LIM),
            con_evt_buf: HashMap::with_capacity(CON_BUF_LIM),
            _p: std::marker::PhantomData,
        }
    }

    fn try_send_new_con(
        cx: &mut Context<'_>,
        evt_send: &mut MultiSender<EndpointEvt>,
        evt_send_closed: &mut bool,
        new_con_buf: &mut NewConQueue,
    ) -> Option<OnceSender<EndpointEvt>> {
        match evt_send.poll_acquire(cx) {
            Poll::Pending => None,
            Poll::Ready(Err(_)) => {
                tracing::debug!("evt send closed");
                *evt_send_closed = true;
                new_con_buf.clear();
                None
            }
            Poll::Ready(Ok(sender)) => Some(sender),
        }
    }

    fn register_con(
        ep_cmd_send: MultiSender<EpCmd>,
        udp_packet_send: MultiSender<OutUdpPacket>,
        connections: &mut ConMap,
        hnd: quinn_proto::ConnectionHandle,
        con: quinn_proto::Connection,
    ) -> (Connection, MultiReceiver<ConnectionEvt>) {
        let (con, con_cmd_send, con_recv) = <ConnectionDriver<Runtime>>::spawn(
            hnd,
            con,
            ep_cmd_send,
            udp_packet_send,
        );
        connections.insert(hnd, con_cmd_send);
        (con, con_recv)
    }

    fn flush_new_con_buf(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        connections: &mut ConMap,
    ) {
        let this = self.project();

        if *this.evt_send_closed {
            return;
        }

        while !this.new_con_buf.is_empty() {
            if let Some(sender) = Self::try_send_new_con(
                cx,
                this.evt_send,
                this.evt_send_closed,
                this.new_con_buf,
            ) {
                let (hnd, con) = this.new_con_buf.pop_front().unwrap();
                let (con, con_recv) = Self::register_con(
                    this.ep_cmd_send.clone(),
                    this.udp_packet_send.clone(),
                    connections,
                    hnd,
                    con,
                );
                sender.send(EndpointEvt::InConnection(con, con_recv));
            } else {
                return;
            }
        }
    }

    fn flush_con_evt_buf(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        connections: &mut ConMap,
    ) {
        let this = self.project();

        let mut rm_buf = Vec::new();

        for (hnd, evt_list) in this.con_evt_buf.iter_mut() {
            let mut rm_con = false;
            if let Some(send) = connections.get_mut(&hnd) {
                while !evt_list.is_empty() {
                    match send.poll_acquire(cx) {
                        Poll::Pending => break,
                        Poll::Ready(Err(_)) => {
                            rm_con = true;
                            break;
                        }
                        Poll::Ready(Ok(sender)) => {
                            sender.send(ConCmd::ConEvt(
                                evt_list.pop_front().unwrap(),
                            ));
                        }
                    }
                }
                if evt_list.is_empty() {
                    rm_buf.push(*hnd);
                }
            } else {
                rm_buf.push(*hnd);
            }
            if rm_con {
                rm_buf.push(*hnd);
                connections.remove(&hnd);
            }
        }

        for hnd in rm_buf {
            this.con_evt_buf.remove(&hnd);
        }
    }

    fn buffer_space(
        new_con_buf: &mut NewConQueue,
        con_evt_buf: &mut ConEvtMap,
    ) -> usize {
        let mut cur = new_con_buf.len();
        for (_, v) in con_evt_buf.iter() {
            cur += v.len();
        }
        CON_BUF_LIM - cur
    }

    pub fn poll(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        endpoint: &mut quinn_proto::Endpoint,
        connections: &mut ConMap,
        more_work: &mut bool,
        want_close: &mut bool,
    ) -> AqResult<()> {
        self.as_mut().flush_new_con_buf(cx, connections);
        self.as_mut().flush_con_evt_buf(cx, connections);

        let this = self.project();

        let mut in_packet_space =
            Self::buffer_space(this.new_con_buf, this.con_evt_buf);

        let now = std::time::Instant::now();

        for _ in 0..32 {
            if *this.udp_packet_recv_closed {
                break;
            }

            if in_packet_space == 0 {
                break;
            }

            match this.udp_packet_recv.poll_recv(cx) {
                Poll::Pending => break,
                Poll::Ready(None) => {
                    tracing::debug!("udp recv closed");
                    *this.udp_packet_recv_closed = true;
                    break;
                }
                Poll::Ready(Some(evt)) => {
                    *more_work = true;
                    match evt {
                        UdpBackendEvt::InUdpPacket(packet) => {
                            if let Some((hnd, evt)) = endpoint.handle(
                                now,
                                packet.src_addr,
                                packet.dst_ip,
                                packet
                                    .ecn
                                    .map(|ecn| {
                                        quinn_proto::EcnCodepoint::from_bits(
                                            ecn,
                                        )
                                    })
                                    .flatten(),
                                packet.data,
                            ) {
                                match evt {
                                    quinn_proto::DatagramEvent::ConnectionEvent(evt) => {
                                        let mut rm_con = false;
                                        if let Some(send) = connections.get_mut(&hnd) {
                                            match send.poll_acquire(cx) {
                                                Poll::Pending => {
                                                    // can't send it now
                                                    // queue it in buffer
                                                    this.con_evt_buf
                                                        .entry(hnd)
                                                        .or_default()
                                                        .push_back(evt);
                                                    in_packet_space -= 1;
                                                }
                                                Poll::Ready(Err(_)) => {
                                                    rm_con = true;
                                                }
                                                Poll::Ready(Ok(sender)) => {
                                                    sender.send(ConCmd::ConEvt(
                                                        evt
                                                    ));
                                                }
                                            }
                                        } else {
                                            // just drop this event
                                            // there is no connection
                                            // to send it to
                                        }
                                        if rm_con {
                                            connections.remove(&hnd);
                                        }
                                    }
                                    quinn_proto::DatagramEvent::NewConnection(con) => {
                                        if let Some(sender) = Self::try_send_new_con(
                                            cx,
                                            this.evt_send,
                                            this.evt_send_closed,
                                            this.new_con_buf,
                                        ) {
                                            let (con, con_recv) = Self::register_con(
                                                this.ep_cmd_send.clone(),
                                                this.udp_packet_send.clone(),
                                                connections,
                                                hnd,
                                                con,
                                            );
                                            sender.send(EndpointEvt::InConnection(con, con_recv));
                                        } else {
                                            this.new_con_buf.push_back((hnd, con));
                                            in_packet_space -= 1;
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        if *this.evt_send_closed && *this.udp_packet_recv_closed {
            *want_close = true;
        }

        Ok(())
    }
}
