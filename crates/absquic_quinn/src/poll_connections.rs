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

        for (hnd, info) in self.connections.iter_mut() {
            if info.connection.is_drained() && info.evt_send_buf.is_empty() {
                remove_connections.push(*hnd);
                continue;
            }

            // no-op if not needed, just always triggering for now
            info.connection.handle_timeout(now);

            // poll cmd_recv
            loop {
                match info.cmd_recv.poll_recv(cx) {
                    Poll::Pending => break,
                    Poll::Ready(None) => {
                        info.want_close = true;
                        break;
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
                                if info.want_close {
                                    sender
                                        .send(Err("ConnectionClosing".into()));
                                    continue;
                                }

                                if let Some(stream_id) = info
                                    .connection
                                    .streams()
                                    .open(quinn_proto::Dir::Uni)
                                {
                                    sender.send(Ok(
                                        info.intake_uni_out(stream_id)
                                    ));
                                } else if info.uni_out_buf.is_some() {
                                    sender.send(Err("pending outgoing uni stream already registered, only call open_uni_stream from a single clone of the connection".into()));
                                } else {
                                    info.uni_out_buf = Some(sender);
                                }
                            }
                            OpenBiStream(sender) => {
                                if info.want_close {
                                    sender
                                        .send(Err("ConnectionClosing".into()));
                                    continue;
                                }

                                if let Some(stream_id) = info
                                    .connection
                                    .streams()
                                    .open(quinn_proto::Dir::Bi)
                                {
                                    sender.send(Ok(info.intake_bi(stream_id)));
                                } else if info.bi_buf.is_some() {
                                    sender.send(Err("pending outgoing bi stream already registered, only call open_bi_stream from a single clone of the connection".into()));
                                } else {
                                    info.bi_buf = Some(sender);
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
                let mut did_con_poll_work = false;

                while !info.evt_send_buf.is_empty() {
                    match info.evt_send.poll_send(cx) {
                        Poll::Pending => break,
                        Poll::Ready(Err(_)) => {
                            info.want_close = true;
                            break;
                        }
                        Poll::Ready(Ok(permit)) => {
                            did_work = true;
                            did_con_poll_work = true;

                            permit(info.evt_send_buf.pop_front().unwrap());
                        }
                    }
                }

                while info.evt_send_buf.len() < BUF_CAP {
                    if let Some(evt) = info.connection.poll() {
                        did_work = true;
                        did_con_poll_work = true;

                        use quinn_proto::Event::*;
                        match evt {
                            HandshakeDataReady => {
                                info.evt_send_buf.push_back(
                                    ConnectionEvt::HandshakeDataReady,
                                );
                            }
                            Connected => {
                                info.evt_send_buf
                                    .push_back(ConnectionEvt::Connected);
                            }
                            ConnectionLost { reason } => {
                                info.want_close = true;
                                info.evt_send_buf.push_back(
                                    ConnectionEvt::Error(one_err::OneErr::new(
                                        reason,
                                    )),
                                );
                            }
                            Stream(quinn_proto::StreamEvent::Opened {
                                dir: quinn_proto::Dir::Uni,
                            }) => {
                                // currently checking every time we loop
                            }
                            Stream(quinn_proto::StreamEvent::Opened {
                                dir: quinn_proto::Dir::Bi,
                            }) => {
                                // currently checking every time we loop
                            }
                            Stream(quinn_proto::StreamEvent::Readable {
                                ..
                            }) => {
                                // currently checking every time we loop
                            }
                            Stream(quinn_proto::StreamEvent::Writable {
                                ..
                            }) => {
                                // currently checking every time we loop
                            }
                            Stream(quinn_proto::StreamEvent::Finished {
                                ..
                            }) => {
                                // check this on poll errors?
                                /*
                                if let Some(stream_info) = info.streams.get_mut(&id)
                                {
                                    stream_info.rm_both();
                                }
                                */
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
                                if let Some(dg) =
                                    info.connection.datagrams().recv()
                                {
                                    info.evt_send_buf.push_back(
                                        ConnectionEvt::InDatagram(dg),
                                    );
                                }
                            }
                        }
                    } else {
                        break;
                    }
                }

                while info.evt_send_buf.len() < BUF_CAP {
                    if let Some(stream_id) =
                        info.connection.streams().accept(quinn_proto::Dir::Uni)
                    {
                        did_work = true;
                        did_con_poll_work = true;

                        tracing::debug!(?stream_id, "in read uni stream");
                        let rf = info.intake_uni_in(stream_id);
                        info.evt_send_buf
                            .push_back(ConnectionEvt::InUniStream(rf));
                    } else {
                        break;
                    }
                }

                while info.evt_send_buf.len() < BUF_CAP {
                    if let Some(stream_id) =
                        info.connection.streams().accept(quinn_proto::Dir::Bi)
                    {
                        did_work = true;
                        did_con_poll_work = true;

                        let (wf, rf) = info.intake_bi(stream_id);
                        info.evt_send_buf
                            .push_back(ConnectionEvt::InBiStream(wf, rf));
                    } else {
                        break;
                    }
                }

                if !did_con_poll_work {
                    break;
                }
            }

            let mut remove_streams = Vec::new();

            for (stream_id, stream_info) in info.streams.iter_mut() {
                let mut remove = false;
                let mut stop_err = None;

                if let Some(rb) = stream_info.get_read() {
                    let mut stop_code = None;

                    let mut recv_stream =
                        info.connection.recv_stream(*stream_id);
                    match recv_stream.read(true) {
                        Err(_) => {
                            did_work = true;
                            remove = true;
                        }
                        Ok(mut chunks) => {
                            loop {
                                let permit = match rb.poll_send(cx) {
                                    Poll::Pending => break,
                                    Poll::Ready(p) => p,
                                };
                                match chunks.next(permit.max_len()) {
                                    Err(quinn_proto::ReadError::Blocked) => {
                                        break
                                    }
                                    Err(err) => {
                                        stop_err =
                                            Some(one_err::OneErr::new(err));
                                        did_work = true;
                                        remove = true;
                                        break;
                                    }
                                    Ok(None) => {
                                        did_work = true;
                                        remove = true;
                                        break;
                                    }
                                    Ok(Some(chunk)) => {
                                        did_work = true;

                                        if chunk.bytes.len() > permit.max_len()
                                        {
                                            panic!("unexpected large chunk");
                                        }

                                        if let Err(code) =
                                            permit.send(chunk.bytes)
                                        {
                                            remove = true;
                                            stop_code = Some(code);
                                            break;
                                        }
                                    }
                                }
                            }

                            if chunks.finalize().should_transmit() {
                                did_work = true;
                            }
                        }
                    };

                    if let Some(code) = stop_code {
                        remove = true;
                        if let Ok(code) = quinn_proto::VarInt::from_u64(code) {
                            let _ = recv_stream.stop(code);
                        }
                    }
                }

                if remove {
                    tracing::debug!(?stream_id, "remove read stream");
                    stream_info.rm_read(stop_err)
                }

                remove = false;

                if let Some(wb) = stream_info.get_write() {
                    let mut send_stream =
                        info.connection.send_stream(*stream_id);
                    loop {
                        match wb.poll_recv(cx) {
                            Poll::Pending => break,
                            Poll::Ready(cmd) => match cmd {
                                WriteCmd::Data(mut cmd) => {
                                    did_work = true;
                                    match send_stream.write(cmd.as_ref()) {
                                        Ok(n) => {
                                            use bytes::Buf;
                                            cmd.advance(n);
                                        }
                                        Err(err) => {
                                            // TODO a way to propagate this
                                            // error up to the implementor
                                            tracing::error!(?err);
                                            remove = true;
                                            break;
                                        }
                                    }
                                }
                                WriteCmd::Stop(error_code) => {
                                    did_work = true;
                                    remove = true;
                                    let code = if let Ok(code) =
                                        quinn_proto::VarInt::from_u64(
                                            error_code,
                                        ) {
                                        code
                                    } else {
                                        quinn_proto::VarInt::from_u32(0)
                                    };
                                    if code.into_inner() == 0 {
                                        let _ = send_stream.finish();
                                    } else {
                                        let _ = send_stream.reset(code);
                                    }
                                    break;
                                }
                            },
                        }
                    }
                }

                if remove {
                    tracing::debug!(?stream_id, "remove write stream");
                    stream_info.rm_write();
                }

                if stream_info.is_closed() {
                    remove_streams.push(*stream_id);
                }
            }

            for stream_id in remove_streams {
                did_work = true;

                if let Some(stream_info) = info.streams.remove(&stream_id) {
                    tracing::debug!(?stream_id, "stream closed");
                    drop(stream_info);
                }
            }

            if info.want_close
                && info.streams.is_empty()
                && !info.connection.is_closed()
            {
                tracing::debug!(?hnd, "closing connection");
                info.connection.close(
                    now,
                    quinn_proto::VarInt::from_u32(0),
                    bytes::Bytes::new(),
                );
            }
        }

        for hnd in remove_connections {
            did_work = true;

            if let Some(mut info) = self.connections.remove(&hnd) {
                tracing::debug!(?hnd, "removing connection");
                if !info.connection.is_closed() {
                    info.connection.close(
                        now,
                        quinn_proto::VarInt::from_u32(0),
                        bytes::Bytes::new(),
                    );
                }
            }
        }

        if let Some(next_timeout) = next_timeout {
            if next_timeout > now
                && (self.next_timeout <= now
                    || next_timeout < self.next_timeout)
            {
                self.next_timeout = next_timeout;
                let waker = self.waker.clone();
                let logic: Box<dyn FnOnce() + 'static + Send> =
                    Box::new(move || {
                        tracing::trace!("wake by timeout logic");
                        waker.wake();
                    });
                self.timeouts_scheduler.schedule(logic, next_timeout);
            }
        }

        if did_work {
            Ok(Disposition::MoreWork)
        } else {
            Ok(Disposition::PendOk)
        }
    }
}
