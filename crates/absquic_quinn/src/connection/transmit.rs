use super::*;

pin_project_lite::pin_project! {
    pub struct ConTransmitDriver {
        udp_packet_send: MultiSenderPoll<OutUdpPacket>,
        udp_packet_send_closed: bool,
    }
}

impl ConTransmitDriver {
    pub fn new(udp_packet_send: MultiSender<OutUdpPacket>) -> Self {
        Self {
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
                    tracing::debug!("tx udp send closed");
                    *this.udp_packet_send_closed = true;
                    *want_close = true;
                    return Ok(());
                }
                Poll::Ready(Ok(sender)) => {
                    // TODO - FIX MAX GSO!!! FIXME
                    if let Some(transmit) = connection.poll_transmit(now, 1) {
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
