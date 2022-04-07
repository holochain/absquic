use super::*;

pin_project_lite::pin_project! {
    pub struct ConTransmitDriver {
        max_gso_provider: MaxGsoProvider,
        udp_packet_send: MultiSenderPoll<OutUdpPacket>,
        udp_packet_send_closed: bool,
    }
}

impl ConTransmitDriver {
    pub fn new(
        max_gso_provider: MaxGsoProvider,
        udp_packet_send: MultiSender<OutUdpPacket>,
    ) -> Self {
        Self {
            max_gso_provider,
            udp_packet_send: MultiSenderPoll::new(udp_packet_send),
            udp_packet_send_closed: false,
        }
    }

    pub fn poll(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        now: std::time::Instant,
        connection: &mut quinn_proto::Connection,
        more_work: &mut bool,
        want_close: &mut bool,
    ) -> AqResult<()> {
        let this = self.project();

        if *this.udp_packet_send_closed {
            *want_close = true;
            return Ok(());
        }

        for _ in 0..32 {
            match this.udp_packet_send.poll_acquire(cx) {
                Poll::Pending => return Ok(()),
                Poll::Ready(Err(_)) => {
                    tracing::debug!("con udp send closed");
                    *this.udp_packet_send_closed = true;
                    *want_close = true;
                    return Ok(());
                }
                Poll::Ready(Ok(sender)) => {
                    let gso = (this.max_gso_provider)();
                    if let Some(transmit) = connection.poll_transmit(now, gso) {
                        let quinn_proto::Transmit {
                            destination,
                            ecn,
                            contents,
                            segment_size,
                            src_ip,
                        } = transmit;
                        sender.send(OutUdpPacket {
                            dst_addr: destination,
                            src_ip,
                            segment_size,
                            ecn: ecn.map(|ecn| ecn as u8),
                            data: contents,
                        });
                    } else {
                        // for now just drop our sender.. we might decide
                        // to cache this if it's identified as inefficient
                        return Ok(());
                    }
                }
            }
        }

        // if we didn't return, there is more work
        *more_work = true;
        Ok(())
    }
}
