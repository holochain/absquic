use super::*;

impl QuinnDriver {
    pub fn poll_connections(
        &mut self,
        cx: &mut Context<'_>,
        now: std::time::Instant,
    ) -> AqResult<Disposition> {
        let mut next_timeout = None;

        let con_count = self.connections.len();

        let mut did_work = false;

        let mut remove_connections = Vec::new();

        'con: for (hnd, info) in self.connections.iter_mut() {
            tracing::trace!(?hnd, "poll_connections iter");

            // no-op if not needed, just always triggering for now
            info.connection.handle_timeout(now);

            // poll cmd_recv
            loop {
                match info.cmd_recv.poll_recv(cx) {
                    Poll::Pending => break,
                    Poll::Ready(None) => {
                        remove_connections.push(*hnd);
                        continue 'con;
                    }
                    Poll::Ready(Some(cmd)) => {
                        did_work = true;

                        use ConnectionCmd::*;
                        match cmd {
                            GetRemoteAddress(sender) => {
                                sender
                                    .send(Ok(info.connection.remote_address()));
                            }
                            OpenUniStream(sender) => {
                                if let Some(stream_id) = info
                                    .connection
                                    .streams()
                                    .open(quinn_proto::Dir::Uni)
                                {
                                    sender.send(Ok(
                                        info.intake_uni_out(stream_id)
                                    ));
                                } else {
                                    if info.uni_out_buf.is_some() {
                                        sender.send(Err("pending outgoing uni stream already registered, only call open_uni_stream from a single clone of the connection".into()));
                                    } else {
                                        info.uni_out_buf = Some(sender);
                                    }
                                }
                            }
                            OpenBiStream(sender) => {
                                if let Some(stream_id) = info
                                    .connection
                                    .streams()
                                    .open(quinn_proto::Dir::Bi)
                                {
                                    sender.send(Ok(info.intake_bi(stream_id)));
                                } else {
                                    if info.bi_buf.is_some() {
                                        sender.send(Err("pending outgoing bi stream already registered, only call open_bi_stream from a single clone of the connection".into()));
                                    } else {
                                        info.bi_buf = Some(sender);
                                    }
                                }
                            }
                        }
                    }
                }
            }

            // poll_transmit
            if self.udp_send_buf.len() < BUF_CAP {
                // careful looping over this, because a single
                // connection could starve others...
                let loop_count = std::cmp::max(
                    1,
                    (BUF_CAP - self.udp_send_buf.len())
                        / std::cmp::max(1, con_count),
                );
                for _ in 0..loop_count {
                    let max_gso = self.udp_send.max_gso_segments();
                    if let Some(transmit) =
                        info.connection.poll_transmit(now, max_gso)
                    {
                        did_work = true;

                        self.udp_send_buf.push_back(OutUdpPacket {
                            dst_addr: transmit.destination,
                            src_ip: transmit.src_ip,
                            segment_size: transmit.segment_size,
                            ecn: transmit.ecn.map(|ecn| ecn as u8),
                            data: transmit.contents,
                        });
                    } else {
                        break;
                    }
                }
            }

            // poll_timeout
            if let Some(to) = info.connection.poll_timeout() {
                if let Some(next_timeout) = &mut next_timeout {
                    if to < *next_timeout {
                        *next_timeout = to;
                    }
                } else {
                    next_timeout = Some(to);
                }
            }

            // poll_endpoint_events
            while let Some(evt) = info.connection.poll_endpoint_events() {
                did_work = true;

                if let Some(evt) = self.endpoint.handle_event(*hnd, evt) {
                    info.connection.handle_event(evt);
                }
            }

            // poll
            loop {
                while !info.evt_send_buf.is_empty() {
                    match info.evt_send.poll_send(cx) {
                        Poll::Pending => break,
                        Poll::Ready(Err(_)) => {
                            remove_connections.push(*hnd);
                            continue 'con;
                        }
                        Poll::Ready(Ok(permit)) => {
                            did_work = true;

                            permit(info.evt_send_buf.pop_front().unwrap());
                        }
                    }
                }

                if info.evt_send_buf.len() >= BUF_CAP {
                    break;
                }

                if let Some(evt) = info.connection.poll() {
                    did_work = true;

                    use quinn_proto::Event::*;
                    match evt {
                        HandshakeDataReady => {
                            info.evt_send_buf
                                .push_back(ConnectionEvt::HandshakeDataReady);
                        }
                        Connected => {
                            info.evt_send_buf
                                .push_back(ConnectionEvt::Connected);
                        }
                        ConnectionLost { reason } => {
                            info.evt_send_buf.push_back(ConnectionEvt::Error(
                                one_err::OneErr::new(reason),
                            ));
                        }
                        Stream(quinn_proto::StreamEvent::Opened {
                            dir: quinn_proto::Dir::Uni,
                        }) => {
                            if let Some(stream_id) = info
                                .connection
                                .streams()
                                .accept(quinn_proto::Dir::Uni)
                            {
                                let rf = info.intake_uni_in(stream_id);
                                info.evt_send_buf
                                    .push_back(ConnectionEvt::InUniStream(rf));
                            }
                        }
                        Stream(quinn_proto::StreamEvent::Opened {
                            dir: quinn_proto::Dir::Bi,
                        }) => {
                            if let Some(stream_id) = info
                                .connection
                                .streams()
                                .accept(quinn_proto::Dir::Bi)
                            {
                                let (wf, rf) = info.intake_bi(stream_id);
                                info.evt_send_buf.push_back(
                                    ConnectionEvt::InBiStream(wf, rf),
                                );
                            }
                        }
                        Stream(quinn_proto::StreamEvent::Available {
                            dir: quinn_proto::Dir::Uni,
                        }) => {
                            if let Some(sender) = info.uni_out_buf.take() {
                                if let Some(stream_id) = info
                                    .connection
                                    .streams()
                                    .open(quinn_proto::Dir::Uni)
                                {
                                    let wf = info.intake_uni_out(stream_id);
                                    sender.send(Ok(wf));
                                } else {
                                    sender.send(Err(
                                        "failed to open uni stream".into(),
                                    ));
                                }
                            }
                        }
                        Stream(quinn_proto::StreamEvent::Available {
                            dir: quinn_proto::Dir::Bi,
                        }) => {
                            if let Some(sender) = info.bi_buf.take() {
                                if let Some(stream_id) = info
                                    .connection
                                    .streams()
                                    .open(quinn_proto::Dir::Bi)
                                {
                                    let r = info.intake_bi(stream_id);
                                    sender.send(Ok(r));
                                } else {
                                    sender.send(Err(
                                        "failed to open bi stream".into(),
                                    ));
                                }
                            }
                        }
                        Stream(evt) => panic!("unhandled event: {:?}", evt),
                        DatagramReceived => {
                            if let Some(dg) = info.connection.datagrams().recv()
                            {
                                info.evt_send_buf
                                    .push_back(ConnectionEvt::InDatagram(dg));
                            }
                        }
                    }
                } else {
                    break;
                }
            }

            for (stream_id, stream_info) in info.streams.iter_mut() {
                let (wb, rb) = match stream_info {
                    StreamInfo::UniOut(wb) => (Some(wb), None),
                    StreamInfo::UniIn(rb) => (None, Some(rb)),
                    StreamInfo::Bi(wb, rb) => (Some(wb), Some(rb)),
                };

                if let Some(rb) = rb {
                    let mut recv_stream =
                        info.connection.recv_stream(*stream_id);
                    let mut chunks = match recv_stream.read(true) {
                        // TODO - close the stream instead
                        Err(e) => return Err(format!("{:?}", e).into()),
                        Ok(chunks) => chunks,
                    };
                    loop {
                        let (read_max, read_cb) = match rb.poll_request_push(cx)
                        {
                            Poll::Pending => break,
                            Poll::Ready(Err(_)) => break,
                            Poll::Ready(Ok(r)) => r,
                        };
                        match chunks.next(read_max) {
                            Err(quinn_proto::ReadError::Blocked) => break,
                            Err(_) => break,
                            Ok(None) => {
                                // TODO close the stream
                                panic!("arrarg");
                            }
                            Ok(Some(chunk)) => {
                                did_work = true;

                                if chunk.bytes.len() > read_max {
                                    panic!("unexpected large chunk");
                                }

                                read_cb(chunk.bytes);
                            }
                        }
                    }

                    if chunks.finalize().should_transmit() {
                        did_work = true;
                    }
                }

                if let Some(wb) = wb {
                    let mut send_stream =
                        info.connection.send_stream(*stream_id);
                    loop {
                        match wb.poll_incoming(cx, |b| {
                            let res = send_stream
                                .write(b.as_ref())
                                .map_err(|e| one_err::OneErr::new(e))?;
                            use absquic_core::deps::bytes::Buf;
                            b.advance(res);
                            Ok(())
                        }) {
                            Poll::Pending => break,
                            // TODO - close the stream
                            Poll::Ready(Err(_)) => break,
                            Poll::Ready(Ok(cmd)) => {
                                did_work = true;
                                match cmd {
                                    WriteCmd::Data => (),
                                    WriteCmd::Reset(error_code) => {
                                        let _ = send_stream.reset(
                                            quinn_proto::VarInt::from_u64(
                                                error_code,
                                            )
                                            .map_err(one_err::OneErr::new)?,
                                        );
                                        // TODO - close the stream
                                        break;
                                    }
                                    WriteCmd::Finish => {
                                        let _ = send_stream.finish();
                                        // TODO - close the stream
                                        break;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        for hnd in remove_connections {
            did_work = true;

            if let Some(mut info) = self.connections.remove(&hnd) {
                info.connection.close(
                    now,
                    quinn_proto::VarInt::from_u32(0),
                    absquic_core::deps::bytes::Bytes::new(),
                );
            }
        }

        if let Some(next_timeout) = next_timeout {
            if next_timeout > now {
                if self.next_timeout <= now || next_timeout < self.next_timeout
                {
                    self.next_timeout = next_timeout;
                    let waker = self.waker.clone();
                    let logic: Box<dyn FnOnce() + 'static + Send> =
                        Box::new(move || {
                            waker.wake();
                        });
                    self.timeouts_scheduler.schedule(logic, next_timeout);
                }
            }
        }

        if did_work {
            tracing::trace!("poll_connections more work");
            Ok(Disposition::MoreWork)
        } else {
            tracing::trace!("poll_connections pendok");
            Ok(Disposition::PendOk)
        }
    }
}
