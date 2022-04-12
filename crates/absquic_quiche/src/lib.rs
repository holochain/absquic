#![deny(missing_docs)]
#![deny(warnings)]
#![deny(unsafe_code)]
//! Absquic quiche quic/http3 backend implementation

pub mod ep_factory;

/*
use std::collections::HashMap;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::atomic;
use std::sync::Arc;

fn cid() -> quiche::ConnectionId<'static> {
    static CID: atomic::AtomicU64 = atomic::AtomicU64::new(0);
    let id = CID.fetch_add(1, atomic::Ordering::Relaxed);
    let id = id.to_le_bytes();
    let mut id = id.to_vec();
    id.extend_from_slice(&[0xdb; 8]);
    quiche::ConnectionId::from_vec(id.to_vec())
}

type OnceSender<T> = tokio::sync::oneshot::Sender<T>;
//type OnceReceiver<T> = tokio::sync::oneshot::Receiver<T>;
type Sender<T> = tokio::sync::mpsc::UnboundedSender<T>;
type Receiver<T> = tokio::sync::mpsc::UnboundedReceiver<T>;

/// ConEvt
pub enum ConEvt {
    /// Connected
    Connected,
}

enum ConCmd {
    Init,
    Timeout,
    PakRecv(Vec<u8>, SocketAddr),
}

/// Con
pub struct Con {
    #[allow(dead_code)]
    con_send: Sender<ConCmd>,
}

impl Con {
    fn new(
        ep_send: Sender<EpCmd>,
        con: Pin<Box<quiche::Connection>>,
    ) -> (Self, Sender<ConCmd>, Receiver<ConEvt>) {
        let (con_send, con_recv) = tokio::sync::mpsc::unbounded_channel();
        let (con_evt_send, con_evt_recv) = tokio::sync::mpsc::unbounded_channel();
        let _ = con_send.send(ConCmd::Init);
        tokio::task::spawn(con_task(ep_send, con_send.clone(), con_evt_send, con_recv, con));
        (
            Con {
                con_send: con_send.clone(),
            },
            con_send,
            con_evt_recv,
        )
    }
}

async fn con_task(
    ep_send: Sender<EpCmd>,
    con_send: Sender<ConCmd>,
    con_evt_send: Sender<ConEvt>,
    mut con_recv: Receiver<ConCmd>,
    mut con: Pin<Box<quiche::Connection>>,
) {
    let mut buf = [0_u8; 64 * 1024];
    let mut last_timeout = None;
    let mut connected = false;

    while let Some(cmd) = con_recv.recv().await {
        #[allow(unused_assignments)]
        let mut should_send = false;

        match cmd {
            ConCmd::Init => {
                should_send = true;
            }
            ConCmd::Timeout => {
                con.on_timeout();
                last_timeout = None;
                should_send = true;
            }
            ConCmd::PakRecv(mut data, addr) => {
                con.recv(&mut data[..], quiche::RecvInfo { from: addr })
                    .unwrap();
                should_send = true;
            }
        }

        if let Some(timeout) = con.timeout() {
            let timeout = std::time::Instant::now() + timeout;
            let mut set = false;
            if last_timeout.is_none() {
                set = true;
            } else {
                if let Some(lt) = last_timeout {
                    if timeout < lt {
                        set = true;
                    }
                }
            }
            if set {
                last_timeout = Some(timeout);
                let con_send = con_send.clone();
                tokio::task::spawn(async move {
                    tokio::time::sleep_until(timeout.into()).await;
                    let _ = con_send.send(ConCmd::Timeout);
                });
            }
        }

        if should_send {
            loop {
                match con.send(&mut buf[..]) {
                    Err(quiche::Error::Done) => break,
                    Err(err) => panic!("{:?}", err),
                    Ok((len, info)) => {
                        let _ = ep_send.send(EpCmd::PakSend(
                            buf[..len].to_vec(),
                            info.to,
                            info.at,
                        ));
                    }
                }
            }
        }

        println!(
            "is_established {} {:?}",
            con.is_established(),
            con.source_id(),
        );

        if !connected && con.is_established() {
            connected = true;
            let _ = con_evt_send.send(ConEvt::Connected);
        }
    }
}

/// EpEvt
pub enum EpEvt {
    /// InCon
    InCon(Con, Receiver<ConEvt>),
}

enum EpCmd {
    PakRecv(Vec<u8>, SocketAddr),
    PakSend(Vec<u8>, SocketAddr, std::time::Instant),
    LocAddr(OnceSender<SocketAddr>),
    Con(SocketAddr, OnceSender<(Con, Receiver<ConEvt>)>),
}

/// Ep
pub struct Ep {
    ep_send: Sender<EpCmd>,
}

impl Ep {
    /// bind
    pub async fn bind(addr: &str) -> (Self, Receiver<EpEvt>) {
        let (ep_send, ep_recv) = tokio::sync::mpsc::unbounded_channel();
        let (ep_evt_send, ep_evt_recv) = tokio::sync::mpsc::unbounded_channel();
        let sock = tokio::net::UdpSocket::bind(addr).await.unwrap();
        let sock = Arc::new(sock);
        tokio::task::spawn(ep_socket_recv_task(ep_send.clone(), sock.clone()));
        tokio::task::spawn(ep_task(
            ep_send.clone(),
            ep_evt_send,
            ep_recv,
            sock,
        ));
        (Ep { ep_send }, ep_evt_recv)
    }

    /// addr
    pub async fn addr(&self) -> SocketAddr {
        let (s, r) = tokio::sync::oneshot::channel();
        let _ = self.ep_send.send(EpCmd::LocAddr(s));
        r.await.unwrap()
    }

    /// con
    pub async fn con(&self, addr: SocketAddr) -> (Con, Receiver<ConEvt>) {
        let (s, r) = tokio::sync::oneshot::channel();
        let _ = self.ep_send.send(EpCmd::Con(addr, s));
        r.await.unwrap()
    }
}

async fn ep_task(
    ep_send: Sender<EpCmd>,
    ep_evt_send: Sender<EpEvt>,
    mut ep_recv: Receiver<EpCmd>,
    sock: Arc<tokio::net::UdpSocket>,
) {
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

    let mut con_map: HashMap<quiche::ConnectionId<'static>, Sender<ConCmd>> =
        HashMap::new();

    while let Some(cmd) = ep_recv.recv().await {
        match cmd {
            EpCmd::PakRecv(mut data, addr) => {
                let hdr =
                    quiche::Header::from_slice(data.as_mut_slice(), 16).unwrap();
                println!("hdr: {:?}", hdr);
                if let Some(con_send) = con_map.get(&hdr.dcid) {
                    println!("in pak routed");
                    let _ = con_send.send(ConCmd::PakRecv(data, addr));
                } else if hdr.ty == quiche::Type::Initial {
                    let scid = if hdr.dcid.len() == 16 {
                        hdr.dcid
                    } else {
                        cid()
                    };
                    println!("new in con {:?}", scid);
                    let con =
                        quiche::accept(&scid, None, addr, &mut config).unwrap();
                    let (con, con_send, con_evt_recv) = Con::new(ep_send.clone(), con);
                    let _ = con_send.send(ConCmd::PakRecv(data, addr));
                    con_map.insert(scid, con_send);
                    let _ = ep_evt_send.send(EpEvt::InCon(con, con_evt_recv));
                }
            }
            EpCmd::PakSend(data, addr, at) => {
                let sock = sock.clone();
                tokio::task::spawn(async move {
                    tokio::time::sleep_until(at.into()).await;
                    sock.send_to(&data, addr).await.unwrap();
                    println!("sent packet");
                });
            }
            EpCmd::LocAddr(rsp) => {
                let _ = rsp.send(sock.local_addr().unwrap());
            }
            EpCmd::Con(addr, rsp) => {
                let scid = cid();
                println!("new out con {:?}", scid);
                let con =
                    quiche::connect(None, &scid, addr, &mut config).unwrap();
                let (con, con_send, con_evt_recv) = Con::new(ep_send.clone(), con);
                con_map.insert(scid, con_send);
                let _ = rsp.send((con, con_evt_recv));
            }
        }
    }
}

async fn ep_socket_recv_task(
    ep_send: Sender<EpCmd>,
    sock: Arc<tokio::net::UdpSocket>,
) {
    let mut buf = [0_u8; 64 * 1024];
    loop {
        match sock.recv_from(&mut buf[..]).await {
            Ok((len, addr)) => {
                if let Err(_) =
                    ep_send.send(EpCmd::PakRecv(buf[..len].to_vec(), addr))
                {
                    panic!("stop udp recv");
                }
            }
            Err(err) => panic!("{:?}", err),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s_con_recv(mut con_evt_recv: Receiver<ConEvt>) {
        tokio::task::spawn(async move {
            while let Some(evt) = con_evt_recv.recv().await {
                match evt {
                    ConEvt::Connected => {
                        println!("!!!! CONNECTED !!!!");
                    }
                }
            }
        });
    }

    fn s_ep(ep: Ep, addr: SocketAddr) {
        tokio::task::spawn(async move {
            let (_con, con_evt_recv) = ep.con(addr).await;
            s_con_recv(con_evt_recv);
        });
    }

    fn s_ep_recv(mut ep_evt_recv: Receiver<EpEvt>) {
        tokio::task::spawn(async move {
            while let Some(evt) = ep_evt_recv.recv().await {
                match evt {
                    EpEvt::InCon(_, con_evt_recv) => {
                        println!("in con!");
                        s_con_recv(con_evt_recv);
                    }
                }
            }
        });
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn sanity() {
        let (c1, r1) = Ep::bind("127.0.0.1:0").await;
        let a1 = c1.addr().await;
        println!("a1: {:?}", a1);

        let (c2, r2) = Ep::bind("127.0.0.1:0").await;
        let a2 = c2.addr().await;
        println!("a2: {:?}", a2);

        s_ep_recv(r1);
        s_ep_recv(r2);

        s_ep(c1, a2);
        s_ep(c2, a1);

        tokio::time::sleep(std::time::Duration::from_secs(5)).await;
    }
}
*/
