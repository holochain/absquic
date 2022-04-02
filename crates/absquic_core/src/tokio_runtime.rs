//! `feature = "tokio_runtime"` Absquic_core tokio runtime

use crate::runtime::*;
use crate::*;
use std::future::Future;
use std::pin::Pin;

/// Absquic AsyncRuntime backed by tokio
pub struct TokioRuntime;

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
