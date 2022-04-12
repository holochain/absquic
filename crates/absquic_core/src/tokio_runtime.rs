//! `feature = "tokio_runtime"` Absquic_core AsyncRuntime backed by tokio

use crate::backend::*;
use crate::endpoint::*;
use crate::rt;
use crate::runtime::*;
use crate::*;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

/// Spawn result future type
#[must_use = "futures do nothing unless you `.await` or poll them"]
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
    /// Sleep result future type
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
        let this = self.project();
        this.inner.poll(cx)
    }
}

/// Semaphore permit guard
pub struct TokioSemaphoreGuard(tokio::sync::OwnedSemaphorePermit);
impl rt::SemaphoreGuard for TokioSemaphoreGuard {}

/// Semaphore permit quard acquire future type
#[must_use = "futures do nothing unless you `.await` or poll them"]
pub struct TokioSemaphoreAcquireFut(AqFut<'static, TokioSemaphoreGuard>);

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

/// Semaphore type
pub struct TokioSemaphore(Arc<tokio::sync::Semaphore>);

impl rt::Semaphore for TokioSemaphore {
    type GuardTy = TokioSemaphoreGuard;
    type AcquireFut = TokioSemaphoreAcquireFut;

    #[inline(always)]
    fn acquire(&self) -> Self::AcquireFut {
        let sem = self.0.clone();
        // would be nice to do this without the box
        TokioSemaphoreAcquireFut(AqFut::new(async move {
            // safe to unwrap since we never close the semaphore
            let guard = sem.acquire_owned().await.unwrap();
            TokioSemaphoreGuard(guard)
        }))
    }
}

/// Channel sender type
pub struct TokioMultiSend<T: 'static + Send>(
    tokio::sync::mpsc::UnboundedSender<(T, TokioSemaphoreGuard)>,
);

impl<T: 'static + Send> rt::MultiSend<T> for TokioMultiSend<T> {
    type GuardTy = TokioSemaphoreGuard;

    #[inline(always)]
    fn send(&self, t: T, g: Self::GuardTy) -> ChanResult<()> {
        match self.0.send((t, g)) {
            Ok(_) => Ok(()),
            _ => Err(ChannelClosed),
        }
    }
}

/// Channel receiver type
#[must_use = "streams do nothing unless you `.await` or poll them"]
pub struct TokioMultiRecv<T: 'static + Send>(
    tokio::sync::mpsc::UnboundedReceiver<(T, TokioSemaphoreGuard)>,
);

impl<T: 'static + Send> futures_core::Stream for TokioMultiRecv<T> {
    type Item = (T, TokioSemaphoreGuard);

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
    ) -> (rt::DynOnceSend<T>, AqFut<'static, Option<T>>) {
        let (s, r) = tokio::sync::oneshot::channel();
        (
            Box::new(move |t| {
                let _ = s.send(t);
            }),
            AqFut::new(async move {
                match r.await {
                    Ok(r) => Some(r),
                    _ => None,
                }
            }),
        )
    }

    #[inline(always)]
    fn channel<T: 'static + Send>() -> (
        rt::DynMultiSend<T, TokioSemaphoreGuard>,
        rt::DynMultiRecv<T, TokioSemaphoreGuard>,
    ) {
        let (s, r) = tokio::sync::mpsc::unbounded_channel();
        (Arc::new(TokioMultiSend(s)), Box::pin(TokioMultiRecv(r)))
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use futures::StreamExt;
    use rt::Rt;
    use rt::Semaphore;

    #[tokio::test(flavor = "multi_thread")]
    async fn test_rt_oneshot() {
        let (s, r) = TokioRt::one_shot::<isize>();
        s(42);
        assert_eq!(42, r.await.unwrap());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn test_rt_multi_chan() {
        let (s, mut r) = TokioRt::channel::<isize>();
        let l = TokioRt::semaphore(1);
        let g = l.acquire().await;
        s.send(42, g).unwrap();
        assert_eq!(42, r.next().await.unwrap().0);
    }
}

/// `feature = "tokio_runtime"` Absquic_core AsyncRuntime backed by tokio
pub struct TokioRuntime;

impl TokioRuntime {
    /// Build an endpoint from the TokioRuntime abstract.
    /// This convenience function just makes it so you don't have to
    /// explicitly `use EndpointBuilder;`
    #[inline(always)]
    pub async fn build_endpoint<Udp, Quic>(
        udp_backend: Udp,
        quic_backend: Quic,
    ) -> AqResult<(Endpoint, MultiReceiver<EndpointEvt>)>
    where
        Udp: UdpBackendFactory,
        Quic: QuicBackendFactory,
    {
        <TokioRuntime as EndpointBuilder<TokioRuntime>>::build(
            udp_backend,
            quic_backend,
        )
        .await
    }
}

impl AsyncRuntime for TokioRuntime {
    fn spawn<R, F>(f: F) -> SpawnHnd<R>
    where
        R: 'static + Send,
        F: Future<Output = AqResult<R>> + 'static + Send,
    {
        let fut = tokio::task::spawn(f);
        SpawnHnd::new(async move { fut.await.map_err(one_err::OneErr::new)? })
    }

    fn sleep(time: std::time::Instant) -> AqFut<'static, ()> {
        AqFut::new(async move {
            tokio::time::sleep_until(time.into()).await;
        })
    }

    fn semaphore(limit: usize) -> DynSemaphore {
        struct G(tokio::sync::OwnedSemaphorePermit);
        impl SemaphoreGuard for G {}

        struct X(Arc<tokio::sync::Semaphore>);

        impl Semaphore for X {
            fn acquire(&self) -> AqFut<'static, DynSemaphoreGuard> {
                let sem = self.0.clone();
                AqFut::new(async move {
                    // safe to unwrap since we never close the semaphore
                    let guard = sem.acquire_owned().await.unwrap();
                    let guard: DynSemaphoreGuard = Box::new(G(guard));
                    guard
                })
            }
        }

        Arc::new(X(Arc::new(tokio::sync::Semaphore::new(limit))))
    }

    fn one_shot<T: 'static + Send>(
    ) -> (OnceSender<T>, AqFut<'static, Option<T>>) {
        let (s, r) = tokio::sync::oneshot::channel();
        (
            OnceSender::new(move |t| {
                let _ = s.send(t);
            }),
            AqFut::new(async move {
                match r.await {
                    Ok(r) => Some(r),
                    Err(_) => None,
                }
            }),
        )
    }

    fn channel<T: 'static + Send>(
        bound: usize,
    ) -> (MultiSender<T>, MultiReceiver<T>) {
        let (s, r) = tokio::sync::mpsc::channel(bound);

        struct S<T: 'static + Send>(tokio::sync::mpsc::Sender<T>);

        impl<T: 'static + Send> MultiSend<T> for S<T> {
            fn acquire(&self) -> AqFut<'static, ChanResult<OnceSender<T>>> {
                let s = self.0.clone();
                AqFut::new(async move {
                    let permit =
                        s.reserve_owned().await.map_err(|_| ChannelClosed)?;
                    Ok(OnceSender::new(move |t| {
                        let _ = permit.send(t);
                    }))
                })
            }
        }

        struct R<T: 'static + Send>(tokio::sync::mpsc::Receiver<T>);

        impl<T: 'static + Send> futures_core::stream::Stream for R<T> {
            type Item = T;

            fn poll_next(
                mut self: Pin<&mut Self>,
                cx: &mut Context<'_>,
            ) -> Poll<Option<Self::Item>> {
                self.0.poll_recv(cx)
            }
        }

        (MultiSender::new(S(s)), MultiReceiver::new(R(r)))
    }
}
