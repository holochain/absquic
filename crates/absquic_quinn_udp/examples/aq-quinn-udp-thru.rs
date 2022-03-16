/*
use absquic_quinn_udp::*;
use absquic_core::backend::*;
use absquic_core::backend::util::*;
use std::sync::Arc;
use std::sync::atomic;

struct SockInner {
    data: Box<[u8]>,
    send1: UdpTestSend,
    _recv1: UdpTestRecv,
    _send2: UdpTestSend,
    recv2: UdpTestRecv,
    recv_addr: std::net::SocketAddr,
    cont: atomic::AtomicBool,
}

impl SockInner {
    fn get_cont(&self) -> bool {
        self.cont.load(atomic::Ordering::SeqCst)
    }

    fn stop(&self) {
        self.cont.store(false, atomic::Ordering::SeqCst);
    }
}

type Sock = Arc<SockInner>;

async fn gen_sock(len: usize) -> Sock {
    let data = vec![0xdb; len].into_boxed_slice();

    let s1 = QuinnUdpBackendFactory::new(([127, 0, 0, 1], 0).into(), None);
    let s1 = s1.bind().await.unwrap();
    let (send1, recv1, driver1, _shutdown1) = udp_test(s1);

    let s2 = QuinnUdpBackendFactory::new(([127, 0, 0, 1], 0).into(), None);
    let s2 = s2.bind().await.unwrap();
    let recv_addr = s2.local_addr().unwrap();
    let (send2, recv2, driver2, _shutdown2) = udp_test(s2);

    Arc::new(SockInner {
        data,
        send1,
        _recv1: recv1,
        _send2: send2,
        recv2,
        recv_addr,
        cont: atomic::AtomicBool::new(true),
    })
}

async fn send_task(_sock: Sock) -> usize {
    /*
    let mut send_count = 0;

    while sock.get_cont() {
        let wrote = sock.sock_send.send_to(&sock.data, &sock.recv_addr).await.unwrap();
        assert_eq!(wrote, sock.data.len());

        send_count += 1;
        if send_count % 100 == 0 {
            tokio::task::yield_now().await;
        }
    }

    send_count
    */
    0
}

async fn recv_task(_sock: Sock) -> usize {
    /*
    let mut buf = vec![0; 64 * 1024];
    let mut recv_count = 0;

    while let Ok((size, _)) = sock.sock_recv.recv_from(&mut buf).await {
        let data = &buf[0..size];
        assert_eq!(data, &*sock.data);

        recv_count += 1;
        if recv_count % 100 == 0 {
            tokio::task::yield_now().await;
        }

        if !sock.get_cont() {
            break;
        }
    }

    recv_count
    */
    0
}
*/

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    /*
        println!("gen_sock");
        let sock = gen_sock(60 * 1024).await;
        println!("spawn tasks");
        let ts = tokio::task::spawn(send_task(sock.clone()));
        let tr = tokio::task::spawn(recv_task(sock.clone()));
        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
        println!("call stop");
        sock.stop();
        println!("await tasks");
        let send_count = ts.await.unwrap();
        let recv_count = tr.await.unwrap();
        println!("send_count: {}, recv_count: {}", send_count, recv_count);
        println!("{:0.2} % received", (recv_count as f64 / send_count as f64 * 100.0));
    */
}
