//! absquic_core stream types

use crate::util::*;
use crate::AqResult;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic;
use std::sync::Arc;
use std::task::Context;
use std::task::Poll;

/// types only relevant when implementing a quic state machine backend
pub mod backend {
    use super::*;

    struct CounterInner {
        max: usize,
        counter: atomic::AtomicUsize,
        waker: parking_lot::Mutex<Option<std::task::Waker>>,
    }

    impl CounterInner {
        fn wake(&self) {
            if let Some(waker) = self.waker.lock().take() {
                waker.wake();
            }
        }
    }

    pub(crate) struct Counter(Arc<CounterInner>);

    impl Counter {
        fn new(max: usize) -> Self {
            Self(Arc::new(CounterInner {
                max,
                counter: atomic::AtomicUsize::new(0),
                waker: parking_lot::Mutex::new(None),
            }))
        }

        pub(crate) fn reserve(
            &self,
            cx: &mut Context<'_>,
        ) -> Option<CounterAddPermit> {
            let cur = self.0.counter.swap(self.0.max, atomic::Ordering::AcqRel);
            if cur >= self.0.max {
                *self.0.waker.lock() = Some(cx.waker().clone());
                None
            } else {
                let amount = self.0.max - cur;
                Some(CounterAddPermit(self.0.clone(), amount))
            }
        }
    }

    pub(crate) struct CounterAddPermit(Arc<CounterInner>, usize);

    impl Drop for CounterAddPermit {
        fn drop(&mut self) {
            self.0.counter.fetch_sub(self.1, atomic::Ordering::AcqRel);
            self.0.wake();
        }
    }

    impl CounterAddPermit {
        pub(crate) fn len(&self) -> usize {
            self.1
        }

        pub(crate) fn downgrade_to(&mut self, new_amount: usize) {
            if new_amount == self.1 {
                return;
            }

            if new_amount < 1 || new_amount > self.1 {
                panic!("invalid target amount");
            }

            let sub = self.1 - new_amount;
            self.1 = new_amount;
            self.0.counter.fetch_sub(sub, atomic::Ordering::AcqRel);
            self.0.wake();
        }
    }

    /// the max length of a bytes authorized for a ReadStreamBackend push
    pub type ReadMaxSize = usize;

    /// a callback allowing data to be pushed into a ReadStreamBackend
    pub type ReadCb = Box<dyn FnOnce(bytes::Bytes) + 'static + Send>;

    /// the backend of a read stream, allows publish data to the api user
    pub struct ReadStreamBackend {
        counter: Counter,
        send: tokio::sync::mpsc::UnboundedSender<(
            bytes::Bytes,
            CounterAddPermit,
        )>,
    }

    impl ReadStreamBackend {
        /// request to push data into the read stream
        pub fn poll_request_push(
            &mut self,
            cx: &mut Context<'_>,
        ) -> Poll<AqResult<(ReadMaxSize, ReadCb)>> {
            if let Some(mut permit) = self.counter.reserve(cx) {
                let sender = self.send.clone();
                let max_size = permit.len();
                let read_cb: ReadCb = Box::new(move |mut b| {
                    if b.len() > max_size {
                        b = b.split_to(max_size);
                    } else if b.len() < max_size {
                        permit.downgrade_to(b.len());
                    }
                    // TODO - doesn't really make sense to error here
                    // but we do need to let the poller know about closed stream
                    let _ = sender.send((b, permit));
                });
                Poll::Ready(Ok((max_size, read_cb)))
            } else {
                Poll::Pending
            }
        }
    }

    /// construct a new read stream backend and frontend pair
    pub fn read_stream_pair(
        buf_size: usize,
    ) -> (ReadStreamBackend, ReadStream) {
        let counter = Counter::new(buf_size);
        // unbounded ok, because we're using Counter to set a bound
        // on the outstanding byte count sent over the channel
        let (send, recv) = tokio::sync::mpsc::unbounded_channel();
        (
            ReadStreamBackend { counter, send },
            ReadStream { recv, buf: None },
        )
    }

    pub(crate) enum WriteCmdInner {
        /// data sent over the write channel
        Data(bytes::Bytes, CounterAddPermit),

        /// reset this write channel
        Reset(u64, OneShotSender<()>),

        /// signal shutdown on this write channel
        Finish(OneShotSender<()>),
    }

    /// incoming command from the frontend write stream
    pub enum WriteCmd {
        /// data sent over the write channel
        Data,

        /// reset this write channel
        Reset(u64),

        /// signal shutdown on this write channel
        Finish,
    }

    /// the backend of a write stream, allows collecting the written data
    pub struct WriteStreamBackend {
        recv: tokio::sync::mpsc::UnboundedReceiver<WriteCmdInner>,
        buf: Option<WriteCmdInner>,
    }

    impl WriteStreamBackend {
        /// poll for incoming data on the write stream
        pub fn poll_incoming<F>(
            &mut self,
            cx: &mut Context<'_>,
            f: F,
        ) -> Poll<AqResult<WriteCmd>>
        where
            F: FnOnce(&mut bytes::Bytes) -> AqResult<()>,
        {
            if self.buf.is_none() {
                match self.recv.poll_recv(cx) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(None) => {
                        return Poll::Ready(Err("ChannelClosed".into()));
                    }
                    Poll::Ready(Some(cmd)) => self.buf = Some(cmd),
                }
            }
            match self.buf.take().unwrap() {
                WriteCmdInner::Reset(error_code, send) => {
                    send.send(Ok(()));
                    Poll::Ready(Ok(WriteCmd::Reset(error_code)))
                }
                WriteCmdInner::Finish(send) => {
                    send.send(Ok(()));
                    Poll::Ready(Ok(WriteCmd::Finish))
                }
                WriteCmdInner::Data(mut b, mut p) => {
                    if b.len() > 0 {
                        f(&mut b)?;
                        if b.len() > 0 {
                            p.downgrade_to(b.len());
                            self.buf = Some(WriteCmdInner::Data(b, p));
                        }
                        Poll::Ready(Ok(WriteCmd::Data))
                    } else {
                        // tail recurse
                        self.poll_incoming(cx, f)
                    }
                }
            }
        }
    }

    /// construct a new write stream backend and frontend pair
    pub fn write_stream_pair(
        buf_size: usize,
    ) -> (WriteStreamBackend, WriteStream) {
        let counter = Counter::new(buf_size);
        // unbounded ok, because we're using Counter to set a bound
        // on the outstanding byte count sent over the channel
        let (send, recv) = tokio::sync::mpsc::unbounded_channel();
        (
            WriteStreamBackend { recv, buf: None },
            WriteStream { counter, send },
        )
    }

    /*
    pub(crate) struct WriteStreamInner {
        pub(crate) closed: bool,
        pub(crate) gone: bool,
        pub(crate) write_waker: Option<Waker>,
        pub(crate) capacity: usize,
        pub(crate) buffer: VecDeque<bytes::Bytes>,
    }

    impl WriteStreamInner {
        pub(crate) fn len(&self) -> usize {
            let mut out = 0;
            for buf in self.buffer.iter() {
                out += buf.len();
            }
            out
        }
    }

    pub(crate) type WriteStreamCore = Arc<Mutex<WriteStreamInner>>;

    /// the backend of a write stream, allows collecting the written data
    pub struct WriteStreamBackend(WriteStreamCore);

    impl Drop for WriteStreamBackend {
        fn drop(&mut self) {
            if let Some(waker) = {
                let mut inner = self.0.lock();
                inner.closed = true;
                inner.gone = true;
                inner.write_waker.take()
            } {
                waker.wake()
            }
        }
    }

    impl WriteStreamBackend {
        /// take some data out of this write stream backend
        pub fn take(
            &self,
            should_close: &mut bool,
            wake_later: &mut WakeLater,
            dest: &mut VecDeque<bytes::Bytes>,
            max_byte_count: usize,
        ) {
            if max_byte_count == 0 {
                return;
            }
            let mut inner = self.0.lock();
            let mut remain_bytes = max_byte_count;
            wake_later.push(inner.write_waker.take());
            while remain_bytes > 0 && !inner.buffer.is_empty() {
                let this_len = inner.buffer.front().unwrap().len();
                if this_len <= remain_bytes {
                    dest.push_back(inner.buffer.pop_front().unwrap());
                    remain_bytes -= this_len;
                } else {
                    dest.push_back(
                        inner
                            .buffer
                            .front_mut()
                            .unwrap()
                            .split_to(remain_bytes),
                    );
                    remain_bytes = 0;
                }
            }
            if inner.buffer.is_empty() && inner.closed {
                *should_close = true;
            }
        }
    }
    */
}

use backend::*;

/// Quic Read Stream
pub struct ReadStream {
    recv:
        tokio::sync::mpsc::UnboundedReceiver<(bytes::Bytes, CounterAddPermit)>,
    buf: Option<(bytes::Bytes, CounterAddPermit)>,
}

impl ReadStream {
    /// read a chunk of data from the stream
    pub fn poll_read_chunk(
        &mut self,
        cx: &mut Context<'_>,
        max_bytes: usize,
    ) -> Poll<Option<bytes::Bytes>> {
        if max_bytes == 0 {
            return Poll::Ready(Some(bytes::Bytes::new()));
        }

        if self.buf.is_none() {
            match self.recv.poll_recv(cx) {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(None) => return Poll::Ready(None),
                Poll::Ready(Some((b, p))) => self.buf = Some((b, p)),
            }
        }

        let (mut b, mut p) = self.buf.take().unwrap();

        if b.len() <= max_bytes {
            Poll::Ready(Some(b))
        } else {
            let out = b.split_to(max_bytes);
            p.downgrade_to(max_bytes);
            self.buf = Some((b, p));
            Poll::Ready(Some(out))
        }
    }

    /// read a chunk of data from the stream
    pub async fn read_chunk(
        &mut self,
        max_bytes: usize,
    ) -> Option<bytes::Bytes> {
        struct X<'lt>(&'lt mut ReadStream, usize);

        impl<'lt> Future for X<'lt> {
            type Output = Option<bytes::Bytes>;

            fn poll(
                mut self: Pin<&mut Self>,
                cx: &mut Context<'_>,
            ) -> Poll<Self::Output> {
                let max_bytes = self.1;
                self.0.poll_read_chunk(cx, max_bytes)
            }
        }

        X(self, max_bytes).await
    }

    /*
    /// cancel the stream with given error code
    pub fn poll_stop(
        &mut self,
        _cx: Context<'_>,
        _error_code: u64,
    ) -> Poll<AqResult<()>> {
        todo!()
    }

    /// cancel the stream with given error code
    pub async fn stop(&mut self, _error_code: u64) -> AqResult<()> {
        todo!()
    }
    */
}

/// Quic Write Stream
pub struct WriteStream {
    counter: Counter,
    send: tokio::sync::mpsc::UnboundedSender<WriteCmdInner>,
}

impl WriteStream {
    /// write data to the stream
    pub fn poll_write_chunk(
        &mut self,
        cx: &mut Context<'_>,
        chunk: &mut bytes::Bytes,
    ) -> Poll<AqResult<()>> {
        match self.counter.reserve(cx) {
            None => Poll::Pending,
            Some(mut permit) => {
                let b = if chunk.len() >= permit.len() {
                    chunk.split_to(permit.len())
                } else {
                    let len = chunk.len();
                    permit.downgrade_to(len);
                    chunk.split_to(len)
                };
                self.send
                    .send(WriteCmdInner::Data(b, permit))
                    .map_err(|_| one_err::OneErr::new("ChannelClosed"))?;
                Poll::Ready(Ok(()))
            }
        }
    }

    /// write data to the stream
    pub async fn write_chunk(
        &mut self,
        chunk: &mut bytes::Bytes,
    ) -> AqResult<()> {
        struct X<'lt1, 'lt2>(&'lt1 mut WriteStream, &'lt2 mut bytes::Bytes);

        impl<'lt1, 'lt2> Future for X<'lt1, 'lt2> {
            type Output = AqResult<()>;

            fn poll(
                mut self: Pin<&mut Self>,
                cx: &mut Context<'_>,
            ) -> Poll<Self::Output> {
                let X(s, b) = &mut *self;
                s.poll_write_chunk(cx, b)
            }
        }

        X(self, chunk).await
    }

    /// signal shutdown on this write channel
    pub fn finish(self) -> OneShotReceiver<()> {
        OneShotReceiver::new(async move {
            let (s, r) = one_shot_channel();
            self.send
                .send(WriteCmdInner::Finish(s))
                .map_err(|_| one_err::OneErr::new("ChannelClosed"))?;
            Ok(OneShotKind::Forward(r.into_inner()))
        })
    }

    /// reset this write channel
    pub fn reset(self, error_code: u64) -> OneShotReceiver<()> {
        OneShotReceiver::new(async move {
            let (s, r) = one_shot_channel();
            self.send
                .send(WriteCmdInner::Reset(error_code, s))
                .map_err(|_| one_err::OneErr::new("ChannelClosed"))?;
            Ok(OneShotKind::Forward(r.into_inner()))
        })
    }
}

/*
/// Quic Write Stream
pub struct WriteStream(WriteStreamCore);

impl Drop for WriteStream {
    fn drop(&mut self) {
        self.0.lock().closed = true;
    }
}

impl WriteStream {
    /// Attempt to write data to this WriteStream
    pub fn poll_write(
        &mut self,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<AqResult<usize>> {
        let mut inner = self.0.lock();
        if inner.closed {
            return Poll::Ready(Err("WriteStreamClosed".into()));
        }
        let write_len = std::cmp::min(buf.len(), inner.capacity - inner.len());
        if write_len == 0 {
            inner.write_waker = Some(cx.waker().clone());
            return Poll::Pending;
        }
        inner
            .buffer
            .push_back(bytes::Bytes::copy_from_slice(&buf[..write_len]));
        Poll::Ready(Ok(write_len))
    }

    // private inner flush / shutdown handler
    fn poll_flush_or_shutdown(
        &mut self,
        cx: &mut Context<'_>,
        shutdown: bool,
    ) -> Poll<AqResult<()>> {
        let mut inner = self.0.lock();
        if inner.gone && !inner.buffer.is_empty() {
            Poll::Ready(Err("WriteStreamClosed".into()))
        } else if inner.buffer.is_empty() {
            if shutdown {
                inner.closed = true;
            }
            Poll::Ready(Ok(()))
        } else {
            inner.write_waker = Some(cx.waker().clone());
            Poll::Pending
        }
    }

    /// Flush all data out of this write stream
    pub fn poll_flush(&mut self, cx: &mut Context<'_>) -> Poll<AqResult<()>> {
        self.poll_flush_or_shutdown(cx, false)
    }

    /// Close this write stream
    pub fn poll_shutdown(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<AqResult<()>> {
        self.poll_flush_or_shutdown(cx, true)
    }
}
*/
