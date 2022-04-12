//! Absquic quiche ep factory

use absquic_core::*;
use absquic_core::con::*;
use absquic_core::ep::*;
use absquic_core::udp::*;

/// Absquic quiche ep factory
pub struct QuicheEpFactory {
    _config: quiche::Config,
}

impl QuicheEpFactory {
    /// Construct a new absquic quiche ep factory
    pub fn new(config: quiche::Config) -> Self {
        Self {
            _config: config,
        }
    }
}

impl EpFactory for QuicheEpFactory {
    type ConTy = DynCon;
    type ConRecvTy = DynConRecv;
    type EpTy = DynEp;
    type EpRecvTy = DynEpRecv;
    type BindFut = BoxFut<'static, Result<(Self::EpTy, Self::EpRecvTy)>>;

    fn bind<U: UdpFactory>(self, _udp: U) -> Self::BindFut {
        todo!()
    }
}
