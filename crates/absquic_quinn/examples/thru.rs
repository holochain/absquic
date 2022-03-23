use absquic_core::endpoint::*;
use absquic_quinn::*;
use absquic_quinn_udp::*;
use std::sync::Arc;

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    let subscriber = tracing_subscriber::FmtSubscriber::builder()
        .with_env_filter(
            tracing_subscriber::filter::EnvFilter::from_default_env(),
        )
        .compact()
        .without_time()
        .finish();
    tracing::subscriber::set_global_default(subscriber).unwrap();

    let cert =
        rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let pk = rustls::PrivateKey(cert.serialize_private_key_der());
    let cert = rustls::Certificate(cert.serialize_der().unwrap());
    let endpoint_config = quinn_proto::EndpointConfig::default();
    let server_config =
        quinn_proto::ServerConfig::with_single_cert(vec![cert], pk).unwrap();
    let client_config = rustls::ClientConfig::builder()
        .with_safe_defaults()
        .with_custom_certificate_verifier(Arc::new(V))
        .with_no_client_auth();
    let client_config = quinn_proto::ClientConfig::new(Arc::new(client_config));

    let udp_factory =
        QuinnUdpBackendFactory::new(([127, 0, 0, 1], 0).into(), None);

    let quic_factory = QuinnDriverFactory::new(
        endpoint_config,
        Some(server_config),
        client_config,
    );

    let ep_factory = EndpointFactory::new(udp_factory, quic_factory);

    let (mut ep1, mut rc1, d1) = ep_factory.bind(T::default()).await.unwrap();
    tokio::task::spawn(d1);
    tokio::task::spawn(async move {
        while let Some(evt) = rc1.recv().await {
            match evt {
                EndpointEvt::Error(e) => panic!("{:?}", e),
                EndpointEvt::InConnection(_, _) => panic!("unexpected con"),
            }
        }
        tracing::warn!("rc1 loop ending");
    });
    println!("{:?}", ep1.local_address().await.unwrap());

    let (mut ep2, mut rc2, d2) = ep_factory.bind(T::default()).await.unwrap();
    tokio::task::spawn(d2);
    tokio::task::spawn(async move {
        while let Some(evt) = rc2.recv().await {
            match evt {
                EndpointEvt::Error(e) => panic!("{:?}", e),
                EndpointEvt::InConnection(_con, recv) => {
                    tracing::warn!("got in connection");
                    tokio::task::spawn(async move {
                        let _con = _con;
                        let mut recv = recv.wait_connected().await.unwrap();
                        tracing::warn!("in connection connected");
                        while let Some(_evt) = recv.recv().await {
                            panic!("GOT EVENT");
                        }
                    });
                }
            }
        }
        tracing::warn!("rc2 loop ending");
    });
    let addr2 = ep2.local_address().await.unwrap();
    println!("{:?}", addr2);

    for _ in 0..10 {
        let (_con, recv) = ep1.connect(addr2, "noname").await.unwrap();
        tracing::warn!("got out connection");
        tokio::task::spawn(async move {
            let _con = _con;
            let mut recv = recv.wait_connected().await.unwrap();
            tracing::warn!("out connection connected");
            while let Some(_evt) = recv.recv().await {
                panic!("GOT EVENT");
            }
        });
    }

    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
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

struct V;

impl rustls::client::ServerCertVerifier for V {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::Certificate,
        _intermediates: &[rustls::Certificate],
        _server_name: &rustls::ServerName,
        _scts: &mut dyn Iterator<Item = &[u8]>,
        _ocsp_response: &[u8],
        _now: std::time::SystemTime,
    ) -> Result<rustls::client::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::ServerCertVerified::assertion())
    }
}
