use super::{failure, ControlError, Peer};
use rsipstack::sip::{headers::untyped::ToTypedHeader, HeadersExt, Method, Request, SipMessage};
use std::{net::SocketAddr, time::Duration};

fn same_dialog(message: &SipMessage, local: &Request) -> Result<bool, ControlError> {
    let (received_from, received_to, received_id) = match message {
        SipMessage::Request(request) => (
            request.from_header().map_err(failure)?.value(),
            request.to_header().map_err(failure)?.value(),
            request.call_id_header().map_err(failure)?.value(),
        ),
        SipMessage::Response(response) => (
            response.from_header().map_err(failure)?.value(),
            response.to_header().map_err(failure)?.value(),
            response.call_id_header().map_err(failure)?.value(),
        ),
    };
    let (from, to) = if matches!(message, SipMessage::Request(_)) {
        (
            local.to_header().map_err(failure)?.value(),
            local.from_header().map_err(failure)?.value(),
        )
    } else {
        (
            local.from_header().map_err(failure)?.value(),
            local.to_header().map_err(failure)?.value(),
        )
    };
    Ok(received_from == from
        && received_to == to
        && received_id == local.call_id_header().map_err(failure)?.value())
}

async fn receive(peer: &Peer, target: SocketAddr) -> Result<SipMessage, ControlError> {
    let mut bytes = [0; 16385];
    let (size, sender) = peer.signal.recv_from(&mut bytes).await.map_err(failure)?;
    if sender != target {
        return Err(ControlError::Invalid("Surviving dialog changed its peer"));
    }
    SipMessage::try_from(&bytes[..size]).map_err(failure)
}

pub(super) async fn end(
    ending: &Peer,
    local: &Request,
    survivor: &Peer,
    surviving_dialog: &Request,
    targets: [SocketAddr; 2],
) -> Result<(), ControlError> {
    let [target, survivor_target] = targets;
    let bye = format!("BYE {} SIP/2.0\r\nVia: SIP/2.0/UDP {};branch=z9hG4bK-survivor-bye;rport\r\nFrom: {}\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: 3 BYE\r\nMax-Forwards: 70\r\nContent-Length: 0\r\n\r\n", local.uri, ending.contact()?, local.from_header().map_err(failure)?.value(), local.to_header().map_err(failure)?.value(), local.call_id_header().map_err(failure)?.value());
    ending
        .signal
        .send_to(bye.as_bytes(), target)
        .await
        .map_err(failure)?;
    tokio::time::timeout(Duration::from_secs(2), async {
        let response = async {
            let message = receive(ending, target).await?;
            if !same_dialog(&message, local)? {
                return Err(ControlError::Invalid("Survivor response changed its dialog"));
            }
            match message {
                SipMessage::Response(response)
                    if response.status_code.code() == 200
                        && response.cseq_header().map_err(failure)?.value() == "3 BYE" =>
                {
                    Ok(())
                }
                SipMessage::Response(_) | SipMessage::Request(_) => {
                    Err(ControlError::Invalid("Expected survivor BYE acknowledgement"))
                }
            }
        };
        let peer_ended = async {
            let message = receive(survivor, survivor_target).await?;
            if !same_dialog(&message, surviving_dialog)? {
                return Err(ControlError::Invalid("Surviving peer changed its dialog"));
            }
            let request = match message {
                SipMessage::Request(request) if request.method == Method::Bye => request,
                SipMessage::Request(_) | SipMessage::Response(_) => {
                    return Err(ControlError::Invalid("Expected surviving peer BYE"))
                }
            };
            if request.cseq_header().map_err(failure)?.typed().map_err(failure)?.method != Method::Bye {
                return Err(ControlError::Invalid("Surviving BYE changed its method"));
            }
            let response = format!("SIP/2.0 200 OK\r\nVia: {}\r\nFrom: {}\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: {}\r\nContent-Length: 0\r\n\r\n", request.top_via_header().map_err(failure)?.value(), request.from_header().map_err(failure)?.value(), request.to_header().map_err(failure)?.value(), request.call_id_header().map_err(failure)?.value(), request.cseq_header().map_err(failure)?.value());
            survivor.signal.send_to(response.as_bytes(), survivor_target).await.map_err(failure)?;
            Ok::<(), ControlError>(())
        };
        tokio::try_join!(response, peer_ended)?;
        Ok::<(), ControlError>(())
    })
    .await
    .map_err(failure)?
}
