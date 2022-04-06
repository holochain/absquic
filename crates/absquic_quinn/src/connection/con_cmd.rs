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

        for _ in 0..32 {
            match this.con_cmd_recv.as_mut().poll_next(cx) {
                Poll::Pending => break,
                Poll::Ready(None) => {
                    *this.con_cmd_recv_closed = true;
                    *want_close = true;
                    return Ok(());
                }
                Poll::Ready(Some(evt)) => {
                    *more_work = true;
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
                            this.uni_queue.push_back((rsp, guard));
                        }
                        ConCmd::ConCmd(ConnectionCmd::OpenBiStream(
                            rsp,
                            guard,
                        )) => {
                            this.bi_queue.push_back((rsp, guard));
                        }
                    }
                }
            }
        }

        while !this.uni_queue.is_empty() {
            if !*outbound_uni {
                break;
            }

            if let Some(stream_id) =
                connection.streams().open(quinn_proto::Dir::Uni)
            {
                *more_work = true;
                let (wb, wf) = write_stream_pair::<Runtime>(BYTES_CAP);
                let info = StreamInfo {
                    stream_id,
                    read: None,
                    readable: false,
                    write: Some(wb),
                    writable: true,
                };
                streams.insert(stream_id, info);
                let (rsp, _) = this.uni_queue.pop_front().unwrap();
                rsp.send(Ok(wf));
            } else {
                *outbound_uni = false;
                break;
            }
        }

        while !this.bi_queue.is_empty() {
            if !*outbound_bi {
                break;
            }

            if let Some(stream_id) =
                connection.streams().open(quinn_proto::Dir::Bi)
            {
                *more_work = true;
                let (wb, wf) = write_stream_pair::<Runtime>(BYTES_CAP);
                let (rb, rf) = read_stream_pair::<Runtime>(BYTES_CAP);
                let info = StreamInfo {
                    stream_id,
                    read: Some(rb),
                    readable: true,
                    write: Some(wb),
                    writable: true,
                };
                streams.insert(stream_id, info);
                let (rsp, _) = this.bi_queue.pop_front().unwrap();
                rsp.send(Ok((wf, rf)));
            } else {
                *outbound_bi = false;
                break;
            }
        }

        Ok(())
    }
}
