use super::super::{
    answer_dialog_at, exchange, final_response, invite_at, transfer_peer::Peer, AudioPeer,
};
use super::{
    combine, failure,
    signals::{acknowledge, ReferEvent, RequestRoute, TransferSignals},
};
use rsipstack::sip::{headers::untyped::ToTypedHeader, HeadersExt, SipMessage};
use rusterisk_config::Codec;
use rusterisk_media::{AudioFormat, RtpSender};
use rusterisk_server::ControlError;
use rusterisk_sip::Session;
use std::net::SocketAddr;
use tokio::{net::UdpSocket, sync::mpsc};

#[derive(Clone, Copy)]
pub(super) enum Departure {
    Server,
    Transferor,
}

pub(super) struct ObservedTransfer {
    pub event: ReferEvent,
    pub initial_before_answer: bool,
    pub server_bye_before_answer: bool,
}

fn pending_signal(socket: &UdpSocket) -> Result<String, ControlError> {
    let mut bytes = [0; 16385];
    match socket.try_recv_from(&mut bytes) {
        Ok((size, _)) => match SipMessage::try_from(&bytes[..size]).map_err(failure)? {
            SipMessage::Response(response) => {
                Ok(format!("response {}", response.status_code.code()))
            }
            SipMessage::Request(request) => Ok(format!("request {}", request.method)),
        },
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok("none".into()),
        Err(error) => Err(failure(error)),
    }
}

pub(super) async fn run(
    peers: [Peer; 3],
    target: SocketAddr,
    media_ports: mpsc::Sender<SocketAddr>,
    departure: Departure,
    requests: RequestRoute,
) -> Result<ObservedTransfer, ControlError> {
    let [alice, bob, charlie] = peers;
    invite_at(&alice.signal, target, alice.contact()?, alice.media()?)
        .await
        .map_err(|error| failure(format!("authenticated invitation: {error}")))?;
    let (bob_target, _, _) = answer_dialog_at(&bob.signal, bob.contact()?, bob.media()?)
        .await
        .map_err(|error| match pending_signal(&alice.signal) {
            Ok(observed) => failure(format!(
                "outgoing peer answer: {error}; incoming {observed}"
            )),
            Err(observation) => failure(format!(
                "outgoing peer answer: {error}; observation failed: {observation}"
            )),
        })?;
    media_ports.try_send(bob_target).map_err(failure)?;
    let answered = final_response(&alice.signal)
        .await
        .map_err(|error| failure(format!("incoming peer answer: {error}")))?;
    if answered.status_code.code() != 200 {
        return Err(ControlError::Invalid("Expected an answered call"));
    }
    let alice_target = Session::parse(
        std::str::from_utf8(&answered.body).map_err(failure)?,
        &[Codec::Pcmu],
    )
    .map_err(failure)?
    .remote;
    media_ports.try_send(alice_target).map_err(failure)?;
    let remote = answered
        .contact_header()
        .map_err(failure)?
        .typed()
        .map_err(failure)?
        .uri;
    let ack = format!("ACK {remote} SIP/2.0\r\nVia: SIP/2.0/UDP {};branch=z9hG4bK-transfer-ack;rport\r\nFrom: {}\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: 2 ACK\r\nMax-Forwards: 70\r\nContent-Length: 0\r\n\r\n", alice.contact()?, answered.from_header().map_err(failure)?.value(), answered.to_header().map_err(failure)?.value(), answered.call_id_header().map_err(failure)?.value());
    alice
        .signal
        .send_to(ack.as_bytes(), target)
        .await
        .map_err(failure)?;
    let mut left = AudioPeer {
        socket: &alice.audio,
        target: alice_target,
        sender: RtpSender::new(AudioFormat::Pcmu, 0, [7; 10]).map_err(failure)?,
        sample: 4000,
    };
    let mut right = AudioPeer {
        socket: &bob.audio,
        target: bob_target,
        sender: RtpSender::new(AudioFormat::Pcmu, 0, [8; 10]).map_err(failure)?,
        sample: -7000,
    };
    exchange(&mut left, &mut right, (-7000, 4000))
        .await
        .map_err(|error| failure(format!("initial received audio: {error}")))?;
    let refer = format!("REFER {remote} SIP/2.0\r\nVia: SIP/2.0/UDP {};branch=z9hG4bK-transfer-refer;rport\r\nFrom: {}\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: 3 REFER\r\nContact: <sip:phone@{}>\r\nRefer-To: <sip:200@{target}>\r\nMax-Forwards: 70\r\nContent-Length: 0\r\n\r\n", alice.contact()?, answered.from_header().map_err(failure)?.value(), answered.to_header().map_err(failure)?.value(), answered.call_id_header().map_err(failure)?.value(), alice.contact()?);
    alice
        .signal
        .send_to(refer.as_bytes(), target)
        .await
        .map_err(failure)?;
    let mut observed = TransferSignals::new(&answered, requests)?;
    observed
        .accepted(&alice.signal, target)
        .await
        .map_err(|error| failure(format!("REFER response: {error}")))?;
    match departure {
        Departure::Server => observed.source_ended(&alice.signal, target).await,
        Departure::Transferor => {
            observed
                .transferor_ended(&alice.signal, target, alice.contact()?)
                .await
        }
    }
    .map_err(|error| failure(format!("source dialog departure: {error}")))?;
    let initial_before_answer = observed.received_initial_notify();
    let server_bye_before_answer = observed.received_server_bye();
    let (charlie_target, _, charlie_acknowledgement) =
        answer_dialog_at(&charlie.signal, charlie.contact()?, charlie.media()?)
            .await
            .map_err(|error| failure(format!("transfer target answer: {error}")))?;
    media_ports.try_send(charlie_target).map_err(failure)?;
    acknowledge(&charlie.signal, &charlie_acknowledgement).await.map_err(|error| failure(format!("{error}; initial_before_answer={initial_before_answer}, server_bye_before_answer={server_bye_before_answer}, event={:?}", observed.event().ok())))?;
    let mut third = AudioPeer {
        socket: &charlie.audio,
        target: charlie_target,
        sender: RtpSender::new(AudioFormat::Pcmu, 0, [9; 10]).map_err(failure)?,
        sample: 9000,
    };
    let (progress, audio) = tokio::join!(
        observed.completed(&alice.signal, target),
        exchange(&mut right, &mut third, (9000, -7000))
    );
    combine(
        progress.map_err(|error| failure(format!("final transfer notification: {error}"))),
        audio.map_err(|error| failure(format!("transferred received audio: {error}"))),
    )?;
    Ok(ObservedTransfer {
        event: observed.event()?,
        initial_before_answer,
        server_bye_before_answer,
    })
}
