use super::failure;
use crate::transfer_transactions::Branch;
use crate::AckIdentity;
use crate::BranchNamespace;
use rsipstack::sip::{
    headers::untyped::ToTypedHeader, HeadersExt, Method, Request, Response, SipMessage, Uri,
};
use rusterisk_server::ControlError;
use rusterisk_test_support::Wait;
use std::net::SocketAddr;
use tokio::net::UdpSocket;
#[path = "observations.rs"]
mod observation_tests;
#[path = "retirement.rs"]
mod retirement_tests;
#[path = "routes.rs"]
mod route_tests;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum ReferEvent {
    Bare,
    Numbered,
}

pub(super) enum RequestRoute {
    Direct(SocketAddr),
    CallbackPending { server: SocketAddr },
    CallbackPinned(SocketAddr),
}

impl RequestRoute {
    pub(super) fn allows(&mut self, peer: SocketAddr) -> bool {
        match *self {
            Self::Direct(expected) | Self::CallbackPinned(expected) => peer == expected,
            Self::CallbackPending { server }
                if peer == server
                    || (peer.is_ipv4() && peer.ip().is_loopback() && peer.port() != 0) =>
            {
                *self = Self::CallbackPinned(peer);
                true
            }
            Self::CallbackPending { .. } => false,
        }
    }
}

enum NotifyProgress {
    Trying,
    Ringing,
    Answered,
}

enum Notice {
    Bye,
    Notify {
        event: ReferEvent,
        progress: NotifyProgress,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Observation {
    refer_response: Option<u16>,
    event: Option<ReferEvent>,
    initial_notify: bool,
    final_notify: bool,
    server_bye: bool,
    transferor_bye_response: bool,
}

struct TransferorBye {
    branch: Branch,
    sent_by: SocketAddr,
}

pub(super) struct TransferSignals {
    from: String,
    to: String,
    call_id: String,
    remote: Uri,
    requests: RequestRoute,
    event: Option<ReferEvent>,
    response: Option<u16>,
    initial: bool,
    ended: bool,
    completed: bool,
    bye_response: bool,
    bye_transaction: Option<TransferorBye>,
}

impl TransferSignals {
    pub(super) const fn observed(&self) -> Observation {
        Observation {
            refer_response: self.response,
            event: self.event,
            initial_notify: self.initial,
            final_notify: self.completed,
            server_bye: self.ended,
            transferor_bye_response: self.bye_response,
        }
    }

    pub(super) fn new(answer: &Response, requests: RequestRoute) -> Result<Self, ControlError> {
        Ok(Self {
            from: answer.to_header().map_err(failure)?.value().into(),
            to: answer.from_header().map_err(failure)?.value().into(),
            call_id: answer.call_id_header().map_err(failure)?.value().into(),
            remote: answer
                .contact_header()
                .map_err(failure)?
                .typed()
                .map_err(failure)?
                .uri,
            requests,
            event: None,
            response: None,
            initial: false,
            ended: false,
            completed: false,
            bye_response: false,
            bye_transaction: None,
        })
    }

    pub(super) fn event(&self) -> Result<ReferEvent, ControlError> {
        self.event
            .ok_or(ControlError::Invalid("Missing REFER event identity"))
    }

    pub(super) fn received_initial_notify(&self) -> bool {
        self.initial
    }

    pub(super) fn received_server_bye(&self) -> bool {
        self.ended
    }

    async fn receive(
        &mut self,
        socket: &UdpSocket,
        target: SocketAddr,
    ) -> Result<(), ControlError> {
        let mut bytes = [0; 16385];
        let (size, peer) = socket.recv_from(&mut bytes).await.map_err(failure)?;
        let message = SipMessage::try_from(&bytes[..size]).map_err(failure)?;
        if let SipMessage::Response(response) = &message {
            if peer != target {
                return Err(ControlError::Invalid("Transfer response changed its peer"));
            }
            if response.from_header().map_err(failure)?.value() != self.to
                || response.to_header().map_err(failure)?.value() != self.from
                || response.call_id_header().map_err(failure)?.value() != self.call_id
            {
                return Err(ControlError::Invalid(
                    "Transfer response changed its dialog",
                ));
            }
        }
        match message {
            SipMessage::Response(response)
                if response.cseq_header().map_err(failure)?.value() == "3 REFER" =>
            {
                let code = response.status_code.code();
                if self.response.replace(code).is_some_and(|old| old != code) {
                    return Err(ControlError::Invalid("REFER changed its final response"));
                }
            }
            SipMessage::Response(response)
                if response.cseq_header().map_err(failure)?.value() == "4 BYE" =>
            {
                let expected = self
                    .bye_transaction
                    .as_ref()
                    .ok_or(ControlError::Invalid("Unsolicited transferor BYE response"))?;
                let via = response
                    .top_via_header()
                    .map_err(failure)?
                    .typed()
                    .map_err(failure)?;
                let branch = expected.branch.to_string();
                if via.branch().map(|branch| branch.value()) != Some(branch.as_str())
                    || via.sent_by() != &expected.sent_by.into()
                {
                    return Err(ControlError::Invalid(
                        "Transferor BYE response changed its transaction",
                    ));
                }
                let code = response.status_code.code();
                if code != 200 {
                    return Err(failure(format!(
                        "Expected SIP 200 for transferor BYE, received {code}"
                    )));
                }
                self.bye_response = true;
            }
            SipMessage::Request(request)
                if matches!(request.method, Method::Notify | Method::Bye) =>
            {
                let notice = self.request(&request)?;
                if !self.requests.allows(peer) {
                    return Err(failure(format!(
                        "Transfer request changed its callback peer to {peer}"
                    )));
                }
                match notice {
                    Notice::Bye => self.ended = true,
                    Notice::Notify { event, progress } => {
                        self.event = Some(event);
                        match progress {
                            NotifyProgress::Trying => self.initial = true,
                            NotifyProgress::Ringing => {}
                            NotifyProgress::Answered => self.completed = true,
                        }
                    }
                }
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

    fn request(&self, request: &Request) -> Result<Notice, ControlError> {
        if request.from_header().map_err(failure)?.value() != self.from
            || request.to_header().map_err(failure)?.value() != self.to
            || request.call_id_header().map_err(failure)?.value() != self.call_id
        {
            return Err(ControlError::Invalid(
                "Transfer notification changed its dialog",
            ));
        }
        if request.method == Method::Bye {
            return Ok(Notice::Bye);
        }
        let mut events = request
            .headers
            .iter()
            .filter(|header| header.name().eq_ignore_ascii_case("Event"));
        let event = match (events.next().map(|header| header.value()), events.next()) {
            (Some("refer"), None) => ReferEvent::Bare,
            (Some("refer;id=3"), None) => ReferEvent::Numbered,
            _ => return Err(ControlError::Invalid("Unexpected REFER event identity")),
        };
        if self.event.is_some_and(|old| old != event) {
            return Err(ControlError::Invalid("REFER event identity changed"));
        }
        if !request.headers.iter().any(|header| {
            header.name().eq_ignore_ascii_case("Content-Type")
                && header.value().split(';').next() == Some("message/sipfrag")
        }) {
            return Err(ControlError::Invalid(
                "Unexpected REFER notification headers",
            ));
        }
        let progress = match request.body.as_slice() {
            b"SIP/2.0 100 Trying\r\n" => NotifyProgress::Trying,
            b"SIP/2.0 180 Ringing\r\n" => NotifyProgress::Ringing,
            b"SIP/2.0 200 OK\r\n"
                if request.headers.iter().any(|header| {
                    header.name().eq_ignore_ascii_case("Subscription-State")
                        && header.value() == "terminated;reason=noresource"
                }) =>
            {
                NotifyProgress::Answered
            }
            _ => {
                return Err(ControlError::Invalid("Unexpected transfer progress"));
            }
        };
        Ok(Notice::Notify { event, progress })
    }

    async fn until(
        &mut self,
        socket: &UdpSocket,
        target: SocketAddr,
        observed: impl Fn(&Self) -> bool,
    ) -> Result<(), ControlError> {
        Wait::Ready
            .bounded(async {
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
            .await
            .map_err(|error| {
                failure(format!(
                    "{error}; initial_notify={}, source_bye={}, final_notify={}, event={:?}",
                    self.initial, self.ended, self.completed, self.event
                ))
            })?;
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
        self.until(socket, target, |state| state.initial && state.completed)
            .await
    }

    pub(super) async fn settled(
        &mut self,
        socket: &UdpSocket,
        target: SocketAddr,
    ) -> Result<(), ControlError> {
        self.until(socket, target, |state| {
            state.initial && state.completed && state.ended
        })
        .await
    }

    pub(super) async fn consultation_ended(
        &mut self,
        socket: &UdpSocket,
        target: SocketAddr,
    ) -> Result<(), ControlError> {
        self.until(socket, target, |state| state.ended).await?;
        if self.response.is_some() || self.initial || self.completed || self.event.is_some() {
            return Err(ControlError::Invalid(
                "Consultation received another dialog's transfer progress",
            ));
        }
        Ok(())
    }

    async fn send_transferor_bye(
        &mut self,
        socket: &UdpSocket,
        target: SocketAddr,
        local: SocketAddr,
    ) -> Result<(), ControlError> {
        let branch = BranchNamespace::random()?.branch(0);
        self.bye_transaction = Some(TransferorBye {
            branch,
            sent_by: local,
        });
        let request = format!("BYE {} SIP/2.0\r\nVia: SIP/2.0/UDP {local};branch={branch};rport\r\nFrom: {}\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: 4 BYE\r\nMax-Forwards: 70\r\nContent-Length: 0\r\n\r\n", self.remote, self.to, self.from, self.call_id);
        socket
            .send_to(request.as_bytes(), target)
            .await
            .map_err(failure)?;
        self.until(socket, target, |state| state.bye_response).await
    }

    pub(super) async fn transferor_ended(
        &mut self,
        socket: &UdpSocket,
        target: SocketAddr,
        local: SocketAddr,
    ) -> Result<(), ControlError> {
        self.send_transferor_bye(socket, target, local).await?;
        if self.completed {
            return Err(ControlError::Invalid(
                "Transfer completed before its target answered",
            ));
        }
        Ok(())
    }

    pub(super) async fn retire_source(
        &mut self,
        socket: &UdpSocket,
        target: SocketAddr,
        local: SocketAddr,
    ) -> Result<(), ControlError> {
        self.completed(socket, target).await?;
        if !self.ended && !self.bye_response {
            self.send_transferor_bye(socket, target, local).await?;
        }
        Ok(())
    }
}

pub(super) async fn acknowledge(
    socket: &UdpSocket,
    expected: &AckIdentity,
) -> Result<(), ControlError> {
    let mut bytes = [0; 16385];
    let (size, peer) = Wait::Ready
        .bounded(socket.recv_from(&mut bytes))
        .await
        .map_err(failure)?
        .map_err(failure)?;
    let target = expected.peer();
    match SipMessage::try_from(&bytes[..size]).map_err(failure)? {
        SipMessage::Request(request) if expected.matches(&request, peer)? => Ok(()),
        SipMessage::Request(request) => Err(failure(format!(
            "Expected transfer target ACK from {target}, received {} from {peer}",
            request.method
        ))),
        SipMessage::Response(response) => Err(failure(format!(
            "Expected transfer target ACK from {target}, received response {} from {peer}",
            response.status_code.code()
        ))),
    }
}
