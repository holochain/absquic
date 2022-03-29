use absquic_core::deps::parking_lot::Mutex;
use absquic_quinn::dev_utils;
use criterion::*;
use std::net::SocketAddr;
use std::sync::Arc;

struct Test {
    ep1: quinn::Endpoint,
    _recv1: quinn::Incoming,
    _ep2: quinn::Endpoint,
    recv2: quinn::Incoming,
    addr2: SocketAddr,
    data: Arc<Vec<u8>>,
}

impl Test {
    pub async fn new(size: usize) -> Self {
        let (cert, pk) = dev_utils::localhost_self_signed_tls_cert();
        let srv = dev_utils::simple_server_config(cert, pk);
        let cli = dev_utils::trusting_client_config();
        let (mut ep1, _recv1) =
            quinn::Endpoint::server(srv, ([127, 0, 0, 1], 0).into()).unwrap();
        ep1.set_default_client_config(cli);

        let (cert, pk) = dev_utils::localhost_self_signed_tls_cert();
        let srv = dev_utils::simple_server_config(cert, pk);
        let cli = dev_utils::trusting_client_config();
        let (mut _ep2, recv2) =
            quinn::Endpoint::server(srv, ([127, 0, 0, 1], 0).into()).unwrap();
        _ep2.set_default_client_config(cli);
        let addr2 = _ep2.local_addr().unwrap();

        let data = Arc::new(vec![0xdb; size]);

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
            ep1,
            _recv1,
            _ep2,
            mut recv2,
            addr2,
            data,
        } = self;

        let data_ref = data.clone();
        let ep1_task = tokio::task::spawn(async move {
            let con = ep1.connect(addr2, "localhost").unwrap();
            let quinn::NewConnection { connection, .. } = con.await.unwrap();

            let mut uni = connection.open_uni().await.unwrap();
            uni.write_all(data_ref.as_slice()).await.unwrap();
            uni.finish().await.unwrap();

            drop(uni);
            drop(connection);
            ep1
        });

        let data_ref = data.clone();
        let recv2_task = tokio::task::spawn(async move {
            use futures::stream::StreamExt;

            let con = recv2.next().await.unwrap();
            let quinn::NewConnection {
                mut uni_streams, ..
            } = con.await.unwrap();

            let uni = uni_streams.next().await.unwrap().unwrap();
            let data = uni.read_to_end(usize::MAX).await.unwrap();

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

    let mut group = c.benchmark_group("quinn");

    static KB: usize = 1024;
    for size in [KB, 16 * KB, 64 * KB].iter() {
        group.throughput(Throughput::Bytes(*size as u64));
        group.bench_with_input(
            BenchmarkId::from_parameter(size),
            size,
            |b, &size| {
                let test = rt.block_on(Test::new(size));
                let test = Arc::new(Mutex::new(Some(test)));
                b.to_async(&rt).iter(|| bench(&test))
            },
        );
    }

    group.finish();
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);
