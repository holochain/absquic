#![cfg(not(loom))]

use absquic_core::connection::*;
use absquic_core::deps::bytes;
use absquic_core::endpoint::*;
use absquic_core::runtime::*;
use absquic_core::tokio_runtime::*;
use absquic_quinn::*;
use absquic_quinn_udp::*;
use std::net::SocketAddr;

const CNT: usize = 10;
const BYTES: &[u8] = &[0xdb; 200 * 1024];

async fn bind() -> (Endpoint, MultiReceiver<EndpointEvt>) {
    let endpoint_config = quinn_proto::EndpointConfig::default();
    let (cert, pk) = dev_utils::localhost_self_signed_tls_cert();
    let server_config = dev_utils::simple_server_config(cert, pk);
    let client_config = dev_utils::trusting_client_config();
    let (udp_factory, gso) =
        QuinnUdpBackendFactory::new(([127, 0, 0, 1], 0).into(), None);
    let quic_factory = QuinnQuicBackendFactory::new(
        gso,
        endpoint_config,
        Some(server_config),
        client_config,
    );
    TokioRuntime::build_endpoint(udp_factory, quic_factory)
        .await
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn thru() {
    let subscriber = tracing_subscriber::FmtSubscriber::builder()
        .with_env_filter(
            tracing_subscriber::filter::EnvFilter::from_default_env(),
        )
        .with_file(true)
        .with_line_number(true)
        .without_time()
        .finish();
    tracing::subscriber::set_global_default(subscriber).unwrap();

    let (ep1, rc1) = bind().await;
    let addr1 = ep1.local_address().await.unwrap();
    println!("addr1: {:?}", addr1);

    let (ep2, rc2) = bind().await;
    let addr2 = ep2.local_address().await.unwrap();
    println!("addr2: {:?}", addr2);

    let mut all = Vec::new();
    all.push(tokio::task::spawn(run_ep_rcv("ep1rcv".into(), rc1)));
    all.push(tokio::task::spawn(run_ep_rcv("ep2rcv".into(), rc2)));
    all.push(tokio::task::spawn(run_ep_hnd("ep1hnd".into(), ep1, addr2)));
    all.push(tokio::task::spawn(run_ep_hnd("ep2hnd".into(), ep2, addr1)));

    futures::future::try_join_all(all).await.unwrap();

    println!("complete");
}

async fn run_ep_hnd(name: String, ep: Endpoint, oth_addr: SocketAddr) {
    let mut all = Vec::new();
    for _ in 0..CNT {
        let (con, rcv) = ep.connect(oth_addr, "localhost").await.unwrap();
        tracing::debug!("outgoing connection");
        all.push(tokio::task::spawn(run_con(
            format!("{}:out_con", name),
            con,
            rcv,
        )));
    }
    futures::future::try_join_all(all).await.unwrap();
    tracing::warn!(%name, "connect loop ending");
}

async fn run_ep_rcv(name: String, mut rcv: MultiReceiver<EndpointEvt>) {
    let mut all = Vec::new();
    while let Some(evt) = rcv.recv().await {
        match evt {
            EndpointEvt::Error(e) => panic!("{:?}", e),
            EndpointEvt::InConnection(con, rcv) => {
                tracing::debug!("incoming connection");
                all.push(tokio::task::spawn(run_con(
                    format!("{}:in_con", name),
                    con,
                    rcv,
                )));
                if all.len() == CNT {
                    break;
                }
            }
        }
    }
    if all.len() != CNT {
        panic!("expected {} connections, got {}", CNT, all.len());
    }
    futures::future::try_join_all(all).await.unwrap();
    tracing::warn!(%name, "recv loop ending");
}

static WRITE_COUNT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
static READ_COUNT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

async fn run_con(
    name: String,
    con: Connection,
    rcv: MultiReceiver<ConnectionEvt>,
) {
    let mut rcv = rcv.wait_connected().await.unwrap();

    let mut all = Vec::new();

    let recv_name = format!("{}:recv_loop", name);

    all.push(tokio::task::spawn(async move {
        let mut all = Vec::new();
        while let Some(evt) = rcv.recv().await {
            match evt {
                ConnectionEvt::Error(e) => panic!("{:?}", e),
                ConnectionEvt::InUniStream(mut r) => {
                    tracing::debug!("incoming uni stream");
                    all.push(tokio::task::spawn(async move {
                        let mut full_buf = bytes::BytesMut::new();
                        while let Some(bytes) = r.read_bytes(usize::MAX).await {
                            let bytes = bytes.unwrap();
                            full_buf.extend_from_slice(bytes.as_ref());
                        }
                        assert_eq!(full_buf.len(), BYTES.len());
                        assert_eq!(full_buf.as_ref(), BYTES);
                        tracing::info!(
                            "READ SUCCESS {}",
                            READ_COUNT.fetch_add(
                                1,
                                std::sync::atomic::Ordering::SeqCst
                            ) + 1
                        );
                    }));
                    if all.len() == CNT {
                        break;
                    }
                }
                _ => (),
            }
        }
        if all.len() != CNT {
            panic!("expected {} in streams, got {}", CNT, all.len());
        }
        futures::future::try_join_all(all).await.unwrap();
        tracing::info!(%recv_name, "rcv loop ending");
    }));

    for _ in 0..CNT {
        tracing::debug!("b4 outgoing uni stream");
        let mut stream = con.open_uni_stream().await.unwrap();
        tracing::debug!("outgoing uni stream");
        all.push(tokio::task::spawn(async move {
            tracing::debug!("About to write data");
            let mut data = BYTES.into();
            stream.write_bytes_all(&mut data).await.unwrap();
            stream.stop(0);
            tracing::info!(
                "WRITE SUCCESS {}",
                WRITE_COUNT.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
                    + 1
            );
        }));
    }

    drop(con);

    futures::future::try_join_all(all).await.unwrap();
    tracing::info!(%name, "run_con COMPLETE");
}
