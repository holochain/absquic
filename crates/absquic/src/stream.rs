//! absquic stream types

use crate::AqResult;
use crate::OutChan;
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::task::Context;
use std::task::Poll;

/// Stream of incoming bytes
pub struct RecvStream(pub(crate) OutChan<AqResult<quinn_proto::Chunk>>);

impl RecvStream {
    /// get the next chunk from this recv source
    #[inline(always)]
    pub fn poll_recv(
        &mut self,
        cx: &mut Context<'_>,
    ) -> Poll<Option<AqResult<quinn_proto::Chunk>>> {
        self.0.poll_recv(cx)
    }

    /// get the next chunk from this recv source
    pub async fn recv(&mut self) -> Option<AqResult<quinn_proto::Chunk>> {
        struct X<'lt>(&'lt mut RecvStream);

        impl Future for X<'_> {
            type Output = Option<AqResult<quinn_proto::Chunk>>;

            fn poll(
                mut self: Pin<&mut Self>,
                cx: &mut Context<'_>,
            ) -> Poll<Self::Output> {
                self.0.poll_recv(cx)
            }
        }

        X(self).await
    }
}

/// Outgoing bytes
pub struct WriteStream {}

impl WriteStream {
    /// write all given bytes to this output stream
    pub fn poll_write_all(
        &mut self,
        _cx: &mut Context<'_>,
        _bytes: &mut VecDeque<bytes::Bytes>,
    ) -> Poll<AqResult<()>> {
        todo!()
    }

    /// write all given bytes to this output stream
    pub async fn write_all(
        &mut self,
        bytes: &mut VecDeque<bytes::Bytes>,
    ) -> AqResult<()> {
        struct X<'lt>(&'lt mut WriteStream, &'lt mut VecDeque<bytes::Bytes>);

        impl Future for X<'_> {
            type Output = AqResult<()>;

            fn poll(
                mut self: Pin<&mut Self>,
                cx: &mut Context<'_>,
            ) -> Poll<Self::Output> {
                let Self(stream, data) = &mut *self;
                stream.poll_write_all(cx, data)
            }
        }

        X(self, bytes).await
    }
}
