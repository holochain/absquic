#![deny(missing_docs)]
#![deny(warnings)]
#![deny(unsafe_code)]
//! Absquic_core quic state-machine abstraction

use std::future::Future;
use std::task::Context;
use std::task::Poll;
use std::pin::Pin;

/// Re-exported dependencies
pub mod deps {
    pub use bytes;
    pub use futures_core;
}

pub use std::io::Result;

/// Absquic boxed future type
#[must_use = "futures do nothing unless you `.await` or poll them"]
pub struct BoxFut<'lt, T>(pub futures_core::future::BoxFuture<'lt, T>);

impl<'lt, T> BoxFut<'lt, T> {
    /// Construct a new absquic boxed future from a generic future
    #[inline(always)]
    pub fn new<F>(f: F) -> Self
    where
        F: Future<Output = T> + 'static + Send,
    {
        Self(Box::pin(f))
    }
}

impl<'lt, T> Future for BoxFut<'lt, T> {
    type Output = T;

    #[inline(always)]
    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Self::Output> {
        Future::poll(self.0.as_mut(), cx)
    }
}

/// Absquic helper until `std::io::Error::other()` is stablized
pub fn other_err<E: Into<Box<dyn std::error::Error + Send + Sync>>>(error: E) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::Other, error)
}

/// Absquic boxed stream receiver type
pub struct BoxRecv<'lt, T>(pub futures_core::stream::BoxStream<'lt, T>);

impl<'lt, T> BoxRecv<'lt, T> {
    /// Construct a new absquic boxed stream receiver from generic
    #[inline(always)]
    pub fn new<S>(s: S) -> Self
    where
        S: futures_core::stream::Stream<Item = T> + 'static + Send,
    {
        Self(Box::pin(s))
    }

    /// Poll this stream receiver for the next item
    #[inline(always)]
    pub fn poll_recv(&mut self, cx: &mut Context<'_>) -> Poll<Option<T>> {
        <dyn futures_core::stream::Stream<Item = T>>::poll_next(self.0.as_mut(), cx)
    }

    /// Attempt to pull the next item from this stream receiver
    #[inline(always)]
    pub async fn recv(&mut self) -> Option<T> {
        struct X<'lt, 'a, T>(&'a mut BoxRecv<'lt, T>);

        impl<'lt, 'a, T> Future for X<'lt, 'a, T> {
            type Output = Option<T>;

            fn poll(
                mut self: Pin<&mut Self>,
                cx: &mut Context<'_>,
            ) -> Poll<Self::Output> {
                futures_core::Stream::poll_next(self.0.0.as_mut(), cx)
            }
        }

        X(self).await
    }
}

impl<'lt, T> futures_core::stream::Stream for BoxRecv<'lt, T> {
    type Item = T;

    #[inline(always)]
    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        <dyn futures_core::stream::Stream<Item = T>>::poll_next(self.0.as_mut(), cx)
    }

    #[inline(always)]
    fn size_hint(&self) -> (usize, Option<usize>) {
        <dyn futures_core::stream::Stream<Item = T>>::size_hint(&self.0)
    }
}

pub mod con;
pub mod ep;
pub mod rt;
pub mod udp;
