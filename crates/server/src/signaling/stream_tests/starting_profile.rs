use super::{configuration, credentials, failure};
use crate::{
    media::MediaHub,
    rtp::SocketPair,
    signaling::{cleanup::combine, transport::Input, Signaling},
    CallKey, ControlError, Controller, Effect, Subscription,
};
use rsipstack::sip::{
    headers::{
        auth::{Algorithm, AuthQop},
        typed::WwwAuthenticate,
    },
    services::DigestGenerator,
    HeadersExt, Method, Response, SipMessage, Uri,
};
use rusterisk_config::{Codec, Dtmf};
use rusterisk_core::{DtmfDigit, DtmfOutbox, DtmfTiming};
use rusterisk_sip::Session;
use std::{net::SocketAddr, time::Duration};
use tokio::{net::UdpSocket, task::JoinHandle};

#[derive(Clone, Copy)]
enum Admission {
    Answer,
    Early,
}

fn require(condition: bool, message: &'static str) -> Result<(), ControlError> {
    if condition {
        Ok(())
    } else {
        Err(ControlError::Invalid(message))
    }
}

struct Fixture {
    signaling: Signaling,
    runner: JoinHandle<Result<(), ControlError>>,
    hub: JoinHandle<Result<(), rusterisk_media::MediaError>>,
    subscription: Option<Subscription>,
    peer: UdpSocket,
    audio: SocketPair,
    original: Option<SocketPair>,
    replacement: SocketPair,
    local: SocketAddr,
    media: SocketAddr,
    addresses: Vec<SocketAddr>,
}

impl Fixture {
    async fn new() -> Result<Self, ControlError> {
        let ip = "127.0.0.1".parse().map_err(failure)?;
        let peer = UdpSocket::bind((ip, 0)).await.map_err(failure)?;
        let audio = SocketPair::bind(ip, 0, 0).map_err(failure)?;
        let original = SocketPair::bind(ip, 0, 0).map_err(failure)?;
        let replacement = SocketPair::bind(ip, 0, 0).map_err(failure)?;
        let media = original.local_addr().map_err(failure)?;
        let mut config = configuration("udp", "")?;
        config.rtp.start = media.port();
        config.rtp.end = original.control_addr().map_err(failure)?.port();
        config
            .transports
            .first_mut()
            .ok_or(ControlError::Capacity)?
            .external_media_address = Some("192.0.2.20".parse().map_err(failure)?);
        config
            .endpoints
            .first_mut()
            .ok_or(ControlError::Capacity)?
            .dtmf = Dtmf::Info;
        let (controller, runner, effects) = Controller::new(config.clone(), 4);
        let (port, hub, notices) = MediaHub::new(4);
        let signaling = Signaling::bind(config, controller.clone(), effects, port, notices).await?;
        let local = signaling.network.local_addr(0)?;
        let addresses = vec![
            local,
            peer.local_addr().map_err(failure)?,
            audio.local_addr().map_err(failure)?,
            audio.control_addr().map_err(failure)?,
            media,
            original.control_addr().map_err(failure)?,
            replacement.local_addr().map_err(failure)?,
            replacement.control_addr().map_err(failure)?,
        ];
        let credentials = credentials()?;
        let mut fixture = Self {
            signaling,
            runner: tokio::spawn(runner.run()),
            hub: tokio::spawn(hub.run(controller.shutdown_signal())),
            subscription: None,
            peer,
            audio,
            original: Some(original),
            replacement,
            local,
            media,
            addresses,
        };
        let subscribed = tokio::time::timeout(
            Duration::from_secs(2),
            controller.subscribe(credentials, None, "calls".into()),
        )
        .await
        .map_err(failure)
        .and_then(|result| result);
        match subscribed {
            Ok(subscription) => {
                fixture.subscription = Some(subscription);
                Ok(fixture)
            }
            Err(primary) => match fixture.finish(Err(primary.clone())).await {
                Err(error) => Err(error),
                Ok(()) => Err(primary),
            },
        }
    }

    fn invite(&self, sequence: u32, authorization: &str) -> Result<String, ControlError> {
        let peer = self.peer.local_addr().map_err(failure)?;
        let audio = self.audio.local_addr().map_err(failure)?;
        let body = format!("v=0\r\no=- 1 1 IN IP4 127.0.0.1\r\ns=starting-profile\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio {} RTP/AVP 0 101\r\na=rtpmap:0 PCMU/8000\r\na=rtpmap:101 telephone-event/8000\r\na=sendrecv\r\n", audio.port());
        Ok(format!("INVITE sip:100@{} SIP/2.0\r\nVia: SIP/2.0/UDP {peer};branch=z9hG4bK-incoming-profile-{sequence};rport\r\nFrom: <sip:phone@localhost>;tag=starting-peer\r\nTo: <sip:100@localhost>\r\nCall-ID: incoming-starting-profile\r\nCSeq: {sequence} INVITE\r\nContact: <sip:phone@{peer}>\r\nMax-Forwards: 70\r\n{authorization}Content-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{body}", self.local, body.len()))
    }

    async fn send(&mut self, bytes: &[u8], now: u64) -> Result<(), ControlError> {
        require(
            self.peer
                .send_to(bytes, self.local)
                .await
                .map_err(failure)?
                == bytes.len(),
            "Short incoming profile SIP datagram",
        )?;
        let packet =
            match tokio::time::timeout(Duration::from_secs(2), self.signaling.network.next())
                .await
                .map_err(failure)??
            {
                Input::Packet(packet) => packet,
                Input::Closed { result, .. } | Input::SetupFinished { result, .. } => {
                    result?;
                    return Err(ControlError::Invalid("Incoming profile flow ended"));
                }
                Input::Prepared(_) => {
                    return Err(ControlError::Invalid("Unexpected incoming setup"))
                }
            };
        require(
            packet.peer == self.peer.local_addr().map_err(failure)? && packet.bytes == bytes,
            "Incoming profile packet changed its source or bytes",
        )?;
        let output = self.signaling.listeners[packet.listener]
            .engine
            .receive(&packet.bytes, packet.peer, now)
            .map_err(failure)?;
        self.signaling.output(packet.listener, output, now).await
    }

    async fn receive(&self) -> Result<SipMessage, ControlError> {
        let mut bytes = [0; rusterisk_sip::MAX_MESSAGE + 1];
        let (length, peer) =
            tokio::time::timeout(Duration::from_secs(2), self.peer.recv_from(&mut bytes))
                .await
                .map_err(failure)?
                .map_err(failure)?;
        require(
            peer == self.local,
            "Incoming profile response changed its source",
        )?;
        SipMessage::try_from(&bytes[..length]).map_err(failure)
    }

    async fn response(&self, status: u16) -> Result<Response, ControlError> {
        let SipMessage::Response(response) = self.receive().await? else {
            return Err(ControlError::Invalid("Expected incoming profile response"));
        };
        require(
            response.status_code.code() == status,
            "Unexpected incoming profile response status",
        )?;
        Ok(response)
    }

    async fn admit(&mut self) -> Result<CallKey, ControlError> {
        self.send(self.invite(1, "")?.as_bytes(), 0).await?;
        let challenge = self.response(401).await?;
        let challenge = WwwAuthenticate::parse(
            challenge
                .www_authenticate_header()
                .ok_or(ControlError::Capacity)?
                .value(),
        )
        .map_err(failure)?;
        let uri = Uri::try_from(format!("sip:100@{}", self.local)).map_err(failure)?;
        let qop = AuthQop::Auth {
            cnonce: "profile-client".into(),
            nc: 1,
        };
        require(
            challenge.algorithm.unwrap_or(Algorithm::Md5) == Algorithm::Md5,
            "Expected the fixture's MD5 challenge",
        )?;
        let digest = DigestGenerator {
            username: "phone",
            password: "test-password",
            nonce: &challenge.nonce,
            uri: &uri,
            realm: &challenge.realm,
            method: &Method::Invite,
            qop: Some(&qop),
            algorithm: Algorithm::Md5,
        }
        .compute();
        let authorization = format!("Authorization: Digest username=\"phone\", realm=\"{}\", nonce=\"{}\", uri=\"{uri}\", response=\"{digest}\", algorithm=MD5, qop=auth, cnonce=\"profile-client\", nc=00000001\r\n", challenge.realm, challenge.nonce);
        self.send(self.invite(2, &authorization)?.as_bytes(), 1)
            .await?;
        self.response(100).await?;
        require(
            self.signaling.calls.len() == 1,
            "Incoming profile was not admitted",
        )?;
        self.signaling
            .calls
            .keys()
            .next()
            .cloned()
            .ok_or(ControlError::Capacity)
    }

    fn replace_current_settings(&mut self) -> Result<(), ControlError> {
        let mut changed = self.signaling.config.clone();
        changed.rtp.start = self.replacement.local_addr().map_err(failure)?.port();
        changed.rtp.end = self.replacement.control_addr().map_err(failure)?.port();
        changed
            .transports
            .first_mut()
            .ok_or(ControlError::Capacity)?
            .external_media_address = Some("192.0.2.21".parse().map_err(failure)?);
        let endpoint = changed
            .endpoints
            .first_mut()
            .ok_or(ControlError::Capacity)?;
        endpoint.dtmf = Dtmf::Rfc4733;
        endpoint.codecs = [Codec::Pcma].into_iter().collect();
        self.signaling.config = changed;
        drop(self.original.take());
        Ok(())
    }

    fn starting_media(&self, response: &Response) -> Result<(), ControlError> {
        let session = Session::parse(
            std::str::from_utf8(&response.body).map_err(failure)?,
            &[Codec::Pcmu, Codec::Pcma],
        )
        .map_err(failure)?;
        require(
            session.remote
                == SocketAddr::new("192.0.2.20".parse().map_err(failure)?, self.media.port())
                && session.codec == Codec::Pcmu,
            "Incoming response used replacement media settings",
        )?;
        let control = self
            .media
            .port()
            .checked_add(1)
            .ok_or(ControlError::Capacity)?;
        match SocketPair::bind(self.media.ip(), self.media.port(), control) {
            Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => Ok(()),
            Err(error) => Err(failure(error)),
            Ok(_) => Err(ControlError::Invalid(
                "Published incoming RTP pair has no owner",
            )),
        }
    }

    async fn starting_digits(
        &mut self,
        key: &CallKey,
        answer: &Response,
    ) -> Result<(), ControlError> {
        let peer = self.peer.local_addr().map_err(failure)?;
        let ack = format!("ACK sip:100@{} SIP/2.0\r\nVia: SIP/2.0/UDP {peer};branch=z9hG4bK-incoming-profile-ack;rport\r\nFrom: {}\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: 2 ACK\r\nMax-Forwards: 70\r\nContent-Length: 0\r\n\r\n", self.local, answer.from_header().map_err(failure)?.value(), answer.to_header().map_err(failure)?.value(), answer.call_id_header().map_err(failure)?.value());
        self.send(ack.as_bytes(), 12).await?;
        let mut digits = DtmfOutbox::new(u16::MAX)?;
        let command = digits.admit(&[DtmfDigit::Five], DtmfTiming::new(0, 0, 40, 0)?)?;
        self.signaling
            .effect(
                Effect::Dtmf {
                    key: key.clone(),
                    command,
                },
                13,
            )
            .await?;
        self.signaling.digit_tick(13).await?;
        let SipMessage::Request(info) = self.receive().await? else {
            return Err(ControlError::Invalid(
                "Captured INFO mode did not send a request",
            ));
        };
        require(
            info.method == Method::Info
                && info.call_id_header().map_err(failure)?
                    == answer.call_id_header().map_err(failure)?
                && info.from_header().map_err(failure)?.value()
                    == answer.to_header().map_err(failure)?.value()
                && info.to_header().map_err(failure)?.value()
                    == answer.from_header().map_err(failure)?.value()
                && info.body == b"Signal=5\r\nDuration=40\r\n",
            "Incoming digit delivery changed its admitted INFO profile",
        )?;
        let reply = format!("SIP/2.0 200 OK\r\nVia: {}\r\nFrom: {}\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: {}\r\nContent-Length: 0\r\n\r\n", info.top_via_header().map_err(failure)?.value(), info.from_header().map_err(failure)?.value(), info.to_header().map_err(failure)?.value(), info.call_id_header().map_err(failure)?.value(), info.cseq_header().map_err(failure)?.value());
        self.send(reply.as_bytes(), 14).await
    }

    async fn finish(self, primary: Result<(), ControlError>) -> Result<(), ControlError> {
        let controller = self.signaling.controller.clone();
        let stopped = controller.shutdown_signal();
        controller.shutdown();
        let mut result = combine(
            primary,
            joined(tokio::spawn(self.signaling.run(stopped))).await,
        );
        result = combine(result, joined(self.runner).await);
        result = combine(result, joined(self.hub).await);
        drop((
            self.subscription,
            self.peer,
            self.audio,
            self.original,
            self.replacement,
            controller,
        ));
        let mut rebound = Vec::with_capacity(self.addresses.len());
        for address in self.addresses {
            match UdpSocket::bind(address).await {
                Ok(socket) => rebound.push(socket),
                Err(error) => result = combine(result, Err(failure(error))),
            }
        }
        result
    }
}

async fn joined<E: std::fmt::Display>(
    mut task: JoinHandle<Result<(), E>>,
) -> Result<(), ControlError> {
    match tokio::time::timeout(Duration::from_secs(3), &mut task).await {
        Ok(result) => result.map_err(failure)?.map_err(failure),
        Err(primary) => {
            task.abort();
            let cleanup = match task.await {
                Ok(result) => result.map_err(failure),
                Err(error) if error.is_cancelled() => Ok(()),
                Err(error) => Err(failure(error)),
            };
            combine(Err(failure(primary)), cleanup)
        }
    }
}

async fn exercise(admission: Admission) -> Result<(), ControlError> {
    let mut fixture = Fixture::new().await?;
    let mut phase = "authenticated admission";
    let primary = tokio::time::timeout(Duration::from_secs(8), async {
        let key = fixture.admit().await?;
        phase = "adapter current settings replacement";
        fixture.replace_current_settings()?;
        let progress = match admission {
            Admission::Early => {
                phase = "early transport from the admitted profile";
                fixture.signaling.prepare_early_media(&key, 10).await?;
                let progress = fixture.response(183).await?;
                fixture.starting_media(&progress)?;
                Some(progress)
            }
            Admission::Answer => None,
        };
        phase = "answer from the admitted profile";
        fixture.signaling.answer(&key, 11).await?;
        let answer = fixture.response(200).await?;
        fixture.starting_media(&answer)?;
        if let Some(progress) = progress {
            require(
                answer.body == progress.body
                    && answer.to_header().map_err(failure)?
                        == progress.to_header().map_err(failure)?
                    && answer.contact_header().map_err(failure)?
                        == progress.contact_header().map_err(failure)?,
                "Answer replaced the early starting transport",
            )?;
        }
        phase = "admitted INFO delivery";
        fixture.starting_digits(&key, &answer).await
    })
    .await
    .map_err(failure)
    .and_then(|result| result)
    .map_err(|error| failure(format!("incoming starting profile phase={phase}: {error}")));
    fixture.finish(primary).await
}

#[tokio::test]
async fn an_incoming_adapter_keeps_its_admission_settings_until_answer() -> Result<(), ControlError>
{
    exercise(Admission::Answer).await
}

#[tokio::test]
async fn an_early_incoming_adapter_keeps_its_admission_settings_through_answer(
) -> Result<(), ControlError> {
    exercise(Admission::Early).await
}
