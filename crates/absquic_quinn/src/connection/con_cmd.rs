#![allow(clippy::type_complexity)]
#![allow(clippy::too_many_arguments)]
use super::*;

pin_project_lite::pin_project! {
    pub struct ConCmdDriver<Runtime: AsyncRuntime> {
        #[pin]
        con_cmd_recv: futures_util::stream::SelectAll<ConCmdRecv>,
        con_cmd_recv_closed: bool,
        uni_queue: VecDeque<(
            OnceSender<AqResult<WriteStream>>,
            DynSemaphoreGuard,
        )>,
        bi_queue: VecDeque<(
            OnceSender<AqResult<(WriteStream, ReadStream)>>,
            DynSemaphoreGuard,
        )>,
        _p: std::marker::PhantomData<Runtime>,
    }
}

impl<Runtime: AsyncRuntime> ConCmdDriver<Runtime> {
    pub fn new(
        con_cmd_recv: futures_util::stream::SelectAll<ConCmdRecv>,
    ) -> Self {
        Self {
            con_cmd_recv,
            con_cmd_recv_closed: false,
            uni_queue: VecDeque::new(),
            bi_queue: VecDeque::new(),
            _p: std::marker::PhantomData,
        }
    }

    fn register_uni(
        streams: &mut StreamMap,
        stream_id: quinn_proto::StreamId,
        rsp: OnceSender<AqResult<WriteStream>>,
    ) {
        let (wb, wf) = write_stream_pair::<Runtime>(BYTES_CAP);
        let info = StreamInfo::uni_out(stream_id, wb);
        streams.insert(stream_id, info);
        rsp.send(Ok(wf));
    }

    fn register_bi(
        streams: &mut StreamMap,
        stream_id: quinn_proto::StreamId,
        rsp: OnceSender<AqResult<(WriteStream, ReadStream)>>,
    ) {
        let (wb, wf) = write_stream_pair::<Runtime>(BYTES_CAP);
        let (rb, rf) = read_stream_pair::<Runtime>(BYTES_CAP);
        let info = StreamInfo::bi(stream_id, rb, wb);
        streams.insert(stream_id, info);
        rsp.send(Ok((wf, rf)));
    }

    pub fn poll(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        streams: &mut StreamMap,
        connection: &mut quinn_proto::Connection,
        outbound_uni: &mut bool,
        outbound_bi: &mut bool,
        more_work: &mut bool,
        want_close: &mut bool,
    ) -> AqResult<()> {
        use futures_util::stream::Stream;

        let mut this = self.project();

        if *this.con_cmd_recv_closed {
            *want_close = true;
            return Ok(());
        }

        while !this.uni_queue.is_empty() && *outbound_uni {
            if let Some(stream_id) =
                connection.streams().open(quinn_proto::Dir::Uni)
            {
                let (rsp, _) = this.uni_queue.pop_front().unwrap();
                Self::register_uni(streams, stream_id, rsp);
            } else {
                *outbound_uni = false;
                break;
            }
        }

        while !this.bi_queue.is_empty() && *outbound_bi {
            if let Some(stream_id) =
                connection.streams().open(quinn_proto::Dir::Bi)
            {
                let (rsp, _) = this.bi_queue.pop_front().unwrap();
                Self::register_bi(streams, stream_id, rsp);
            } else {
                *outbound_bi = false;
                break;
            }
        }

        let mut got_pending = false;
        let start = std::time::Instant::now();
        let mut elapsed_ms = 0;
        for _ in 0..CHAN_CAP {
            match this.con_cmd_recv.as_mut().poll_next(cx) {
                Poll::Pending => {
                    got_pending = true;
                    break;
                }
                Poll::Ready(None) => {
                    *this.con_cmd_recv_closed = true;
                    *want_close = true;
                    return Ok(());
                }
                Poll::Ready(Some(evt)) => {
                    match evt {
                        ConCmd::ConEvt(evt) => {
                            connection.handle_event(evt);
                        }
                        ConCmd::ConCmd(ConnectionCmd::GetRemoteAddress(
                            rsp,
                            _,
                        )) => {
                            rsp.send(Ok(connection.remote_address()));
                        }
                        ConCmd::ConCmd(ConnectionCmd::OpenUniStream(
                            rsp,
                            guard,
                        )) => {
                            if *outbound_uni {
                                if let Some(stream_id) = connection.streams().open(quinn_proto::Dir::Uni) {
                                    Self::register_uni(streams, stream_id, rsp);
                                    continue;
                                }
                            }
                            *outbound_uni = false;
                            this.uni_queue.push_back((rsp, guard));
                        }
                        ConCmd::ConCmd(ConnectionCmd::OpenBiStream(
                            rsp,
                            guard,
                        )) => {
                            if *outbound_bi {
                                if let Some(stream_id) = connection.streams().open(quinn_proto::Dir::Bi) {
                                    Self::register_bi(streams, stream_id, rsp);
                                    continue;
                                }
                            }
                            *outbound_bi = false;
                            this.bi_queue.push_back((rsp, guard));
                        }
                    }
                }
            }

            elapsed_ms = start.elapsed().as_millis();
            if elapsed_ms >= 2 {
                break;
            }
        }

        if elapsed_ms >= 3 {
            tracing::warn!(%elapsed_ms, "long con cmd poll");
        }

        // if we got pending, there is no more work
        if !got_pending {
            // otherwise:
            *more_work = true;
        }

        Ok(())
    }
}
