//! absquic_core utility types

use crate::AqResult;
use parking_lot::Mutex;
use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
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

struct OneShotInner<T: 'static + Send> {
    waker: Option<Waker>,
    value: Option<T>,
    closed: bool,
}

type OneShotCore<T> = Arc<Mutex<OneShotInner<T>>>;

/// Sender side of a one-shot channel
pub struct OneShotSender<T: 'static + Send>(OneShotCore<T>);

impl<T: 'static + Send> Drop for OneShotSender<T> {
    fn drop(&mut self) {
        if let Some(waker) = {
            let mut inner = self.0.lock();
            inner.closed = true;
            inner.waker.take()
        } {
            waker.wake();
        }
    }
}

impl<T: 'static + Send> OneShotSender<T> {
    /// send the item to the receiver side of this channel
    pub fn send(self, wake_later: &mut WakeLater, t: T) {
        let mut inner = self.0.lock();
        inner.closed = true;
        inner.value = Some(t);
        wake_later.push(inner.waker.take());
    }
}

/// Receiver side of a one-shot channel
pub struct OneShotReceiver<T: 'static + Send>(OneShotCore<T>);

impl<T: 'static + Send> OneShotReceiver<T> {
    /// future-aware poll receive function
    pub fn poll_recv(&mut self, cx: &mut Context<'_>) -> Poll<Option<T>> {
        let mut inner = self.0.lock();
        if let Some(t) = inner.value.take() {
            Poll::Ready(Some(t))
        } else if inner.closed {
            Poll::Ready(None)
        } else {
            inner.waker = Some(cx.waker().clone());
            Poll::Pending
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
    let inner = OneShotInner {
        waker: None,
        value: None,
        closed: false,
    };
    let core = Arc::new(Mutex::new(inner));
    (OneShotSender(core.clone()), OneShotReceiver(core))
}

struct OutChanInner<T: 'static + Send> {
    closed: bool,
    read_waker: Option<Waker>,
    buffer: VecDeque<T>,
}

type OutChanCore<T> = Arc<Mutex<OutChanInner<T>>>;

/// sender side of an out-going data oriented channel
pub struct OutChanSender<T: 'static + Send>(OutChanCore<T>);

impl<T: 'static + Send> Drop for OutChanSender<T> {
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

impl<T: 'static + Send> OutChanSender<T> {
    /// is this channel closed
    pub fn is_closed(&self) -> bool {
        self.0.lock().closed
    }

    /// push data into this channel
    pub fn push(
        &self,
        should_close: &mut bool,
        wake_later: &mut WakeLater,
        data: &mut VecDeque<T>,
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

/// receiver side of an out-going data oriented channel
pub struct OutChanReceiver<T: 'static + Send>(OutChanCore<T>);

impl<T: 'static + Send> Drop for OutChanReceiver<T> {
    fn drop(&mut self) {
        self.0.lock().closed = true;
    }
}

impl<T: 'static + Send> OutChanReceiver<T> {
    /// Receive data sent by the sender on this channel
    pub fn poll_recv(&mut self, cx: &mut Context<'_>) -> Poll<Option<T>> {
        let mut inner = self.0.lock();
        if inner.buffer.is_empty() {
            if inner.closed {
                Poll::Ready(None)
            } else {
                inner.read_waker = Some(cx.waker().clone());
                Poll::Pending
            }
        } else {
            Poll::Ready(inner.buffer.pop_front())
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
    let inner = OutChanInner {
        closed: false,
        read_waker: None,
        buffer: VecDeque::new(),
    };
    let core = Arc::new(Mutex::new(inner));
    (OutChanSender(core.clone()), OutChanReceiver(core))
}

struct InChanInner<T: 'static + Send> {
    closed: bool,
    buffer: VecDeque<T>,
    waker: Option<Waker>,
}

type InChanCore<T> = Arc<Mutex<InChanInner<T>>>;

/// sender side of an incoming data channel
pub struct InChanSender<T: 'static + Send>(InChanCore<T>);

impl<T: 'static + Send> Clone for InChanSender<T> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<T: 'static + Send> Drop for InChanSender<T> {
    fn drop(&mut self) {
        if let Some(waker) = {
            let mut inner = self.0.lock();
            inner.closed = true;
            inner.waker.take()
        } {
            waker.wake();
        }
    }
}

impl<T: 'static + Send> InChanSender<T> {
    /// send data into the channel
    pub fn send(&self, t: T) -> AqResult<()> {
        if let Some(waker) = {
            let mut inner = self.0.lock();
            if inner.closed {
                return Err("ChannelClosed".into());
            } else {
                inner.buffer.push_back(t);
            }
            inner.waker.take()
        } {
            waker.wake();
        }
        Ok(())
    }
}

/// receiver side of an incoming data channel
pub struct InChanReceiver<T: 'static + Send>(InChanCore<T>);

impl<T: 'static + Send> Drop for InChanReceiver<T> {
    fn drop(&mut self) {
        self.0.lock().closed = true;
    }
}

impl<T: 'static + Send> InChanReceiver<T> {
    /// register a waker that will be triggered
    /// on new incoming send
    pub fn register_waker(&self, waker: Waker) {
        self.0.lock().waker = Some(waker);
    }

    /// receive data from the channel
    pub fn recv(
        &self,
        should_close: &mut bool,
        dest: &mut VecDeque<T>,
        close: bool,
    ) {
        let mut inner = self.0.lock();
        dest.append(&mut inner.buffer);
        if close {
            inner.closed = true;
        }
        if inner.closed {
            *should_close = true;
        }
    }
}

/// create a new incoming data oriented channel pair
pub fn in_chan<T: 'static + Send>() -> (InChanSender<T>, InChanReceiver<T>) {
    let inner = InChanInner {
        closed: false,
        buffer: VecDeque::new(),
        waker: None,
    };
    let core = Arc::new(Mutex::new(inner));
    (InChanSender(core.clone()), InChanReceiver(core))
}

#[cfg(test)]
mod test {
    use super::*;
    use futures::future::FutureExt;

    #[tokio::test(flavor = "multi_thread")]
    async fn test_one_shot() {
        let mut all = Vec::with_capacity(40);

        for _ in 0..10 {
            let (s, r) = one_shot_channel();
            all.push(
                async move {
                    let mut wake_later = WakeLater::new();
                    s.send(&mut wake_later, ());
                    wake_later.wake();
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
                        let mut wake_later = WakeLater::new();
                        s.send(&mut wake_later, ());
                        wake_later.wake();
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
        s.push(&mut should_close, &mut WakeLater::new(), &mut out, true);
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
        s.push(&mut should_close, &mut WakeLater::new(), &mut out, false);
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
        s.push(&mut should_close, &mut WakeLater::new(), &mut out, false);
        drop(s);
        task.await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_in_chan_recv_drop() {
        let (s, r) = in_chan::<()>();
        drop(r);
        assert!(s.send(()).is_err());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_in_chan_send_drop() {
        let (s, r) = in_chan::<()>();
        drop(s);
        let mut should_close = false;
        let mut buf = VecDeque::new();
        r.recv(&mut should_close, &mut buf, false);
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
                    self.0.register_waker(cx.waker().clone());
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

        let (s, r) = in_chan::<()>();

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
}
