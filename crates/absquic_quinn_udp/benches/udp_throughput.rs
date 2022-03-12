use criterion::{BenchmarkId, criterion_group, criterion_main, Criterion};
use std::sync::Arc;
use std::net::SocketAddr;
use absquic_core::deps::parking_lot::Mutex;
use absquic_core::backend::*;
use absquic_core::backend::util::*;
use absquic_quinn_udp::*;

struct TestInner {
    data: Box<[u8]>,
    _addr1: SocketAddr,
    send1: UdpTestSend,
    _recv1: UdpTestRecv,
    addr2: SocketAddr,
    _send2: UdpTestSend,
    recv2: UdpTestRecv,
}

impl TestInner {
    async fn test(&mut self) {
        self.send1.send(OutUdpPacket {
            dst_addr: self.addr2,
            ecn: None,
            data: self.data.to_vec(),
            segment_size: None,
            src_ip: None,
        }).unwrap();
        println!("about to recv");
        let res = self.recv2.recv().await.unwrap();
        println!("done recv");
        assert_eq!(res.data.as_ref(), &*self.data);
    }
}

type TestCore = Arc<Mutex<Option<TestInner>>>;

struct Test(TestCore);

impl Test {
    async fn new(size: usize) -> Self {
        let data = vec![0xdb; size].into_boxed_slice();
        let socket1 = QuinnUdpBackendFactory::new(([127, 0, 0, 1], 0).into(), None);
        let socket1 = socket1.bind().await.unwrap();
        let _addr1 = socket1.local_addr().unwrap();
        let (send1, _recv1, driver1, _shutdown1) = udp_test(socket1);
        tokio::task::spawn(driver1);
        let socket2 = QuinnUdpBackendFactory::new(([127, 0, 0, 1], 0).into(), None);
        let socket2 = socket2.bind().await.unwrap();
        let addr2 = socket2.local_addr().unwrap();
        let (_send2, recv2, driver2, _shutdown2) = udp_test(socket2);
        tokio::task::spawn(driver2);
        Self(Arc::new(Mutex::new(Some(TestInner {
            data,
            _addr1,
            send1,
            _recv1,
            addr2,
            _send2,
            recv2,
        }))))
    }

    async fn test(&self) {
        let mut test = self.0.lock().take();
        test.as_mut().unwrap().test().await;
        *self.0.lock() = test;
    }
}

fn criterion_benchmark(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap();
    let rt = &rt;

    //let size: usize = 60 * 1024;
    let size: usize = 1024;
    let test = rt.block_on(Test::new(size));
    let test = &test;
    rt.block_on(test.test());
    println!("yay");
    rt.block_on(test.test());
    println!("yay2");
    rt.block_on(test.test());
    println!("yay3");
    rt.block_on(test.test());
    println!("yay4");
    c.bench_with_input(BenchmarkId::new("aq_udp_thru", size), &(), move |b, _| {
        b.to_async(rt).iter(|| test.test());
    });
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);
