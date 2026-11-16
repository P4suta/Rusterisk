use crate::failure;
use rsipstack::sip::{
    headers::{typed, untyped::ToTypedHeader},
    HeadersExt, Method, Request,
};
use rusterisk_server::ControlError;
use std::net::SocketAddr;

pub(super) struct AckIdentity {
    peer: SocketAddr,
    from: typed::From,
    to: typed::To,
    call_id: String,
    sequence: u32,
}

impl AckIdentity {
    pub(super) fn for_invitation(
        invite: &Request,
        peer: SocketAddr,
        tagged_to: String,
    ) -> Result<Self, ControlError> {
        let sequence = invite
            .cseq_header()
            .map_err(failure)?
            .typed()
            .map_err(failure)?;
        if invite.method != Method::Invite || sequence.method != Method::Invite {
            return Err(ControlError::Invalid(
                "Acknowledgement requires an invitation",
            ));
        }
        Ok(Self {
            peer,
            from: invite
                .from_header()
                .map_err(failure)?
                .typed()
                .map_err(failure)?,
            to: typed::To::parse(&tagged_to).map_err(failure)?,
            call_id: invite.call_id_header().map_err(failure)?.value().into(),
            sequence: sequence.seq,
        })
    }

    pub(super) const fn peer(&self) -> SocketAddr {
        self.peer
    }

    pub(super) fn matches(&self, packet: &Request, peer: SocketAddr) -> Result<bool, ControlError> {
        if packet.method != Method::Ack || peer != self.peer {
            return Ok(false);
        }
        let sequence = packet
            .cseq_header()
            .map_err(failure)?
            .typed()
            .map_err(failure)?;
        let from = packet
            .from_header()
            .map_err(failure)?
            .typed()
            .map_err(failure)?;
        let to = packet
            .to_header()
            .map_err(failure)?
            .typed()
            .map_err(failure)?;
        Ok(sequence.method == Method::Ack
            && sequence.seq == self.sequence
            && from.uri == self.from.uri
            && from.tag() == self.from.tag()
            && to.uri == self.to.uri
            && to.tag() == self.to.tag()
            && packet.call_id_header().map_err(failure)?.value() == self.call_id)
    }
}

#[cfg(test)]
mod tests {
    use super::AckIdentity;
    use crate::failure;
    use rsipstack::sip::{Request, SipMessage};
    use rusterisk_server::ControlError;
    use std::net::SocketAddr;

    fn packet(
        method: &str,
        from: &str,
        to: &str,
        call_id: &str,
        sequence: &str,
    ) -> Result<Request, ControlError> {
        let uri = if method == "INVITE" {
            "sip:charlie@localhost"
        } else {
            "sip:phone@localhost"
        };
        let bytes = format!("{method} {uri} SIP/2.0\r\nVia: SIP/2.0/UDP 127.0.0.1:5060;branch=z9hG4bK-{method}\r\nFrom: {from}\r\nTo: {to}\r\nCall-ID: {call_id}\r\nCSeq: {sequence}\r\nMax-Forwards: 70\r\nContent-Length: 0\r\n\r\n");
        match SipMessage::try_from(bytes.as_str()).map_err(failure)? {
            SipMessage::Request(request) => Ok(request),
            SipMessage::Response(_) => Err(ControlError::Invalid("Expected a fixture request")),
        }
    }

    #[test]
    fn acknowledgement_uses_the_invitation_dialog_and_sequence_with_its_own_branch(
    ) -> Result<(), ControlError> {
        let peer = SocketAddr::from(([127, 0, 0, 1], 5060));
        let from = "<sip:server@localhost>;tag=source";
        let to = "<sip:charlie@localhost>;tag=phone";
        let invite = packet(
            "INVITE",
            from,
            "<sip:charlie@localhost>",
            "target-dialog",
            "17 INVITE",
        )?;
        let expected = AckIdentity::for_invitation(&invite, peer, to.into())?;
        assert_eq!(expected.peer(), peer);
        let ack = packet("ACK", from, to, "target-dialog", "17 ACK")?;
        assert!(expected.matches(&ack, peer)?);
        assert!(!expected.matches(&ack, SocketAddr::from(([127, 0, 0, 1], 5061)))?);
        for (method, from, to, call_id, sequence) in [
            (
                "ACK",
                "<sip:server@localhost>;tag=different",
                to,
                "target-dialog",
                "17 ACK",
            ),
            (
                "ACK",
                "<sip:other@localhost>;tag=source",
                to,
                "target-dialog",
                "17 ACK",
            ),
            (
                "ACK",
                from,
                "<sip:charlie@localhost>;tag=different",
                "target-dialog",
                "17 ACK",
            ),
            (
                "ACK",
                from,
                "<sip:other@localhost>;tag=phone",
                "target-dialog",
                "17 ACK",
            ),
            ("ACK", from, to, "different-dialog", "17 ACK"),
            ("ACK", from, to, "target-dialog", "18 ACK"),
            ("BYE", from, to, "target-dialog", "17 BYE"),
        ] {
            assert!(!expected.matches(&packet(method, from, to, call_id, sequence)?, peer)?);
        }
        assert!(packet("ACK", from, to, "target-dialog", "17 INVITE").is_err());
        Ok(())
    }

    #[test]
    fn acknowledgement_identity_ignores_display_names_and_header_brackets(
    ) -> Result<(), ControlError> {
        let peer = SocketAddr::from(([127, 0, 0, 1], 5060));
        let invite = packet(
            "INVITE",
            "<sip:server@localhost>;tag=source",
            "<sip:charlie@localhost>",
            "target-dialog",
            "17 INVITE",
        )?;
        let expected =
            AckIdentity::for_invitation(&invite, peer, "<sip:charlie@localhost>;tag=phone".into())?;
        for (from, to) in [
            (
                "\"Updated\" <sip:server@localhost>;tag=source",
                "\"Charlie\" <sip:charlie@localhost>;tag=phone",
            ),
            (
                "sip:server@localhost;tag=source",
                "sip:charlie@localhost;tag=phone",
            ),
        ] {
            assert!(expected.matches(&packet("ACK", from, to, "target-dialog", "17 ACK")?, peer)?);
        }
        Ok(())
    }

    #[test]
    fn acknowledgement_identity_requires_an_invite_method_and_cseq() -> Result<(), ControlError> {
        let peer = SocketAddr::from(([127, 0, 0, 1], 5060));
        for (method, sequence) in [("ACK", "17 ACK"), ("BYE", "17 BYE")] {
            let request = packet(
                method,
                "<sip:server@localhost>;tag=source",
                "<sip:charlie@localhost>",
                "target-dialog",
                sequence,
            )?;
            assert!(AckIdentity::for_invitation(
                &request,
                peer,
                "<sip:charlie@localhost>;tag=phone".into()
            )
            .is_err());
        }
        assert!(packet(
            "INVITE",
            "<sip:server@localhost>;tag=source",
            "<sip:charlie@localhost>",
            "target-dialog",
            "17 ACK"
        )
        .is_err());
        Ok(())
    }
}
