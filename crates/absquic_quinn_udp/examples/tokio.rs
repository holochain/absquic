use std::sync::atomic;
use std::sync::Arc;

struct SockInner {
    data: Box<[u8]>,
    sock_send: tokio::net::UdpSocket,
    sock_recv: tokio::net::UdpSocket,
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
    let sock_send =
        tokio::net::UdpSocket::bind(("127.0.0.1", 0)).await.unwrap();
    let sock_recv =
        tokio::net::UdpSocket::bind(("127.0.0.1", 0)).await.unwrap();
    let recv_addr = sock_recv.local_addr().unwrap();
    Arc::new(SockInner {
        data,
        sock_send,
        sock_recv,
        recv_addr,
        cont: atomic::AtomicBool::new(true),
    })
}

async fn send_task(sock: Sock) -> usize {
    let mut send_count = 0;

    let start = std::time::Instant::now();

    while sock.get_cont() {
        // the other side has to do a to_vec... do that here too
        let data = sock.data.to_vec();
        let wrote = sock
            .sock_send
            .send_to(&data, &sock.recv_addr)
            .await
            .unwrap();
        assert_eq!(wrote, sock.data.len());

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

async fn recv_task(sock: Sock) -> usize {
    let mut buf = vec![0; 64 * 1024];
    let mut recv_count = 0;

    while let Ok(Ok((size, _))) = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        sock.sock_recv.recv_from(&mut buf),
    )
    .await
    {
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
}

#[tokio::main(flavor = "multi_thread")]
async fn main() {
    println!("gen_sock");
    let sock = gen_sock(20 * 1024).await;
    println!("spawn tasks");
    let ts = tokio::task::spawn(send_task(sock.clone()));
    let tr = tokio::task::spawn(recv_task(sock.clone()));
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
