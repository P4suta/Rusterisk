use rusterisk_core::DtmfTone;
use rusterisk_media::{
    AudioFormat, Frame, MediaError, RtpPacket, RtpReceiver, RtpSender, TelephoneFormat,
    TelephoneReceiver, MAX_PACKET,
};
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::net::UdpSocket;
use tokio::sync::{mpsc, watch};

pub struct SocketPair {
    data: UdpSocket,
    control: UdpSocket,
}

impl SocketPair {
    pub fn bind(ip: IpAddr, start: u16, end: u16) -> Result<Self, io::Error> {
        let ephemeral = start == 0 && end == 0;
        if !ephemeral && (start == 0 || !start.is_multiple_of(2) || start >= end) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid media port range",
            ));
        }
        let attempts = if ephemeral {
            128
        } else {
            (usize::from(end - start) / 2 + 1).min(256)
        };
        for index in 0..attempts {
            let port = if ephemeral {
                0
            } else {
                start + index as u16 * 2
            };
            let data = match std::net::UdpSocket::bind(SocketAddr::new(ip, port)) {
                Ok(socket) => socket,
                Err(error) if error.kind() == io::ErrorKind::AddrInUse => continue,
                Err(error) => return Err(error),
            };
            let local = data.local_addr()?;
            if !local.port().is_multiple_of(2) {
                continue;
            }
            let Some(control_port) = local.port().checked_add(1) else {
                continue;
            };
            if !ephemeral && control_port > end {
                continue;
            }
            let control = match std::net::UdpSocket::bind(SocketAddr::new(ip, control_port)) {
                Ok(socket) => socket,
                Err(error) if error.kind() == io::ErrorKind::AddrInUse => continue,
                Err(error) => return Err(error),
            };
            data.set_nonblocking(true)?;
            control.set_nonblocking(true)?;
            return Ok(Self {
                data: UdpSocket::from_std(data)?,
                control: UdpSocket::from_std(control)?,
            });
        }
        Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            "media port capacity exceeded",
        ))
    }

    pub fn local_addr(&self) -> Result<SocketAddr, io::Error> {
        self.data.local_addr()
    }

    pub fn control_addr(&self) -> Result<SocketAddr, io::Error> {
        self.control.local_addr()
    }

    pub fn data(&self) -> &UdpSocket {
        &self.data
    }
    pub fn control(&self) -> &UdpSocket {
        &self.control
    }
}

#[derive(Clone, Copy, Debug)]
pub struct RtpSession {
    pub remote: SocketAddr,
    pub format: AudioFormat,
    pub payload_type: u8,
    pub telephone: Option<TelephoneFormat>,
}

pub struct RtpChannels {
    pub incoming: mpsc::Sender<Frame>,
    pub digits: mpsc::Sender<DtmfTone>,
    pub outgoing: mpsc::Receiver<Frame>,
    pub stop: watch::Receiver<bool>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct LinkStats {
    pub packets_received: u64,
    pub packets_sent: u64,
    pub dropped: u64,
}

pub struct RtpLink {
    sockets: SocketPair,
    session: RtpSession,
    channels: RtpChannels,
    receiver: RtpReceiver,
    sender: RtpSender,
    telephone: Option<TelephoneReceiver>,
}

fn failure(error: impl std::fmt::Display) -> MediaError {
    MediaError::Transport(error.to_string())
}

async fn send(socket: &UdpSocket, bytes: &[u8], peer: SocketAddr) -> Result<(), MediaError> {
    let sent = tokio::time::timeout(Duration::from_millis(20), socket.send_to(bytes, peer))
        .await
        .map_err(failure)?
        .map_err(failure)?;
    if sent != bytes.len() {
        return Err(MediaError::Transport("incomplete datagram".into()));
    }
    Ok(())
}

fn report_delay(initial: bool) -> Result<Duration, MediaError> {
    let mut entropy = [0; 2];
    getrandom::fill(&mut entropy).map_err(failure)?;
    let random = f64::from(u16::from_be_bytes(entropy)) / 65536.0 + 0.5;
    Ok(Duration::from_secs_f64(
        if initial { 2.5 } else { 5.0 } * random / 1.21828,
    ))
}

impl RtpLink {
    pub fn new(
        sockets: SocketPair,
        session: RtpSession,
        channels: RtpChannels,
    ) -> Result<Self, MediaError> {
        if session.remote.port() == 0 || session.remote.port() == u16::MAX {
            return Err(MediaError::InvalidPacket);
        }
        if session
            .telephone
            .is_some_and(|format| format.payload_type() == session.payload_type)
        {
            return Err(MediaError::InvalidPacket);
        }
        let mut seed = [0; 10];
        getrandom::fill(&mut seed).map_err(failure)?;
        Ok(Self {
            telephone: session
                .telephone
                .map(|format| TelephoneReceiver::new(format.payload_type(), format.rate()))
                .transpose()?,
            receiver: RtpReceiver::new(session.format, session.payload_type)?,
            sender: RtpSender::new(session.format, session.payload_type, seed)?,
            sockets,
            session,
            channels,
        })
    }

    pub async fn run(mut self) -> Result<LinkStats, MediaError> {
        let period = Duration::from_millis(20);
        let mut playout =
            tokio::time::interval_at(tokio::time::Instant::now() + period * 2, period);
        playout.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        let mut report_at = tokio::time::Instant::now() + report_delay(true)?;
        let mut data = [0; MAX_PACKET + 1];
        let mut control = [0; MAX_PACKET + 1];
        let mut stats = LinkStats::default();
        let started = tokio::time::Instant::now();
        let mut last_sent = None;
        let mut octets = 0u32;
        let control_peer =
            SocketAddr::new(self.session.remote.ip(), self.session.remote.port() + 1);
        loop {
            if *self.channels.stop.borrow() || self.channels.incoming.is_closed() {
                break;
            }
            tokio::select! {
                changed = self.channels.stop.changed() => {
                    if changed.is_err() || *self.channels.stop.borrow() { break; }
                }
                packet = self.sockets.data.recv_from(&mut data) => {
                    let (size, peer) = packet.map_err(failure)?;
                    if peer != self.session.remote || size > MAX_PACKET { stats.dropped += 1; continue; }
                    let now = started.elapsed().as_millis() as u64;
                    match RtpPacket::parse(&data[..size]).and_then(|packet| self.receive(&packet, now)) {
                        Ok(true) => stats.packets_received += 1,
                        Ok(false) | Err(_) => stats.dropped += 1,
                    }
                }
                packet = self.sockets.control.recv_from(&mut control) => {
                    let (size, peer) = packet.map_err(failure)?;
                    if peer != control_peer || size > MAX_PACKET || rusterisk_media::inspect_rtcp(&control[..size]).is_err() { stats.dropped += 1; }
                }
                frame = self.channels.outgoing.recv() => {
                    let Some(frame) = frame else { break; };
                    let packet = self.sender.packet(frame)?;
                    send(&self.sockets.data, &packet.encode()?, self.session.remote).await?;
                    last_sent = Some((packet.ssrc(), packet.timestamp(), std::time::Instant::now()));
                    octets = octets.wrapping_add(packet.payload().len() as u32);
                    stats.packets_sent += 1;
                }
                _instant = playout.tick() => {
                    if let Some(telephone) = &mut self.telephone {
                        if let Some(tone) = telephone.expire(started.elapsed().as_millis() as u64)? {
                            self.channels.digits.try_send(tone).map_err(|_| MediaError::Capacity)?;
                        }
                    }
                    match self.receiver.pop() {
                        Ok(Some(frame)) => match self.channels.incoming.try_send(frame) {
                            Ok(()) => {}
                            Err(mpsc::error::TrySendError::Full(_)) => stats.dropped += 1,
                            Err(mpsc::error::TrySendError::Closed(_)) => break,
                        },
                        Ok(None) => {}
                        Err(_) => stats.dropped += 1,
                    }
                }
                () = tokio::time::sleep_until(report_at) => {
                    if let Some((ssrc, timestamp, sent_at)) = last_sent {
                        let now = SystemTime::now().duration_since(UNIX_EPOCH).map_err(failure)?;
                        let ntp = (now.as_secs().wrapping_add(2_208_988_800) << 32)
                            | ((u64::from(now.subsec_nanos()) << 32) / 1_000_000_000);
                        let elapsed = sent_at.elapsed().as_nanos() * u128::from(self.session.format.rate()) / 1_000_000_000;
                        let timestamp = timestamp.wrapping_add(elapsed as u32);
                        let bytes = rusterisk_media::sender_report(ssrc, ntp, timestamp, stats.packets_sent as u32, octets, "rusterisk")?;
                        send(&self.sockets.control, &bytes, control_peer).await?;
                    }
                    report_at = tokio::time::Instant::now() + report_delay(false)?;
                }
            }
        }
        Ok(stats)
    }

    fn receive(&mut self, packet: &RtpPacket, now: u64) -> Result<bool, MediaError> {
        if self
            .session
            .telephone
            .is_some_and(|format| packet.payload_type() == format.payload_type())
        {
            let telephone = self.telephone.as_mut().ok_or(MediaError::Unsupported)?;
            for tone in telephone.push(packet, now)?.into_tones() {
                self.channels
                    .digits
                    .try_send(tone)
                    .map_err(|_| MediaError::Capacity)?;
            }
            self.receiver.skip(packet)?;
            Ok(true)
        } else {
            self.receiver.push(packet)
        }
    }
}
