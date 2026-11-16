use super::failure;
use rsipstack::sip::{HeadersExt, Method, Request, SipMessage};
use rusterisk_server::ControlError;
use std::{net::SocketAddr, time::Duration};
use tokio::net::UdpSocket;

#[derive(Default)]
pub(super) struct TransferSignals {
    response: Option<u16>,
    initial: bool,
    ended: bool,
    completed: bool,
}

impl TransferSignals {
    async fn receive(
        &mut self,
        socket: &UdpSocket,
        target: SocketAddr,
    ) -> Result<(), ControlError> {
        let mut bytes = [0; 16385];
        let (size, peer) = socket.recv_from(&mut bytes).await.map_err(failure)?;
        if peer != target {
            return Err(ControlError::Invalid("Transfer response changed its peer"));
        }
        match SipMessage::try_from(&bytes[..size]).map_err(failure)? {
            SipMessage::Response(response)
                if response.cseq_header().map_err(failure)?.value() == "3 REFER" =>
            {
                let code = response.status_code.code();
                if self.response.replace(code).is_some_and(|old| old != code) {
                    return Err(ControlError::Invalid("REFER changed its final response"));
                }
            }
            SipMessage::Request(request)
                if matches!(request.method, Method::Notify | Method::Bye) =>
            {
                self.request(&request)?;
                let response = format!("SIP/2.0 200 OK\r\nVia: {}\r\nFrom: {}\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: {}\r\nContent-Length: 0\r\n\r\n", request.top_via_header().map_err(failure)?.value(), request.from_header().map_err(failure)?.value(), request.to_header().map_err(failure)?.value(), request.call_id_header().map_err(failure)?.value(), request.cseq_header().map_err(failure)?.value());
                socket
                    .send_to(response.as_bytes(), peer)
                    .await
                    .map_err(failure)?;
            }
            SipMessage::Response(_) | SipMessage::Request(_) => {
                return Err(ControlError::Invalid("Expected transfer signaling"));
            }
        }
        Ok(())
    }

    fn request(&mut self, request: &Request) -> Result<(), ControlError> {
        if request.method == Method::Bye {
            self.ended = true;
            return Ok(());
        }
        if !request.headers.iter().any(|header| {
            header.name().eq_ignore_ascii_case("Event") && header.value() == "refer;id=3"
        }) || !request.headers.iter().any(|header| {
            header.name().eq_ignore_ascii_case("Content-Type")
                && header.value().split(';').next() == Some("message/sipfrag")
        }) {
            return Err(ControlError::Invalid(
                "Unexpected REFER notification headers",
            ));
        }
        match request.body.as_slice() {
            b"SIP/2.0 100 Trying\r\n" => self.initial = true,
            b"SIP/2.0 180 Ringing\r\n" => {}
            b"SIP/2.0 200 OK\r\n"
                if request.headers.iter().any(|header| {
                    header.name().eq_ignore_ascii_case("Subscription-State")
                        && header.value() == "terminated;reason=noresource"
                }) =>
            {
                self.completed = true;
            }
            _ => {
                return Err(ControlError::Invalid("Unexpected transfer progress"));
            }
        }
        Ok(())
    }

    async fn until(
        &mut self,
        socket: &UdpSocket,
        target: SocketAddr,
        observed: impl Fn(&Self) -> bool,
    ) -> Result<(), ControlError> {
        tokio::time::timeout(Duration::from_secs(2), async {
            for _ in 0..16 {
                if observed(self) {
                    return Ok(());
                }
                self.receive(socket, target).await?;
            }
            Err(ControlError::Capacity)
        })
        .await
        .map_err(failure)?
    }

    pub(super) async fn accepted(
        &mut self,
        socket: &UdpSocket,
        target: SocketAddr,
    ) -> Result<(), ControlError> {
        self.until(socket, target, |state| state.response.is_some())
            .await?;
        match self.response {
            Some(202) => Ok(()),
            Some(code) => Err(failure(format!("Expected SIP 202, received {code}"))),
            None => Err(ControlError::Invalid("Missing REFER response")),
        }
    }

    pub(super) async fn source_ended(
        &mut self,
        socket: &UdpSocket,
        target: SocketAddr,
    ) -> Result<(), ControlError> {
        self.until(socket, target, |state| state.initial && state.ended)
            .await?;
        if self.completed {
            return Err(ControlError::Invalid(
                "Transfer completed before its target answered",
            ));
        }
        Ok(())
    }

    pub(super) async fn completed(
        &mut self,
        socket: &UdpSocket,
        target: SocketAddr,
    ) -> Result<(), ControlError> {
        self.until(socket, target, |state| state.completed).await
    }
}

pub(super) async fn acknowledge(
    socket: &UdpSocket,
    target: SocketAddr,
) -> Result<(), ControlError> {
    let mut bytes = [0; 16385];
    let (size, peer) = tokio::time::timeout(Duration::from_secs(2), socket.recv_from(&mut bytes))
        .await
        .map_err(failure)?
        .map_err(failure)?;
    if peer != target
        || !matches!(SipMessage::try_from(&bytes[..size]).map_err(failure)?, SipMessage::Request(request) if request.method == Method::Ack)
    {
        return Err(ControlError::Invalid(
            "Missing transfer target acknowledgement",
        ));
    }
    Ok(())
}
