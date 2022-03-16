//! absquic_core utility types

use crate::AqResult;
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic;
use std::task::Context;
use std::task::Poll;
use std::task::Waker;

/// Util struct to aggregate wakers we want to wake later
/// usually after a synchronization lock has been released
/// Dropping this struct will wake all aggregated wakers
pub struct WakeLater(Vec<Waker>);

impl Drop for WakeLater {
    fn drop(&mut self) {
        self.wake();
    }
}

impl Default for WakeLater {
    fn default() -> Self {
        WakeLater::new()
    }
}

impl WakeLater {
    /// construct a new WakeLater instance
    pub fn new() -> Self {
        Self(Vec::new())
    }

    /// push a waker into this instance
    #[inline(always)]
    pub fn push(&mut self, waker: Option<Waker>) {
        if let Some(waker) = waker {
            self.0.push(waker);
        }
    }

    /// wake all wakers aggregated by this instance
    pub fn wake(&mut self) {
        for waker in self.0.drain(..) {
            waker.wake();
        }
    }
}

/// Sender side of a one-shot channel
pub struct OneShotSender<T: 'static + Send>(tokio::sync::oneshot::Sender<T>);

impl<T: 'static + Send> OneShotSender<T> {
    /// send the item to the receiver side of this channel
    pub fn send(self, t: T) {
        let _ = self.0.send(t);
    }
}

/// Receiver side of a one-shot channel
pub struct OneShotReceiver<T: 'static + Send>(
    tokio::sync::oneshot::Receiver<T>,
);

impl<T: 'static + Send> OneShotReceiver<T> {
    /// future-aware poll receive function
    pub fn poll_recv(&mut self, cx: &mut Context<'_>) -> Poll<Option<T>> {
        match std::pin::Pin::new(&mut self.0).poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Err(_)) => Poll::Ready(None),
            Poll::Ready(Ok(t)) => Poll::Ready(Some(t)),
        }
    }

    /// receive the item sent by the sender side of this channel
    pub async fn recv(mut self) -> Option<T> {
        struct X<'lt, T: 'static + Send>(&'lt mut OneShotReceiver<T>);

        impl<'lt, T: 'static + Send> Future for X<'lt, T> {
            type Output = Option<T>;

            fn poll(
                mut self: Pin<&mut Self>,
                cx: &mut Context<'_>,
            ) -> Poll<Self::Output> {
                self.0.poll_recv(cx)
            }
        }

        X(&mut self).await
    }
}

/// create a one-shot channel sender / receiver pair
pub fn one_shot_channel<T: 'static + Send>(
) -> (OneShotSender<T>, OneShotReceiver<T>) {
    let (s, r) = tokio::sync::oneshot::channel();
    (OneShotSender(s), OneShotReceiver(r))
}

/// sender side of an out-going data oriented channel
pub struct OutChanSender<T: 'static + Send> {
    want_close: atomic::AtomicBool,
    sender: tokio::sync::mpsc::UnboundedSender<Option<T>>,
}

impl<T: 'static + Send> OutChanSender<T> {
    /// is this channel closed
    pub fn is_closed(&self) -> bool {
        if self.want_close.load(atomic::Ordering::SeqCst) {
            return true;
        }
        self.sender.is_closed()
    }

    /// push data into this channel
    pub fn push(
        &self,
        should_close: &mut bool,
        data: &mut VecDeque<T>,
        close: bool,
    ) {
        loop {
            if self.is_closed() {
                *should_close = true;
                break;
            }

            if data.is_empty() {
                break;
            }

            if let Err(_) = self.sender.send(Some(data.pop_front().unwrap())) {
                *should_close = true;
                break;
            }
        }

        if close || *should_close {
            self.want_close.store(true, atomic::Ordering::SeqCst);
            let _ = self.sender.send(None);
            *should_close = true;
        }
    }
}

/// receiver side of an out-going data oriented channel
pub struct OutChanReceiver<T: 'static + Send> {
    receiver: Option<tokio::sync::mpsc::UnboundedReceiver<Option<T>>>,
}

impl<T: 'static + Send> OutChanReceiver<T> {
    /// Receive data sent by the sender on this channel
    pub fn poll_recv(&mut self, cx: &mut Context<'_>) -> Poll<Option<T>> {
        if self.receiver.is_none() {
            return Poll::Ready(None);
        }
        match self.receiver.as_mut().unwrap().poll_recv(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Some(Some(t))) => {
                // the stream sent a value
                Poll::Ready(Some(t))
            }
            Poll::Ready(None) => {
                // the stream properly ended
                drop(self.receiver.take());
                Poll::Ready(None)
            }
            Poll::Ready(Some(None)) => {
                // the stream sent a close signal
                drop(self.receiver.take());
                Poll::Ready(None)
            }
        }
    }

    /// receive data sent by the sender on this channel
    pub async fn recv(&mut self) -> Option<T> {
        struct X<'lt, T: 'static + Send>(&'lt mut OutChanReceiver<T>);

        impl<'lt, T: 'static + Send> Future for X<'lt, T> {
            type Output = Option<T>;

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

/// create a new out-going data oriented channel pair
pub fn out_chan<T: 'static + Send>() -> (OutChanSender<T>, OutChanReceiver<T>) {
    let (s, r) = tokio::sync::mpsc::unbounded_channel();
    let s = OutChanSender {
        want_close: atomic::AtomicBool::new(false),
        sender: s,
    };
    let r = OutChanReceiver { receiver: Some(r) };
    (s, r)
}

/// sender side of an incoming data channel
pub struct InChanSender<T: 'static + Send> {
    sender: tokio::sync::mpsc::Sender<T>,
}

impl<T: 'static + Send> Clone for InChanSender<T> {
    fn clone(&self) -> Self {
        Self {
            sender: self.sender.clone(),
        }
    }
}

impl<T: 'static + Send> InChanSender<T> {
    /// send data into the channel
    pub async fn send(&self, t: T) -> AqResult<()> {
        self.sender
            .send(t)
            .await
            .map_err(|_| "ChannelClosed".into())
    }

    /*
    /// send data into the channel
    pub fn send(&self, t: T) -> AqResult<()> {
        if let Some(wake_on_send) = {
            let mut inner = self.0.lock();
            if inner.closed {
                return Err("ChannelClosed".into());
            } else {
                inner.buffer.push_back(t);
            }
            inner.wake_on_send.take()
        } {
            wake_on_send.wake();
        }
        Ok(())
    }
    */
}

/// receiver side of an incoming data channel
pub struct InChanReceiver<T: 'static + Send> {
    receiver: Option<tokio::sync::mpsc::Receiver<T>>,
}

impl<T: 'static + Send> InChanReceiver<T> {
    /// receive data from the channel
    pub fn poll_recv(&mut self, cx: &mut Context<'_>) -> Poll<Option<T>> {
        if let Some(r) = &mut self.receiver {
            r.poll_recv(cx)
        } else {
            Poll::Ready(None)
        }
    }

    /// receive data from the channel
    pub async fn recv(&mut self) -> Option<T> {
        struct X<'lt, T: 'static + Send>(&'lt mut InChanReceiver<T>);

        impl<'lt, T: 'static + Send> Future for X<'lt, T> {
            type Output = Option<T>;

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

/// create a new incoming data oriented channel pair
pub fn in_chan<T: 'static + Send>(
    limit: usize,
) -> (InChanSender<T>, InChanReceiver<T>) {
    let (s, r) = tokio::sync::mpsc::channel(limit);
    let s = InChanSender { sender: s };
    let r = InChanReceiver { receiver: Some(r) };
    (s, r)
}

#[cfg(test)]
mod test {
    use super::*;
    use futures::future::FutureExt;
    //use parking_lot::Mutex;
    //use std::sync::Arc;

    #[tokio::test(flavor = "multi_thread")]
    async fn test_one_shot() {
        let mut all = Vec::with_capacity(40);

        for _ in 0..10 {
            let (s, r) = one_shot_channel();
            all.push(
                async move {
                    s.send(());
                }
                .boxed(),
            );
            all.push(
                async move {
                    let _ = r.recv().await;
                }
                .boxed(),
            );
        }

        for _ in 0..10 {
            let (s, r) = one_shot_channel();
            all.push(
                async move {
                    let _ = tokio::task::spawn(async move {
                        s.send(());
                    })
                    .await;
                }
                .boxed(),
            );
            all.push(
                async move {
                    let _ = tokio::task::spawn(async move {
                        let _ = r.recv().await;
                    })
                    .await;
                }
                .boxed(),
            );
        }

        futures::future::join_all(all).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_out_chan_send_drop_close() {
        let (s, mut r) = out_chan::<()>();
        let task = tokio::task::spawn(async move {
            assert!(matches!(r.recv().await, None));
        });
        drop(s);
        task.await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_out_chan_send_explicit_close() {
        let (s, mut r) = out_chan::<()>();
        let task = tokio::task::spawn(async move {
            assert!(matches!(r.recv().await, None));
        });
        let mut should_close = false;
        let mut out = VecDeque::new();
        s.push(&mut should_close, &mut out, true);
        assert!(should_close);
        task.await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_out_chan_recv_close() {
        let (s, r) = out_chan::<()>();
        drop(r);
        let mut should_close = false;
        let mut out = VecDeque::new();
        out.push_back(());
        s.push(&mut should_close, &mut out, false);
        assert!(should_close);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_out_chan_some() {
        let (s, mut r) = out_chan::<()>();
        let task = tokio::task::spawn(async move {
            assert!(matches!(r.recv().await, Some(())));
            assert!(matches!(r.recv().await, Some(())));
            assert!(matches!(r.recv().await, None));
        });
        let mut should_close = false;
        let mut out = VecDeque::new();
        out.push_back(());
        out.push_back(());
        s.push(&mut should_close, &mut out, false);
        drop(s);
        task.await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_in_chan_recv_drop() {
        let (s, r) = in_chan::<()>(32);
        drop(r);
        assert!(s.send(()).await.is_err());
    }

    /*
    #[tokio::test(flavor = "multi_thread")]
    async fn test_in_chan_send_drop() {
        let (s, r) = in_chan::<()>(32);
        drop(s);
        let mut should_close = false;
        let mut buf = VecDeque::new();
        r.recv();
        assert!(should_close);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_in_chan_wake() {
        struct X(InChanReceiver<()>);

        impl Future for X {
            type Output = ();

            fn poll(
                self: std::pin::Pin<&mut Self>,
                cx: &mut Context<'_>,
            ) -> Poll<Self::Output> {
                let mut should_close = false;
                let mut buf = VecDeque::new();
                self.0.recv(&mut should_close, &mut buf, false);
                if buf.is_empty() {
                    self.0.register_wake_on_send(cx.waker().clone());
                    Poll::Pending
                } else {
                    Poll::Ready(())
                }
            }
        }

        struct W(Mutex<usize>);

        impl futures::task::ArcWake for W {
            fn wake_by_ref(arc_self: &Arc<Self>) {
                *arc_self.0.lock() += 1;
            }
        }

        let waker_inner = Arc::new(W(Mutex::new(0)));

        let waker = futures::task::waker(waker_inner.clone());
        let mut cx = Context::from_waker(&waker);

        let (s, r) = in_chan::<()>(32);

        let mut x = X(r);
        assert!(matches!(
            std::pin::Pin::new(&mut x).poll(&mut cx),
            Poll::Pending
        ));
        assert_eq!(0, *waker_inner.0.lock());

        s.send(()).unwrap();
        assert_eq!(1, *waker_inner.0.lock());
        assert!(matches!(
            std::pin::Pin::new(&mut x).poll(&mut cx),
            Poll::Ready(())
        ));
    }
    */
}
