//! Absquic quiche ep factory

use crate::con::*;
use crate::ep::*;
use absquic_core::ep::*;
use absquic_core::rt::*;
use absquic_core::udp::*;
use absquic_core::*;

/// Absquic quiche ep factory
pub struct QuicheEpFactory<R, U>
where
    R: Rt,
    U: UdpFactory,
{
    config: quiche::Config,
    udp_factory: U,
    _p: std::marker::PhantomData<R>,
}

impl<R, U> QuicheEpFactory<R, U>
where
    R: Rt,
    U: UdpFactory,
{
    /// Construct a new absquic quiche ep factory
    pub fn new(config: quiche::Config, udp_factory: U) -> Self {
        Self {
            config,
            udp_factory,
            _p: std::marker::PhantomData,
        }
    }
}

impl<R, U> EpFactory for QuicheEpFactory<R, U>
where
    R: Rt,
    U: UdpFactory,
{
    type ConTy = QuicheCon<R>;
    type ConRecvTy = QuicheConRecv<R>;
    type EpTy = QuicheEp<R, U::UdpTy>;
    type EpRecvTy = QuicheEpRecv<R>;
    type BindFut = BoxFut<'static, Result<(Self::EpTy, Self::EpRecvTy)>>;

    fn bind(self) -> Self::BindFut {
        let Self {
            config,
            udp_factory,
            ..
        } = self;
        BoxFut::new(async move {
            let (udp_send, udp_recv) = udp_factory.bind().await?;
            let (ep, recv) = quiche_ep(config, udp_send, udp_recv);
            Ok((ep, recv))
        })
    }
}
