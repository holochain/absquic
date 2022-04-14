//! `feature = "tokio_runtime"` Absquic_core AsyncRuntime backed by tokio

use absquic_core::deps::futures_core;
use absquic_core::rt;
use absquic_core::*;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::Context;
use std::task::Poll;

/// `feature = "tokio_runtime"` Spawn result future type.
/// This item is intentionally NOT #\[must_use\]
pub struct TokioSpawnFut(tokio::task::JoinHandle<()>);

impl Future for TokioSpawnFut {
    type Output = ();

    #[inline(always)]
    fn poll(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Self::Output> {
        match Pin::new(&mut self.0).poll(cx) {
            Poll::Pending => Poll::Pending,
            _ => Poll::Ready(()),
        }
    }
}

pin_project_lite::pin_project! {
    /// `feature = "tokio_runtime"` Sleep result future type
    #[must_use = "futures do nothing unless you `.await` or poll them"]
    pub struct TokioSleepFut {
        #[pin]
        inner: tokio::time::Sleep,
    }
}

impl Future for TokioSleepFut {
    type Output = ();

    #[inline(always)]
    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        self.project().inner.poll(cx)
    }
}

/// `feature = "tokio_runtime"` Semaphore permit guard
pub struct TokioSemaphoreGuard(tokio::sync::OwnedSemaphorePermit);
impl rt::SemaphoreGuard for TokioSemaphoreGuard {}

/// `feature = "tokio_runtime"` Semaphore permit quard acquire future type
#[must_use = "futures do nothing unless you `.await` or poll them"]
pub struct TokioSemaphoreAcquireFut(BoxFut<'static, TokioSemaphoreGuard>);

impl Future for TokioSemaphoreAcquireFut {
    type Output = TokioSemaphoreGuard;

    #[inline(always)]
    fn poll(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Self::Output> {
        Pin::new(&mut self.0).poll(cx)
    }
}

/// `feature = "tokio_runtime"` Semaphore type
pub struct TokioSemaphore(Arc<tokio::sync::Semaphore>);

impl rt::Semaphore for TokioSemaphore {
    type GuardTy = TokioSemaphoreGuard;
    type AcquireFut = TokioSemaphoreAcquireFut;

    #[inline(always)]
    fn acquire(&self) -> Self::AcquireFut {
        let sem = self.0.clone();
        // would be nice to do this without the box
        TokioSemaphoreAcquireFut(BoxFut::new(async move {
            // safe to unwrap since we never close the semaphore
            let guard = sem.acquire_owned().await.unwrap();
            TokioSemaphoreGuard(guard)
        }))
    }

    #[inline(always)]
    fn try_acquire(&self) -> Option<Self::GuardTy> {
        match self.0.clone().try_acquire_owned() {
            Ok(g) => Some(TokioSemaphoreGuard(g)),
            _ => None,
        }
    }
}

/// `feature = "tokio_runtime"` Channel sender type
pub struct TokioMultiSend<T: 'static + Send>(
    tokio::sync::mpsc::UnboundedSender<T>,
);

impl<T: 'static + Send> rt::MultiSend<T> for TokioMultiSend<T> {
    #[inline(always)]
    fn send(&self, t: T) -> Result<()> {
        match self.0.send(t) {
            Ok(_) => Ok(()),
            _ => Err(other_err("ChannelClosed")),
        }
    }
}

/// `feature = "tokio_runtime"` Channel receiver type
#[must_use = "streams do nothing unless you `.await` or poll them"]
pub struct TokioMultiRecv<T: 'static + Send>(
    tokio::sync::mpsc::UnboundedReceiver<T>,
);

impl<T: 'static + Send> futures_core::Stream for TokioMultiRecv<T> {
    type Item = T;

    #[inline(always)]
    fn poll_next(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Self::Item>> {
        self.0.poll_recv(cx)
    }
}

/// `feature = "tokio_runtime"` Absquic_core AsyncRuntime backed by tokio
pub struct TokioRt;

impl TokioRt {
    /// Spawn a task to execute the given future, return a future
    /// that will complete when the task completes, but will not
    /// affect the task if dropped / cancelled
    pub fn spawn<F>(f: F) -> TokioSpawnFut
    where
        F: Future<Output = ()> + 'static + Send,
    {
        <TokioRt as rt::Rt>::spawn(f)
    }

    /// Returns a future that will complete at the specified instant,
    /// or immediately if the instant is already past
    pub fn sleep(until: std::time::Instant) -> TokioSleepFut {
        <TokioRt as rt::Rt>::sleep(until)
    }

    /// Create an async semaphore instance
    pub fn semaphore(limit: usize) -> TokioSemaphore {
        <TokioRt as rt::Rt>::semaphore(limit)
    }

    /// Creates a slot for a single data transfer
    ///
    /// Note: until generic associated types are stable
    /// we have to make do with dynamic dispatch here
    pub fn one_shot<T: 'static + Send>(
    ) -> (rt::DynOnceSend<T>, BoxFut<'static, Option<T>>) {
        <TokioRt as rt::Rt>::one_shot()
    }

    /// Creates a channel for multiple data transfer
    ///
    /// Note: until generic associated types are stable
    /// we have to make do with dynamic dispatch here
    pub fn channel<T: 'static + Send>(
    ) -> (rt::DynMultiSend<T>, BoxRecv<'static, T>) {
        <TokioRt as rt::Rt>::channel()
    }
}

impl rt::Rt for TokioRt {
    type SpawnFut = TokioSpawnFut;
    type SleepFut = TokioSleepFut;
    type Semaphore = TokioSemaphore;

    #[inline(always)]
    fn spawn<F>(f: F) -> Self::SpawnFut
    where
        F: Future<Output = ()> + 'static + Send,
    {
        TokioSpawnFut(tokio::task::spawn(f))
    }

    #[inline(always)]
    fn sleep(until: std::time::Instant) -> Self::SleepFut {
        TokioSleepFut {
            inner: tokio::time::sleep_until(until.into()),
        }
    }

    #[inline(always)]
    fn semaphore(limit: usize) -> Self::Semaphore {
        TokioSemaphore(Arc::new(tokio::sync::Semaphore::new(limit)))
    }

    #[inline(always)]
    fn one_shot<T: 'static + Send>(
    ) -> (rt::DynOnceSend<T>, BoxFut<'static, Option<T>>) {
        let (s, r) = tokio::sync::oneshot::channel();
        (
            Box::new(move |t| {
                let _ = s.send(t);
            }),
            BoxFut::new(async move {
                match r.await {
                    Ok(r) => Some(r),
                    _ => None,
                }
            }),
        )
    }

    #[inline(always)]
    fn channel<T: 'static + Send>() -> (rt::DynMultiSend<T>, BoxRecv<'static, T>)
    {
        let (s, r) = tokio::sync::mpsc::unbounded_channel();
        (Arc::new(TokioMultiSend(s)), BoxRecv::new(TokioMultiRecv(r)))
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use rt::Rt;

    #[tokio::test(flavor = "multi_thread")]
    async fn test_rt_oneshot() {
        let (s, r) = TokioRt::one_shot::<isize>();
        s(42);
        assert_eq!(42, r.await.unwrap());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_rt_multi_chan() {
        let (s, mut r) = TokioRt::channel::<isize>();
        s.send(42).unwrap();
        assert_eq!(42, r.recv().await.unwrap());
    }
}
