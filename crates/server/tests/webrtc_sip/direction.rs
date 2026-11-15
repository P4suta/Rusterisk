use super::{failure, Application};
use rsipstack::sip::{headers::typed::Contact, HeadersExt, Request, Response, SipMessage};
use rusterisk_media::{Frame, MediaError};
use rusterisk_sip::{Direction, DtlsSetup, WebRtcDescription};
use std::{net::SocketAddr, time::Duration};
use tokio::net::UdpSocket;

pub(super) struct Dialog {
    target: SocketAddr,
    uri: String,
    from: String,
    to: String,
    call_id: String,
    sequence: u32,
    description: String,
    security: Vec<String>,
    setup: DtlsSetup,
}

fn security(source: &str) -> Vec<String> {
    source
        .lines()
        .filter(|line| {
            ["a=ice-ufrag:", "a=ice-pwd:", "a=fingerprint:", "a=tls-id:"]
                .iter()
                .any(|prefix| line.starts_with(prefix))
        })
        .map(str::to_owned)
        .collect()
}

impl Dialog {
    pub(super) fn outgoing(
        invite: &Request,
        answer: &str,
        target: SocketAddr,
    ) -> Result<Self, MediaError> {
        let offered = std::str::from_utf8(&invite.body).map_err(failure)?;
        let setup = match WebRtcDescription::parse(answer).map_err(failure)?.setup() {
            DtlsSetup::Active => DtlsSetup::Passive,
            DtlsSetup::Passive => DtlsSetup::Active,
            DtlsSetup::ActPass => return Err(MediaError::InvalidPacket),
        };
        Ok(Self {
            target,
            uri: Contact::parse(invite.contact_header().map_err(failure)?.value())
                .map_err(failure)?
                .uri
                .to_string(),
            from: format!("{};tag=phone", invite.to_header().map_err(failure)?.value()),
            to: invite.from_header().map_err(failure)?.value().into(),
            call_id: invite.call_id_header().map_err(failure)?.value().into(),
            sequence: 0,
            description: answer.into(),
            security: security(offered),
            setup,
        })
    }

    pub(super) fn incoming(
        answer: &Response,
        offered: &str,
        target: SocketAddr,
    ) -> Result<Self, MediaError> {
        let local = std::str::from_utf8(&answer.body).map_err(failure)?;
        Ok(Self {
            target,
            uri: Contact::parse(answer.contact_header().map_err(failure)?.value())
                .map_err(failure)?
                .uri
                .to_string(),
            from: answer.from_header().map_err(failure)?.value().into(),
            to: answer.to_header().map_err(failure)?.value().into(),
            call_id: answer.call_id_header().map_err(failure)?.value().into(),
            sequence: 2,
            description: offered.into(),
            security: security(local),
            setup: WebRtcDescription::parse(local).map_err(failure)?.setup(),
        })
    }

    async fn update(&mut self, signal: &UdpSocket, direction: Direction) -> Result<(), MediaError> {
        let direction = match direction {
            Direction::SendRecv => "sendrecv",
            Direction::SendOnly => "sendonly",
            Direction::RecvOnly => "recvonly",
            Direction::Inactive => "inactive",
        };
        self.sequence += 1;
        let mut body = String::with_capacity(self.description.len());
        for line in self.description.lines() {
            if let Some(origin) = line.strip_prefix("o=") {
                let fields: Vec<&str> = origin.split_whitespace().collect();
                let [user, id, version, network, family, address] = fields.as_slice() else {
                    return Err(MediaError::InvalidPacket);
                };
                let version = version
                    .parse::<u64>()
                    .map_err(failure)?
                    .checked_add(1)
                    .ok_or(MediaError::Capacity)?;
                body.push_str(&format!(
                    "o={user} {id} {version} {network} {family} {address}\r\n"
                ));
            } else if line.starts_with("a=setup:") {
                body.push_str("a=setup:actpass\r\n");
            } else if !matches!(
                line,
                "a=sendrecv" | "a=sendonly" | "a=recvonly" | "a=inactive"
            ) {
                body.push_str(line);
                body.push_str("\r\n");
            }
        }
        body.push_str(&format!("a={direction}\r\n"));
        let peer = signal.local_addr().map_err(failure)?;
        let sequence = self.sequence;
        let request = format!("INVITE {} SIP/2.0\r\nVia: SIP/2.0/UDP {peer};branch=z9hG4bK-hold-{sequence};rport\r\nFrom: {}\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: {sequence} INVITE\r\nContact: <sip:phone@{peer}>\r\nMax-Forwards: 70\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{body}", self.uri, self.from, self.to, self.call_id, body.len());
        signal
            .send_to(request.as_bytes(), self.target)
            .await
            .map_err(failure)?;
        let mut bytes = [0; 16385];
        let answered = loop {
            let (size, source) = signal.recv_from(&mut bytes).await.map_err(failure)?;
            assert_eq!(source, self.target);
            let response = match SipMessage::try_from(&bytes[..size]).map_err(failure)? {
                SipMessage::Response(response) => response,
                SipMessage::Request(request) if request.method == rsipstack::sip::Method::Ack => {
                    continue
                }
                SipMessage::Request(_) => return Err(MediaError::InvalidPacket),
            };
            if response.cseq_header().map_err(failure)?.value() == format!("{sequence} INVITE") {
                break response;
            }
        };
        assert_eq!(answered.status_code.code(), 200);
        let answer = std::str::from_utf8(&answered.body).map_err(failure)?;
        assert_eq!(security(answer), self.security);
        assert_eq!(
            WebRtcDescription::parse(answer).map_err(failure)?.setup(),
            self.setup
        );
        let ack = format!("ACK {} SIP/2.0\r\nVia: SIP/2.0/UDP {peer};branch=z9hG4bK-hold-ack-{sequence};rport\r\nFrom: {}\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: {sequence} ACK\r\nMax-Forwards: 70\r\nContent-Length: 0\r\n\r\n", self.uri, self.from, self.to, self.call_id);
        signal
            .send_to(ack.as_bytes(), self.target)
            .await
            .map_err(failure)?;
        self.description = body;
        Ok(())
    }
}

pub(super) async fn cycle(
    signal: &UdpSocket,
    dialog: &mut Dialog,
    left: &mut Application,
    right: &mut Application,
) -> Result<(), MediaError> {
    for direction in [
        Direction::Inactive,
        Direction::SendOnly,
        Direction::RecvOnly,
    ] {
        dialog.update(signal, direction).await?;
        restricted(left, right, direction).await?;
        dialog.update(signal, Direction::SendRecv).await?;
        resumed(left, right).await?;
    }
    Ok(())
}

async fn restricted(
    left: &mut Application,
    right: &mut Application,
    direction: Direction,
) -> Result<(), MediaError> {
    let (left_sample, right_sample) = match direction {
        Direction::Inactive => (None, 0),
        Direction::SendOnly => (None, 4000),
        Direction::RecvOnly => (Some(-7000), 0),
        Direction::SendRecv => return Err(MediaError::InvalidRoute),
    };
    let started = tokio::time::Instant::now();
    let mut clock = tokio::time::interval(Duration::from_millis(20));
    clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut received = [0; 2];
    while started.elapsed() < Duration::from_millis(500) {
        tokio::select! {
            _instant = clock.tick() => {
                left.outgoing.try_send(Frame::new(&[4000; 960], 48000)?).map_err(failure)?;
                right.outgoing.try_send(Frame::new(&[-7000; 960], 48000)?).map_err(failure)?;
            }
            frame = left.incoming.recv() => {
                let frame = frame.ok_or(MediaError::UnknownStream)?;
                if started.elapsed() >= Duration::from_millis(100) {
                    let expected = left_sample.ok_or_else(|| MediaError::Transport("Held WebRTC still received audio".into()))?;
                    assert!(frame.samples().iter().all(|sample| (i32::from(*sample) - expected).abs() < 300));
                    received[0] += 1;
                }
            }
            frame = right.incoming.recv() => {
                let frame = frame.ok_or(MediaError::UnknownStream)?;
                if started.elapsed() >= Duration::from_millis(100) {
                    assert!(frame.samples().iter().all(|sample| (i32::from(*sample) - right_sample).abs() < 300));
                    received[1] += 1;
                }
            }
        }
    }
    if left_sample.is_some() {
        assert!(received[0] >= 8);
    }
    assert!(received[1] >= 8);
    Ok(())
}

async fn resumed(left: &mut Application, right: &mut Application) -> Result<(), MediaError> {
    let mut clock = tokio::time::interval(Duration::from_millis(20));
    clock.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut received = [0; 2];
    while !received.iter().all(|received| *received >= 6) {
        tokio::select! {
            _instant = clock.tick() => {
                left.outgoing.try_send(Frame::new(&[4000; 960], 48000)?).map_err(failure)?;
                right.outgoing.try_send(Frame::new(&[-7000; 960], 48000)?).map_err(failure)?;
            }
            frame = left.incoming.recv() => {
                let matches = frame.ok_or(MediaError::UnknownStream)?.samples().iter().all(|sample| (i32::from(*sample) + 7000).abs() < 300);
                received[0] = if matches { received[0] + 1 } else { 0 };
            }
            frame = right.incoming.recv() => {
                let matches = frame.ok_or(MediaError::UnknownStream)?.samples().iter().all(|sample| (i32::from(*sample) - 4000).abs() < 300);
                received[1] = if matches { received[1] + 1 } else { 0 };
            }
        }
    }
    Ok(())
}
