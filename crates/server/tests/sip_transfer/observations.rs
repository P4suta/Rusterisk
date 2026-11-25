use super::{Observation, ReferEvent, RequestRoute, TransferSignals};
use crate::{failure, transfer_lifetime::combine};
use rsipstack::sip::{HeadersExt, Response, SipMessage};
use rusterisk_server::ControlError;
use rusterisk_test_support::Wait;
use std::net::Ipv4Addr;
use tokio::net::UdpSocket;

#[derive(Clone, Copy)]
enum Notice {
    Trying,
    Completed,
    Bye,
}

pub(super) fn answer() -> Result<Response, ControlError> {
    let wire = "SIP/2.0 200 OK\r\nVia: SIP/2.0/UDP 127.0.0.1:5061;branch=z9hG4bK-invite\r\nFrom: <sip:private-caller@localhost>;tag=private-remote\r\nTo: <sip:private-target@localhost>;tag=private-local\r\nCall-ID: private-dialog\r\nCSeq: 2 INVITE\r\nContact: <sip:private-contact@127.0.0.1:5060>\r\nContent-Length: 0\r\n\r\n";
    match SipMessage::try_from(wire).map_err(failure)? {
        SipMessage::Response(response) => Ok(response),
        SipMessage::Request(_) => Err(ControlError::Invalid("Expected fixture response")),
    }
}

async fn observed(notices: &[Notice]) -> Result<Observation, ControlError> {
    let client = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .map_err(failure)?;
    let server = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .map_err(failure)?;
    let local = client.local_addr().map_err(failure)?;
    let target = server.local_addr().map_err(failure)?;
    let mut result = Wait::Ready.bounded(async {
        let answer = answer()?;
        let mut signals = TransferSignals::new(&answer, RequestRoute::Direct(target))?;
        assert_eq!(signals.observed(), Observation {
            refer_response: None,
            event: None,
            initial_notify: false,
            final_notify: false,
            server_bye: false,
            transferor_bye_response: false,
        });
        let accepted = format!("SIP/2.0 202 Accepted\r\nVia: SIP/2.0/UDP {local};branch=z9hG4bK-refer\r\nFrom: {}\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: 3 REFER\r\nContent-Length: 0\r\n\r\n", answer.from_header().map_err(failure)?.value(), answer.to_header().map_err(failure)?.value(), answer.call_id_header().map_err(failure)?.value());
        server.send_to(accepted.as_bytes(), local).await.map_err(failure)?;
        signals.receive(&client, target).await?;
        let mut bytes = [0; 16385];
        for notice in notices {
            let (method, sequence, extra, body) = match notice {
                Notice::Trying => ("NOTIFY", 1, "Event: refer\r\nSubscription-State: active;expires=60\r\nContent-Type: message/sipfrag\r\n", "SIP/2.0 100 Trying\r\n"),
                Notice::Completed => ("NOTIFY", 2, "Event: refer\r\nSubscription-State: terminated;reason=noresource\r\nContent-Type: message/sipfrag\r\n", "SIP/2.0 200 OK\r\n"),
                Notice::Bye => ("BYE", 3, "", ""),
            };
            let request = format!("{method} sip:private-caller@{local} SIP/2.0\r\nVia: SIP/2.0/UDP {target};branch=z9hG4bK-notice-{sequence}\r\nFrom: {}\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: {sequence} {method}\r\nMax-Forwards: 70\r\n{extra}Content-Length: {}\r\n\r\n{body}", answer.to_header().map_err(failure)?.value(), answer.from_header().map_err(failure)?.value(), answer.call_id_header().map_err(failure)?.value(), body.len());
            server.send_to(request.as_bytes(), local).await.map_err(failure)?;
            signals.receive(&client, target).await?;
            let (size, peer) = server.recv_from(&mut bytes).await.map_err(failure)?;
            assert_eq!(peer, local);
            let response = match SipMessage::try_from(&bytes[..size]).map_err(failure)? {
                SipMessage::Response(response) => response,
                SipMessage::Request(_) => return Err(ControlError::Invalid("Expected notice response")),
            };
            assert_eq!(response.status_code.code(), 200);
            assert_eq!(response.cseq_header().map_err(failure)?.value(), format!("{sequence} {method}"));
            assert_eq!(response.call_id_header().map_err(failure)?.value(), "private-dialog");
        }
        Ok(signals.observed())
    }).await.map_err(failure).and_then(|result| result);
    drop(client);
    drop(server);
    let mut rebound = Vec::with_capacity(2);
    for address in [local, target] {
        match UdpSocket::bind(address).await {
            Ok(socket) => rebound.push(socket),
            Err(error) => result = combine(result, Err(failure(error))),
        }
    }
    result
}

#[tokio::test]
async fn completed_notifications_do_not_imply_a_received_bye() -> Result<(), ControlError> {
    let observation = observed(&[Notice::Trying, Notice::Completed]).await?;
    assert_eq!(
        observation,
        Observation {
            refer_response: Some(202),
            event: Some(ReferEvent::Bare),
            initial_notify: true,
            final_notify: true,
            server_bye: false,
            transferor_bye_response: false,
        }
    );
    let displayed = format!("{observation:?}");
    assert!(displayed.len() <= 256);
    assert!(!displayed.contains("private-"));
    assert!(displayed.contains("server_bye: false"));
    Ok(())
}

#[tokio::test]
async fn notify_and_bye_observations_preserve_every_arrival_order() -> Result<(), ControlError> {
    use Notice::{Bye, Completed, Trying};
    for notices in [
        [Trying, Completed, Bye],
        [Trying, Bye, Completed],
        [Completed, Trying, Bye],
        [Completed, Bye, Trying],
        [Bye, Trying, Completed],
        [Bye, Completed, Trying],
    ] {
        assert_eq!(
            observed(&notices).await?,
            Observation {
                refer_response: Some(202),
                event: Some(ReferEvent::Bare),
                initial_notify: true,
                final_notify: true,
                server_bye: true,
                transferor_bye_response: false,
            }
        );
    }
    Ok(())
}
