use super::*;
use absquic_core::ep::*;
use absquic_core::con::*;
use absquic_tokio::*;
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
    config.set_initial_max_streams_bidi(10);
    config.set_initial_max_streams_uni(10);
    config.set_initial_max_data(64 * 1024 * 8);
    config.set_initial_max_stream_data_bidi_local(64 * 1024 * 8);
    config.set_initial_max_stream_data_bidi_remote(64 * 1024 * 8);
    config.set_initial_max_stream_data_uni(64 * 1024 * 8);

    let h3_config = Some(quiche::h3::Config::new().unwrap());

    let udp = TokioUdpFactory::new(([127, 0, 0, 1], 0).into(), None);
    let fact = <QuicheEpFactory<TokioRt, TokioUdpFactory>>::new(config, h3_config, udp);
    let fact = DynEpFactory::new(fact);
    let (ep, recv) = fact.bind().await.unwrap();
    let addr = ep.addr().await.unwrap();
    (ep, recv, addr)
}

fn handle_con_recv(mut recv: DynConRecv) {
    TokioRt::spawn(async move {
        while let Some(evt) = recv.recv().await {
            match evt {
                ConEvt::Connected => {
                    tracing::info!("[TEST] con connected");
                }
                ConEvt::Http3(_m) => {
                    tracing::info!("[TEST] incoming http3");
                }
            }
        }
    });
}

fn handle_ep_recv(mut recv: DynEpRecv) {
    TokioRt::spawn(async move {
        while let Some(evt) = recv.recv().await {
            match evt {
                EpEvt::InCon(_con, con_recv) => {
                    handle_con_recv(con_recv);
                }
            }
        }
    });
}

fn handle_ep(ep: DynEp, dest: SocketAddr) {
    TokioRt::spawn(async move {
        let (_con, con_recv) = ep.connect(dest).await.unwrap();
        handle_con_recv(con_recv);
    });
}

#[tokio::test(flavor = "multi_thread")]
async fn sanity() {
    let subscriber = tracing_subscriber::FmtSubscriber::builder()
        .with_env_filter(
            tracing_subscriber::filter::EnvFilter::from_default_env(),
        )
        .with_file(true)
        .with_line_number(true)
        .without_time()
        .finish();
    tracing::subscriber::set_global_default(subscriber).unwrap();

    let (ep1, recv1, addr1) = bind().await;
    tracing::info!(?addr1, "[TEST]");
    let (ep2, recv2, addr2) = bind().await;
    tracing::info!(?addr2, "[TEST]");

    handle_ep_recv(recv1);
    handle_ep_recv(recv2);

    handle_ep(ep1, addr2);
    handle_ep(ep2, addr1);

    TokioRt::sleep(
        std::time::Instant::now() + std::time::Duration::from_secs(5),
    )
    .await;
}
