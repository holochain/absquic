#![allow(clippy::too_many_arguments)]
use super::*;

pin_project_lite::pin_project! {
    pub struct ConPollDriver<Runtime: AsyncRuntime> {
        con_evt_send: MultiSenderPoll<ConnectionEvt>,
        con_evt_send_closed: bool,
        _p: std::marker::PhantomData<Runtime>,
    }
}

impl<Runtime: AsyncRuntime> ConPollDriver<Runtime> {
    pub fn new(con_evt_send: MultiSender<ConnectionEvt>) -> Self {
        Self {
            con_evt_send: MultiSenderPoll::new(con_evt_send),
            con_evt_send_closed: false,
            _p: std::marker::PhantomData,
        }
    }

    pub fn poll(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        inbound_datagrams: &mut bool,
        inbound_uni: &mut bool,
        inbound_bi: &mut bool,
        outbound_uni: &mut bool,
        outbound_bi: &mut bool,
        streams: &mut StreamMap,
        connection: &mut quinn_proto::Connection,
        more_work: &mut bool,
        want_close: &mut bool,
    ) -> AqResult<()> {
        let this = self.project();

        if *this.con_evt_send_closed {
            *want_close = true;
        }

        let mut send_permit = None;

        // returns true if we should break out of the loop
        // otherwise, the event channel is closed and we can
        // let the events drop
        let mut check_send_permit =
            |send_permit: &mut Option<OnceSender<ConnectionEvt>>| {
                if send_permit.is_none() && !*this.con_evt_send_closed {
                    match this.con_evt_send.poll_acquire(cx) {
                        Poll::Pending => (),
                        Poll::Ready(Err(_)) => {
                            *this.con_evt_send_closed = true;
                        }
                        Poll::Ready(Ok(sender)) => {
                            *send_permit = Some(sender);
                        }
                    }
                }

                #[allow(clippy::needless_bool)] // so we can comment...
                if *this.con_evt_send_closed {
                    // continue to process events - letting them drop
                    false
                } else if send_permit.is_none() {
                    // the channel is NOT closed, we got a pending,
                    // we need to break out of the loop
                    //
                    true
                } else {
                    false
                }
            };

        for _ in 0..32 {
            if check_send_permit(&mut send_permit) {
                break;
            }

            if let Some(evt) = connection.poll() {
                *more_work = true;

                Self::handle_event(
                    streams,
                    inbound_datagrams,
                    inbound_uni,
                    inbound_bi,
                    outbound_uni,
                    outbound_bi,
                    &mut send_permit,
                    evt,
                )?;
            } else {
                break;
            }
        }

        for _ in 0..32 {
            if !*inbound_datagrams {
                break;
            }

            if check_send_permit(&mut send_permit) {
                break;
            }

            if let Some(bytes) = connection.datagrams().recv() {
                *more_work = true;
                if let Some(send_permit) = send_permit.take() {
                    send_permit.send(ConnectionEvt::InDatagram(bytes));
                }
            } else {
                *inbound_datagrams = false;
                break;
            }
        }

        for _ in 0..32 {
            if !*inbound_uni {
                break;
            }

            if check_send_permit(&mut send_permit) {
                break;
            }

            if let Some(stream_id) =
                connection.streams().accept(quinn_proto::Dir::Uni)
            {
                *more_work = true;
                let (rb, rf) = read_stream_pair::<Runtime>(BYTES_CAP);
                let info = StreamInfo::uni_in(stream_id, rb);
                streams.insert(stream_id, info);
                if let Some(send_permit) = send_permit.take() {
                    send_permit.send(ConnectionEvt::InUniStream(rf));
                }
            } else {
                *inbound_uni = false;
                break;
            }
        }

        for _ in 0..32 {
            if !*inbound_bi {
                break;
            }

            if check_send_permit(&mut send_permit) {
                break;
            }

            if let Some(stream_id) =
                connection.streams().accept(quinn_proto::Dir::Bi)
            {
                *more_work = true;
                let (wb, wf) = write_stream_pair::<Runtime>(BYTES_CAP);
                let (rb, rf) = read_stream_pair::<Runtime>(BYTES_CAP);
                let info = StreamInfo::bi(stream_id, rb, wb);
                streams.insert(stream_id, info);
                if let Some(send_permit) = send_permit.take() {
                    send_permit.send(ConnectionEvt::InBiStream(wf, rf));
                }
            } else {
                *inbound_bi = false;
                break;
            }
        }

        Ok(())
    }

    fn handle_event(
        streams: &mut StreamMap,
        inbound_datagrams: &mut bool,
        inbound_uni: &mut bool,
        inbound_bi: &mut bool,
        outbound_uni: &mut bool,
        outbound_bi: &mut bool,
        send_permit: &mut Option<OnceSender<ConnectionEvt>>,
        evt: quinn_proto::Event,
    ) -> AqResult<()> {
        tracing::trace!(?evt);

        use quinn_proto::StreamEvent;
        let mut try_send = |evt| {
            if let Some(send_permit) = send_permit.take() {
                send_permit.send(evt);
            }
        };
        match evt {
            quinn_proto::Event::HandshakeDataReady => {
                try_send(ConnectionEvt::HandshakeDataReady);
            }
            quinn_proto::Event::Connected => {
                try_send(ConnectionEvt::Connected);
            }
            quinn_proto::Event::ConnectionLost { reason } => {
                try_send(ConnectionEvt::Error(one_err::OneErr::new(reason)));
            }
            quinn_proto::Event::DatagramReceived => {
                *inbound_datagrams = true;
            }
            quinn_proto::Event::Stream(StreamEvent::Opened { dir }) => {
                match dir {
                    quinn_proto::Dir::Uni => *inbound_uni = true,
                    quinn_proto::Dir::Bi => *inbound_bi = true,
                }
            }
            quinn_proto::Event::Stream(StreamEvent::Readable { id }) => {
                if let Some(info) = streams.get_mut(&id) {
                    info.set_readable();
                }
            }
            quinn_proto::Event::Stream(StreamEvent::Writable { id }) => {
                if let Some(info) = streams.get_mut(&id) {
                    info.set_writable();
                }
            }
            quinn_proto::Event::Stream(StreamEvent::Finished { id }) => {
                if let Some(info) = streams.remove(&id) {
                    info.finish();
                }
            }
            quinn_proto::Event::Stream(StreamEvent::Stopped {
                id,
                error_code,
            }) => {
                if let Some(info) = streams.remove(&id) {
                    info.stop(error_code);
                }
            }
            quinn_proto::Event::Stream(StreamEvent::Available { dir }) => {
                match dir {
                    quinn_proto::Dir::Uni => *outbound_uni = true,
                    quinn_proto::Dir::Bi => *outbound_bi = true,
                }
            }
        }

        Ok(())
    }
}
