//! absquic_core stream types

use crate::util::*;
use crate::AqResult;
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::sync::Arc;
use std::task::Context;
use std::task::Poll;
use std::task::Waker;

/// types only relevant when implementing a quic state machine backend
pub mod backend {
    use super::*;

    pub(crate) struct ReadStreamInner {
        pub(crate) closed: bool,
        pub(crate) read_waker: Option<Waker>,
        pub(crate) buffer: VecDeque<bytes::Bytes>,
    }

    pub(crate) type ReadStreamCore = Arc<Mutex<ReadStreamInner>>;

    /// the backend of a read stream, allows publish data to the api user
    pub struct ReadStreamBackend(ReadStreamCore);

    impl Drop for ReadStreamBackend {
        fn drop(&mut self) {
            if let Some(waker) = {
                let mut inner = self.0.lock();
                inner.closed = true;
                inner.read_waker.take()
            } {
                waker.wake();
            }
        }
    }

    impl ReadStreamBackend {
        /// push data onto the read stream,
        /// optionally also flagging it for close
        pub fn push(
            &self,
            should_close: &mut bool,
            wake_later: &mut WakeLater,
            data: &mut VecDeque<bytes::Bytes>,
            close: bool,
        ) {
            let mut inner = self.0.lock();
            inner.buffer.append(data);
            wake_later.push(inner.read_waker.take());
            if close {
                inner.closed = true;
            }
            if inner.closed {
                *should_close = true;
            }
        }
    }

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
}

use backend::*;

/// Quic Read Stream
pub struct ReadStream(ReadStreamCore);

impl Drop for ReadStream {
    fn drop(&mut self) {
        self.0.lock().closed = true;
    }
}

impl ReadStream {
    /// Attempt to read data from this ReadStream
    pub fn poll_read(
        &mut self,
        cx: &mut Context<'_>,
        max_byte_count: usize,
    ) -> Poll<AqResult<bytes::Bytes>> {
        let mut inner = self.0.lock();
        if inner.buffer.is_empty() {
            if inner.closed {
                Poll::Ready(Err("ReadStreamClosed".into()))
            } else {
                inner.read_waker = Some(cx.waker().clone());
                Poll::Pending
            }
        } else {
            if max_byte_count <= inner.buffer.front().unwrap().len() {
                Poll::Ready(Ok(inner.buffer.pop_front().unwrap()))
            } else {
                Poll::Ready(Ok(inner
                    .buffer
                    .front_mut()
                    .unwrap()
                    .split_to(max_byte_count)))
            }
        }
    }
}

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
