use super::*;
use absquic_core::ep::*;
use std::net::SocketAddr;

async fn bind() -> (DynEp, DynEpRecv, SocketAddr) {
    let cert =
        rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let pk_pem = cert.serialize_private_key_pem();
    let pk =
        boring::pkey::PKey::private_key_from_pem(pk_pem.as_bytes()).unwrap();
    let cert_pem = cert.serialize_pem().unwrap();
    let cert = boring::x509::X509::from_pem(cert_pem.as_bytes()).unwrap();
    let mut ctx =
        boring::ssl::SslContext::builder(boring::ssl::SslMethod::tls())
            .unwrap();
    ctx.set_certificate(&cert).unwrap();
    ctx.set_private_key(&pk).unwrap();
    let ctx = ctx.build();
    let mut config =
        quiche::Config::with_boring_ssl_ctx(quiche::PROTOCOL_VERSION, ctx)
            .unwrap();
    config
        .set_application_protos(quiche::h3::APPLICATION_PROTOCOL)
        .unwrap();
    config.verify_peer(false);

    let udp =
        absquic_tokio::TokioUdpFactory::new(([127, 0, 0, 1], 0).into(), None);
    let fact = <QuicheEpFactory<
        absquic_tokio::TokioRt,
        absquic_tokio::TokioUdpFactory,
    >>::new(config, udp);
    let fact = DynEpFactory::new(fact);
    let (ep, recv) = fact.bind().await.unwrap();
    let addr = ep.addr().await.unwrap();
    (ep, recv, addr)
}

#[tokio::test(flavor = "multi_thread")]
async fn sanity() {
    let (_ep1, _recv1, addr1) = bind().await;
    println!("addr1: {:?}", addr1);
    let (_ep2, _recv2, addr2) = bind().await;
    println!("addr2: {:?}", addr2);
}
