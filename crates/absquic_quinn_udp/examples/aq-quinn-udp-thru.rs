/*
use absquic_core::backend::*;
use absquic_quinn_udp::*;
use std::sync::atomic;
use std::sync::Arc;

struct SockInner {
    data: Box<[u8]>,
    recv_addr: std::net::SocketAddr,
    cont: atomic::AtomicBool,
}

struct SockSend {
    send1: DynUdpBackendSender,
    _send2: DynUdpBackendSender,
}

struct SockRecv {
    _recv1: DynUdpBackendReceiver,
    recv2: DynUdpBackendReceiver,
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

async fn gen_sock(len: usize) -> (Sock, SockSend, SockRecv) {
    let data = vec![0xdb; len].into_boxed_slice();

    let factory = QuinnUdpBackendFactory::new(([127, 0, 0, 1], 0).into(), None);

    let (s1, r1, d1) = factory.bind().await.unwrap();
    tokio::task::spawn(d1);
    let (mut s2, r2, d2) = factory.bind().await.unwrap();
    tokio::task::spawn(d2);

    let recv_addr = s2.local_addr().await.unwrap();

    (
        Arc::new(SockInner {
            data,
            recv_addr,
            cont: atomic::AtomicBool::new(true),
        }),
        SockSend {
            send1: s1,
            _send2: s2,
        },
        SockRecv {
            _recv1: r1,
            recv2: r2,
        },
    )
}

async fn send_task(sock: Sock, mut sock_send: SockSend) -> usize {
    let mut send_count = 0;

    let start = std::time::Instant::now();

    while sock.get_cont() {
        sock_send
            .send1
            .send(OutUdpPacket {
                dst_addr: sock.recv_addr,
                ecn: None,
                data: sock.data.to_vec(),
                segment_size: None,
                src_ip: None,
            })
            .await
            .unwrap();

        send_count += 1;
        if send_count % 10 == 0 {
            tokio::time::sleep(std::time::Duration::from_micros(100)).await;
        }
        if send_count % 100 == 0 {
            if start.elapsed().as_secs() >= 5 {
                println!("send done");
                break;
            }
            tokio::task::yield_now().await;
        }
    }

    send_count
}

async fn recv_task(sock: Sock, mut sock_recv: SockRecv) -> usize {
    let mut recv_count = 0;

    while let Ok(Some(packet)) = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        sock_recv.recv2.recv(),
    )
    .await
    {
        assert_eq!(packet.data, &*sock.data);

        recv_count += 1;
        if recv_count % 100 == 0 {
            tokio::task::yield_now().await;
        }

        if !sock.get_cont() {
            break;
        }
    }

    recv_count
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    println!("gen_sock");
    let (sock, sock_send, sock_recv) = gen_sock(20 * 1024).await;
    println!("spawn tasks");
    let ts = tokio::task::spawn(send_task(sock.clone(), sock_send));
    let tr = tokio::task::spawn(recv_task(sock.clone(), sock_recv));
    tokio::time::sleep(std::time::Duration::from_secs(10)).await;
    println!("call stop");
    sock.stop();
    println!("await tasks");
    let send_count = ts.await.unwrap();
    let recv_count = tr.await.unwrap();
    println!("send_count: {}, recv_count: {}", send_count, recv_count);
    println!(
        "{:0.2} % received",
        (recv_count as f64 / send_count as f64 * 100.0)
    );
}
*/

#[tokio::main(flavor = "multi_thread")]
async fn main() {}
