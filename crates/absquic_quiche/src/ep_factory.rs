//! Absquic quiche ep factory

use crate::con::*;
use crate::ep::*;
use absquic_core::ep::*;
use absquic_core::rt::*;
use absquic_core::udp::*;
use absquic_core::*;
use std::sync::Arc;

/// Absquic quiche ep factory
pub struct QuicheEpFactory<R, UFact>
where
    R: Rt,
    UFact: UdpFactory,
{
    config: quiche::Config,
    h3_config: Option<quiche::h3::Config>,
    udp_factory: UFact,
    _p: std::marker::PhantomData<R>,
}

impl<R, UFact> QuicheEpFactory<R, UFact>
where
    R: Rt,
    UFact: UdpFactory,
{
    /// Construct a new absquic quiche ep factory
    pub fn new(
        config: quiche::Config,
        h3_config: Option<quiche::h3::Config>,
        udp_factory: UFact,
    ) -> Self {
        Self {
            config,
            h3_config,
            udp_factory,
            _p: std::marker::PhantomData,
        }
    }
}

impl<R, UFact> EpFactory for QuicheEpFactory<R, UFact>
where
    R: Rt,
    UFact: UdpFactory,
{
    type ConTy = QuicheCon<R>;
    type ConRecvTy = QuicheConRecv<R>;
    type EpTy = QuicheEp<R, UFact::UdpTy>;
    type EpRecvTy = QuicheEpRecv<R>;
    type BindFut = BoxFut<'static, Result<(Self::EpTy, Self::EpRecvTy)>>;

    fn bind(self) -> Self::BindFut {
        let Self {
            config,
            h3_config,
            udp_factory,
            ..
        } = self;
        let h3_config = h3_config.map(Arc::new);
        BoxFut::new(async move {
            let (udp_send, udp_recv) = udp_factory.bind().await?;
            let (ep, recv) = quiche_ep(config, h3_config, udp_send, udp_recv);
            Ok((ep, recv))
        })
    }
}
