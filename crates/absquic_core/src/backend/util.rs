//! utilities related to backend traits, largely for testing

use super::*;
use std::sync::atomic;

struct UdpTestDriver {
    shutdown: Arc<atomic::AtomicBool>,
    want_close: bool,
    socket: Box<dyn UdpBackend>,
    in_recv: InChanReceiver<OutUdpPacket>,
    in_buf: VecDeque<OutUdpPacket>,
    out_send: OutChanSender<InUdpPacket>,
    out_buf: VecDeque<InUdpPacket>,
}

impl UdpTestDriver {
    fn poll_send(&mut self, cx: &mut Context<'_>) {
        let Self {
            want_close,
            socket,
            in_recv,
            in_buf,
            ..
        } = self;

        let mut should_close = false;
        in_recv.recv(&mut should_close, in_buf, *want_close);
        if should_close {
            *want_close = true;
        }

        while !in_buf.is_empty() {
            match socket.poll_send(cx, in_buf) {
                Poll::Ready(Err(e)) => panic!("{:?}", e),
                Poll::Pending => {
                    //println!("udp send pend");
                    return;
                }
                _ => (),
            }
        }

        in_recv.register_waker(cx.waker().clone());

        //println!("udp send no work");
    }

    fn poll_recv(&mut self, cx: &mut Context<'_>) {
        let Self {
            want_close,
            socket,
            out_send,
            out_buf,
            ..
        } = self;

        loop {
            match socket.poll_recv(cx, out_buf) {
                Poll::Ready(Err(e)) => panic!("{:?}", e),
                Poll::Pending => {
                    //println!("udp recv pend");
                    break;
                }
                _ => (),
            }
        }

        let mut should_close = false;
        out_send.push(
            &mut should_close,
            &mut WakeLater::new(),
            out_buf,
            *want_close,
        );
        if should_close {
            *want_close = true;
        }
    }
}

impl Future for UdpTestDriver {
    type Output = ();

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Self::Output> {
        self.poll_send(cx);
        self.poll_recv(cx);
        let shutdown = self.shutdown.load(atomic::Ordering::SeqCst);
        /*
        println!(
            "post-poll: shutdown: {}, want_close: {}",
            shutdown, self.want_close
        );
        */
        if shutdown
            || (self.want_close
                && self.in_buf.is_empty()
                && self.out_buf.is_empty())
        {
            //println!("test driver exit");
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    }
}

/// callback to shutdown the test
pub type UdpTestShutdown = Box<dyn FnOnce() + 'static + Send>;

/// construct a udp test sender / receiver / driver
pub fn udp_test(
    socket: Box<dyn UdpBackend>,
) -> (UdpTestSend, UdpTestRecv, BackendDriver, UdpTestShutdown) {
    let (in_send, in_recv) = in_chan();
    let (out_send, out_recv) = out_chan();
    let shutdown = Arc::new(atomic::AtomicBool::new(false));
    let driver = BackendDriver::new(UdpTestDriver {
        shutdown: shutdown.clone(),
        want_close: false,
        socket,
        in_recv,
        in_buf: VecDeque::new(),
        out_send,
        out_buf: VecDeque::new(),
    });
    let shutdown: UdpTestShutdown = Box::new(move || {
        shutdown.store(true, atomic::Ordering::SeqCst);
    });
    (in_send, out_recv, driver, shutdown)
}

/// receive side of udp test
pub type UdpTestRecv = OutChanReceiver<InUdpPacket>;

/// send side of udp test
pub type UdpTestSend = InChanSender<OutUdpPacket>;
