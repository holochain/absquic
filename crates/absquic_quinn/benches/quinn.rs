use absquic_core::connection::*;
use absquic_core::deps::bytes;
use absquic_core::deps::parking_lot::Mutex;
use absquic_core::endpoint::*;
use absquic_core::runtime::*;
use absquic_core::stream::*;
use absquic_quinn::dev_utils;
use criterion::*;
use std::net::SocketAddr;
use std::sync::Arc;

enum MetaSend {
    Quinn(quinn::SendStream),
    Abs(WriteStream),
}

impl MetaSend {
    pub async fn write_to_end(self, data: &[u8]) {
        match self {
            MetaSend::Quinn(mut s) => {
                s.write_all(data).await.unwrap();
                s.finish().await.unwrap();
            }
            MetaSend::Abs(mut s) => {
                s.write_bytes_all(&mut bytes::Bytes::copy_from_slice(data))
                    .await
                    .unwrap();
            }
        }
    }
}

enum MetaRecv {
    Quinn(quinn::RecvStream),
    Abs(ReadStream),
}

impl MetaRecv {
    pub async fn read_to_end(self) -> Vec<u8> {
        match self {
            MetaRecv::Quinn(r) => r.read_to_end(usize::MAX).await.unwrap(),
            MetaRecv::Abs(r) => {
                use bytes::Buf;
                use std::io::Read;
                let r = r.read_to_end(usize::MAX).await.unwrap();
                let mut v = Vec::with_capacity(r.remaining());
                r.reader().read_to_end(&mut v).unwrap();
                v
            }
        }
    }
}

enum MetaCon {
    Quinn(quinn::Connection),
    Abs(Connection),
}

impl MetaCon {
    pub async fn open_uni(&mut self) -> MetaSend {
        match self {
            MetaCon::Quinn(con) => {
                MetaSend::Quinn(con.open_uni().await.unwrap())
            }
            MetaCon::Abs(con) => {
                MetaSend::Abs(con.open_uni_stream().await.unwrap())
            }
        }
    }
}

enum MetaInUni {
    Quinn(quinn::IncomingUniStreams),
    Abs(MultiReceiver<ConnectionEvt>),
}

impl MetaInUni {
    pub async fn recv(&mut self) -> MetaRecv {
        match self {
            MetaInUni::Quinn(ref mut i) => {
                use futures::stream::StreamExt;
                MetaRecv::Quinn(i.next().await.unwrap().unwrap())
            }
            MetaInUni::Abs(ref mut i) => match i.recv().await.unwrap() {
                absquic_core::connection::ConnectionEvt::InUniStream(s) => {
                    MetaRecv::Abs(s)
                }
                _ => panic!(),
            },
        }
    }
}

enum MetaEndpoint {
    Quinn(quinn::Endpoint),
    Abs(Endpoint),
}

impl MetaEndpoint {
    pub async fn connect(&mut self, addr: SocketAddr) -> (MetaCon, MetaInUni) {
        match self {
            MetaEndpoint::Quinn(ep) => {
                let quinn::NewConnection {
                    connection,
                    uni_streams,
                    ..
                } = ep.connect(addr, "localhost").unwrap().await.unwrap();
                (MetaCon::Quinn(connection), MetaInUni::Quinn(uni_streams))
            }
            MetaEndpoint::Abs(ep) => {
                let (con, recv) = ep.connect(addr, "localhost").await.unwrap();
                let recv = recv.wait_connected().await.unwrap();
                (MetaCon::Abs(con), MetaInUni::Abs(recv))
            }
        }
    }
}

enum MetaInCon {
    Quinn(quinn::Incoming),
    Abs(MultiReceiver<EndpointEvt>),
}

impl MetaInCon {
    pub async fn recv(&mut self) -> (MetaCon, MetaInUni) {
        match self {
            MetaInCon::Quinn(ref mut i) => {
                use futures::stream::StreamExt;
                let quinn::NewConnection {
                    connection,
                    uni_streams,
                    ..
                } = i.next().await.unwrap().await.unwrap();
                (MetaCon::Quinn(connection), MetaInUni::Quinn(uni_streams))
            }
            MetaInCon::Abs(i) => match i.recv().await.unwrap() {
                absquic_core::endpoint::EndpointEvt::InConnection(
                    con,
                    recv,
                ) => {
                    let recv = recv.wait_connected().await.unwrap();
                    (MetaCon::Abs(con), MetaInUni::Abs(recv))
                }
                _ => panic!(),
            },
        }
    }
}

async fn bind_ep(q: bool) -> (MetaEndpoint, MetaInCon, SocketAddr) {
    let (cert, pk) = dev_utils::localhost_self_signed_tls_cert();
    let srv = dev_utils::simple_server_config(cert, pk);
    let cli = dev_utils::trusting_client_config();
    if q {
        let (mut ep, recv) =
            quinn::Endpoint::server(srv, ([127, 0, 0, 1], 0).into()).unwrap();
        ep.set_default_client_config(cli);
        let addr = ep.local_addr().unwrap();
        (MetaEndpoint::Quinn(ep), MetaInCon::Quinn(recv), addr)
    } else {
        let (udp_factory, gso) = absquic_quinn_udp::QuinnUdpBackendFactory::new(
            ([127, 0, 0, 1], 0).into(),
            None,
        );

        let quic_factory = absquic_quinn::QuinnQuicBackendFactory::new(
            gso,
            quinn_proto::EndpointConfig::default(),
            Some(srv),
            cli,
        );

        let (ep, recv) =
            absquic_core::tokio_runtime::TokioRuntime::build_endpoint(
                udp_factory,
                quic_factory,
            )
            .await
            .unwrap();

        let addr = ep.local_address().await.unwrap();
        (MetaEndpoint::Abs(ep), MetaInCon::Abs(recv), addr)
    }
}

struct Test {
    ep1: MetaEndpoint,
    _recv1: MetaInCon,
    _ep2: MetaEndpoint,
    recv2: MetaInCon,
    addr2: SocketAddr,
    data: Arc<Vec<u8>>,
}

impl Test {
    pub async fn new(size: usize, q: bool) -> Self {
        let (ep1, _recv1, _addr1) = bind_ep(q).await;
        let (_ep2, recv2, addr2) = bind_ep(q).await;

        let mut data = vec![0xdb; size];
        use ring::rand::SecureRandom;
        ring::rand::SystemRandom::new().fill(&mut data).unwrap();
        let data = Arc::new(data);

        Test {
            ep1,
            _recv1,
            _ep2,
            recv2,
            addr2,
            data,
        }
    }

    pub async fn test(self) -> Self {
        let Test {
            mut ep1,
            _recv1,
            _ep2,
            mut recv2,
            addr2,
            data,
        } = self;

        let data_ref = data.clone();
        let ep1_task = tokio::task::spawn(async move {
            let (mut con, _) = ep1.connect(addr2).await;

            let uni = con.open_uni().await;
            uni.write_to_end(data_ref.as_slice()).await;

            drop(con);
            ep1
        });

        let data_ref = data.clone();
        let recv2_task = tokio::task::spawn(async move {
            let (_, mut uni_streams) = recv2.recv().await;

            let uni = uni_streams.recv().await;
            let data = uni.read_to_end().await;

            assert_eq!(data_ref.len(), data.len());
            assert_eq!(data_ref.as_slice(), data.as_slice());

            drop(uni_streams);
            recv2
        });

        let ep1 = ep1_task.await.unwrap();
        let recv2 = recv2_task.await.unwrap();

        Test {
            ep1,
            _recv1,
            _ep2,
            recv2,
            addr2,
            data,
        }
    }
}

async fn bench(b: &Arc<Mutex<Option<Test>>>) {
    let inner = b.lock().take().unwrap();
    let bench = inner.test().await;
    *b.lock() = Some(bench);
}

fn criterion_benchmark(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();

    static KB: usize = 1024;
    let size_list = [
        // should fit in one datagram even with some quic overhead
        60 * KB,
        // test with a larger amount of data
        200 * KB,
    ];

    let mut absquic = c.benchmark_group("absquic");
    for size in size_list.iter() {
        absquic.throughput(Throughput::Bytes(*size as u64));
        absquic.bench_with_input(
            BenchmarkId::from_parameter(size),
            size,
            |b, &size| {
                let test = rt.block_on(Test::new(size, false));
                let test = Arc::new(Mutex::new(Some(test)));
                b.to_async(&rt).iter(|| bench(&test))
            },
        );
    }
    absquic.finish();

    let mut quinn = c.benchmark_group("quinn");
    for size in size_list.iter() {
        quinn.throughput(Throughput::Bytes(*size as u64));
        quinn.bench_with_input(
            BenchmarkId::from_parameter(size),
            size,
            |b, &size| {
                let test = rt.block_on(Test::new(size, true));
                let test = Arc::new(Mutex::new(Some(test)));
                b.to_async(&rt).iter(|| bench(&test))
            },
        );
    }
    quinn.finish();
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);
