#![cfg(not(loom))]

use absquic_core::connection::*;
use absquic_core::deps::bytes;
use absquic_core::endpoint::*;
use absquic_quinn::*;
use absquic_quinn_udp::*;
use std::net::SocketAddr;

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

    let endpoint_config = quinn_proto::EndpointConfig::default();
    let (cert, pk) = dev_utils::local_ephem_tls_cert();
    let server_config = dev_utils::simple_server_config(cert, pk);
    let client_config = dev_utils::trusting_client_config();

    let udp_factory =
        QuinnUdpBackendFactory::new(([127, 0, 0, 1], 0).into(), None);

    let quic_factory = QuinnDriverFactory::new(
        endpoint_config,
        Some(server_config),
        client_config,
    );

    let ep_factory = EndpointFactory::new(udp_factory, quic_factory);

    let (mut ep1, rc1, d1) = ep_factory.bind(T::default()).await.unwrap();
    tokio::task::spawn(d1);
    let addr1 = ep1.local_address().await.unwrap();
    println!("addr1: {:?}", addr1);

    let (mut ep2, rc2, d2) = ep_factory.bind(T::default()).await.unwrap();
    tokio::task::spawn(d2);
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

async fn run_ep_hnd(name: String, mut ep: Endpoint, oth_addr: SocketAddr) {
    let mut all = Vec::new();
    for _ in 0..10 {
        let (con, rcv) = ep.connect(oth_addr, "localhost").await.unwrap();
        all.push(tokio::task::spawn(run_con(
            format!("{}:out_con", name),
            con,
            rcv,
        )));
    }
    futures::future::try_join_all(all).await.unwrap();
    tracing::warn!(%name, "connect loop ending");
}

async fn run_ep_rcv(name: String, mut rcv: EndpointRecv) {
    let mut all = Vec::new();
    while let Some(evt) = rcv.recv().await {
        match evt {
            EndpointEvt::Error(e) => panic!("{:?}", e),
            EndpointEvt::InConnection(con, rcv) => {
                all.push(tokio::task::spawn(run_con(
                    format!("{}:in_con", name),
                    con,
                    rcv,
                )));
                if all.len() == 10 {
                    break;
                }
            }
        }
    }
    if all.len() != 10 {
        panic!("expected 10 connections, got {}", all.len());
    }
    futures::future::try_join_all(all).await.unwrap();
    tracing::warn!(%name, "recv loop ending");
}

static WRITE_COUNT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);
static READ_COUNT: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

async fn run_con(name: String, mut con: Connection, rcv: ConnectionRecv) {
    let mut rcv = rcv.wait_connected().await.unwrap();

    let mut all = Vec::new();

    let recv_name = format!("{}:recv_loop", name);

    all.push(tokio::task::spawn(async move {
        let mut all = Vec::new();
        while let Some(evt) = rcv.recv().await {
            match evt {
                ConnectionEvt::Error(e) => panic!("{:?}", e),
                ConnectionEvt::InUniStream(mut r) => {
                    all.push(tokio::task::spawn(async move {
                        let mut full_buf = bytes::BytesMut::new();
                        while let Some(bytes) = r.read_bytes(usize::MAX).await {
                            let bytes = bytes.unwrap();
                            full_buf.extend_from_slice(bytes.as_ref());
                        }
                        assert_eq!(full_buf.as_ref(), b"hello");
                        tracing::info!(
                            "READ SUCCESS {}",
                            READ_COUNT.fetch_add(
                                1,
                                std::sync::atomic::Ordering::SeqCst
                            ) + 1
                        );
                    }));
                    if all.len() == 10 {
                        break;
                    }
                }
                _ => (),
            }
        }
        if all.len() != 10 {
            panic!("expected 10 in streams, got {}", all.len());
        }
        futures::future::try_join_all(all).await.unwrap();
        tracing::info!(%recv_name, "rcv loop ending");
    }));

    for _ in 0..10 {
        let mut stream = con.open_uni_stream().await.unwrap();
        all.push(tokio::task::spawn(async move {
            let mut data = (&b"hello"[..]).into();
            stream.write_bytes_all(&mut data).await.unwrap();
            stream.stop(0).await.unwrap();
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

struct T(Option<tokio::task::JoinHandle<()>>);

impl Default for T {
    fn default() -> Self {
        Self(None)
    }
}

impl TimeoutsScheduler for T {
    fn schedule(
        &mut self,
        logic: Box<dyn FnOnce() + 'static + Send>,
        at: std::time::Instant,
    ) {
        if let Some(t) = self.0.take() {
            t.abort();
        }
        self.0 = Some(tokio::task::spawn(async move {
            tokio::time::sleep_until(at.into()).await;
            logic();
        }));
    }
}
