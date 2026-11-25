use super::{observation_tests::answer, Observation, RequestRoute, TransferSignals};
use crate::{failure, transfer_lifetime::combine};
use rsipstack::sip::{headers::untyped::ToTypedHeader, HeadersExt, Method, SipMessage};
use rusterisk_server::ControlError;
use rusterisk_test_support::Wait;
use std::{io::ErrorKind, net::Ipv4Addr};
use tokio::net::UdpSocket;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Reply {
    Matching,
    OtherBranch,
    OtherSentBy,
    OtherDialog,
    OtherSequence,
    Failed,
    CrossedBye,
    AlreadyEnded,
    Unsolicited,
}

fn notification(signals: &TransferSignals, via: &str, sequence: u32, completed: bool) -> String {
    let (state, status) = if completed {
        ("terminated;reason=noresource", "200 OK")
    } else {
        ("active;expires=60", "100 Trying")
    };
    let body = format!("SIP/2.0 {status}\r\n");
    format!("NOTIFY {} SIP/2.0\r\nVia: {via}\r\nFrom: {}\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: {sequence} NOTIFY\r\nEvent: refer\r\nSubscription-State: {state}\r\nContent-Type: message/sipfrag\r\nContent-Length: {}\r\n\r\n{body}", signals.remote, signals.from, signals.to, signals.call_id, body.len())
}

fn remote_bye(signals: &TransferSignals, via: &str) -> String {
    format!("BYE {} SIP/2.0\r\nVia: {via}\r\nFrom: {}\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: 9 BYE\r\nContent-Length: 0\r\n\r\n", signals.remote, signals.from, signals.to, signals.call_id)
}

async fn acknowledgement(server: &UdpSocket, expected_cseq: &str) -> Result<(), ControlError> {
    let mut bytes = [0; 16385];
    let (size, _) = server.recv_from(&mut bytes).await.map_err(failure)?;
    let SipMessage::Response(response) = SipMessage::try_from(&bytes[..size]).map_err(failure)?
    else {
        return Err(ControlError::Invalid("Expected retirement notice response"));
    };
    if response.status_code.code() != 200
        || response.cseq_header().map_err(failure)?.value() != expected_cseq
    {
        return Err(ControlError::Invalid("Retirement notice response mismatch"));
    }
    Ok(())
}

struct Retirement {
    result: Result<(), ControlError>,
    observed: Observation,
}

async fn retire(reply: Reply) -> Result<Retirement, ControlError> {
    let client = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .map_err(failure)?;
    let server = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .map_err(failure)?;
    let local = client.local_addr().map_err(failure)?;
    let target = server.local_addr().map_err(failure)?;
    let mut result = Wait::Ready.bounded(async {
        let mut signals = TransferSignals::new(&answer()?, RequestRoute::Direct(target))?;
        let via = format!("SIP/2.0/UDP {target};branch=z9hG4bK-retirement-notice");
        for (sequence, completed) in [(1, false), (2, true)] {
            let notice = notification(&signals, &via, sequence, completed);
            server.send_to(notice.as_bytes(), local).await.map_err(failure)?;
            signals.receive(&client, target).await?;
            acknowledgement(&server, &format!("{sequence} NOTIFY")).await?;
        }
        let crossed_bye = remote_bye(&signals, &via);
        if reply == Reply::Unsolicited {
            let response = format!("SIP/2.0 200 OK\r\nVia: SIP/2.0/UDP {local};branch=z9hG4bK-unowned\r\nFrom: {}\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: 4 BYE\r\nContent-Length: 0\r\n\r\n", signals.to, signals.from, signals.call_id);
            server.send_to(response.as_bytes(), local).await.map_err(failure)?;
            let result = signals.receive(&client, target).await;
            return Ok(Retirement { result, observed: signals.observed() });
        }
        if reply == Reply::AlreadyEnded {
            server.send_to(crossed_bye.as_bytes(), local).await.map_err(failure)?;
            signals.receive(&client, target).await?;
            acknowledgement(&server, "9 BYE").await?;
            let result = signals.retire_source(&client, target, local).await;
            let mut bytes = [0; 16385];
            match server.try_recv_from(&mut bytes) {
                Err(error) if error.kind() == ErrorKind::WouldBlock => {}
                Ok(_) => return Err(ControlError::Invalid("Ended source sent another BYE")),
                Err(error) => return Err(failure(error)),
            }
            return Ok(Retirement { result, observed: signals.observed() });
        }
        let outgoing = async {
            let result = signals.retire_source(&client, target, local).await;
            if result.is_ok() {
                signals.retire_source(&client, target, local).await?;
            }
            Ok::<_, ControlError>(Retirement { result, observed: signals.observed() })
        };
        let incoming = async {
            let mut bytes = [0; 16385];
            let (size, peer) = server.recv_from(&mut bytes).await.map_err(failure)?;
            if peer != local {
                return Err(ControlError::Invalid("Source BYE changed its peer"));
            }
            let SipMessage::Request(request) = SipMessage::try_from(&bytes[..size]).map_err(failure)? else {
                return Err(ControlError::Invalid("Expected source BYE"));
            };
            if request.method != Method::Bye
                || request.cseq_header().map_err(failure)?.value() != "4 BYE"
            {
                return Err(ControlError::Invalid("Source BYE changed its method or sequence"));
            }
            let original_via = request.top_via_header().map_err(failure)?.value().to_owned();
            let typed_via = request.top_via_header().map_err(failure)?.typed().map_err(failure)?;
            let branch = typed_via.branch().ok_or(ControlError::Invalid("Missing source BYE branch"))?.value();
            let via = match reply {
                Reply::OtherBranch => original_via.replace(branch, "z9hG4bK-unrelated"),
                Reply::OtherSentBy => original_via.replace(&local.to_string(), "127.0.0.1:1"),
                Reply::Matching | Reply::OtherDialog | Reply::OtherSequence | Reply::Failed | Reply::CrossedBye | Reply::AlreadyEnded | Reply::Unsolicited => original_via,
            };
            let call_id = if reply == Reply::OtherDialog { "unrelated-dialog" } else { request.call_id_header().map_err(failure)?.value() };
            let cseq = if reply == Reply::OtherSequence { "5 BYE" } else { "4 BYE" };
            let status = if reply == Reply::Failed { "481 Call/Transaction Does Not Exist" } else { "200 OK" };
            let response = format!("SIP/2.0 {status}\r\nVia: {via}\r\nFrom: {}\r\nTo: {}\r\nCall-ID: {call_id}\r\nCSeq: {cseq}\r\nContent-Length: 0\r\n\r\n", request.from_header().map_err(failure)?.value(), request.to_header().map_err(failure)?.value());
            if reply == Reply::CrossedBye {
                server.send_to(crossed_bye.as_bytes(), local).await.map_err(failure)?;
                acknowledgement(&server, "9 BYE").await?;
            }
            server.send_to(response.as_bytes(), local).await.map_err(failure)?;
            Ok::<(), ControlError>(())
        };
        let (retirement, ()) = tokio::try_join!(outgoing, incoming)?;
        Ok(retirement)
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
async fn completed_transfer_requires_its_own_bye_transaction_response() -> Result<(), ControlError>
{
    for (reply, expected) in [
        (
            Reply::OtherBranch,
            ControlError::Invalid("Transferor BYE response changed its transaction"),
        ),
        (
            Reply::OtherSentBy,
            ControlError::Invalid("Transferor BYE response changed its transaction"),
        ),
        (
            Reply::OtherDialog,
            ControlError::Invalid("Transfer response changed its dialog"),
        ),
        (
            Reply::OtherSequence,
            ControlError::Invalid("Expected transfer signaling"),
        ),
        (
            Reply::Failed,
            failure("Expected SIP 200 for transferor BYE, received 481"),
        ),
    ] {
        let retirement = retire(reply).await?;
        assert_eq!(retirement.result, Err(expected));
        assert!(!retirement.observed.transferor_bye_response);
        assert!(retirement.observed.initial_notify);
        assert!(retirement.observed.final_notify);
    }
    Ok(())
}

#[tokio::test]
async fn completed_transfer_retires_source_with_a_matching_bye_response() -> Result<(), ControlError>
{
    for reply in [Reply::Matching, Reply::CrossedBye] {
        let retirement = retire(reply).await?;
        retirement.result?;
        assert!(retirement.observed.transferor_bye_response);
        assert_eq!(retirement.observed.server_bye, reply == Reply::CrossedBye);
        assert!(retirement.observed.initial_notify);
        assert!(retirement.observed.final_notify);
    }
    Ok(())
}

#[tokio::test]
async fn completed_source_bye_avoids_an_extra_transferor_bye() -> Result<(), ControlError> {
    let retirement = retire(Reply::AlreadyEnded).await?;
    retirement.result?;
    assert!(retirement.observed.server_bye);
    assert!(!retirement.observed.transferor_bye_response);
    Ok(())
}

#[tokio::test]
async fn a_bye_response_without_a_sent_transaction_is_rejected() -> Result<(), ControlError> {
    let retirement = retire(Reply::Unsolicited).await?;
    assert_eq!(
        retirement.result,
        Err(ControlError::Invalid("Unsolicited transferor BYE response"))
    );
    assert!(!retirement.observed.transferor_bye_response);
    Ok(())
}
