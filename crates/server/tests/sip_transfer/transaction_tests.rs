use super::{failure, start_invitation, transfer_lifetime::combine, update_direction, Invitation};
use rsipstack::sip::{headers::untyped::ToTypedHeader, HeadersExt, Method, Request, SipMessage};
use rusterisk_server::rtp::SocketPair;
use rusterisk_server::ControlError;
use std::{collections::BTreeSet, net::Ipv4Addr, time::Duration};
use tokio::net::UdpSocket;

async fn request(socket: &UdpSocket) -> Result<Request, ControlError> {
    let mut bytes = [0; 16385];
    let (size, _) = socket.recv_from(&mut bytes).await.map_err(failure)?;
    match SipMessage::try_from(&bytes[..size]).map_err(failure)? {
        SipMessage::Request(request) => Ok(request),
        SipMessage::Response(_) => Err(ControlError::Invalid("Expected fixture request")),
    }
}

async fn invitation(call_id: &str, from_tag: &str) -> Result<[Request; 3], ControlError> {
    let client = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .map_err(failure)?;
    let server = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .map_err(failure)?;
    let local = client.local_addr().map_err(failure)?;
    let target = server.local_addr().map_err(failure)?;
    let mut result = tokio::time::timeout(Duration::from_secs(2), async {
        let incoming = async {
            let first = request(&server).await?;
            let challenge = format!("SIP/2.0 401 Unauthorized\r\nVia: {}\r\nFrom: {}\r\nTo: {};tag=challenge\r\nCall-ID: {}\r\nCSeq: {}\r\nWWW-Authenticate: Digest realm=\"fixture\", nonce=\"nonce\", algorithm=MD5, qop=\"auth\"\r\nContent-Length: 0\r\n\r\n", first.top_via_header().map_err(failure)?.value(), first.from_header().map_err(failure)?.value(), first.to_header().map_err(failure)?.value(), first.call_id_header().map_err(failure)?.value(), first.cseq_header().map_err(failure)?.value());
            server.send_to(challenge.as_bytes(), local).await.map_err(failure)?;
            let acknowledgement = request(&server).await?;
            let authenticated = request(&server).await?;
            Ok::<_, ControlError>([first, acknowledgement, authenticated])
        };
        let outgoing = start_invitation(
            &client,
            target,
            local,
            "127.0.0.1:10000".parse().map_err(failure)?,
            Invitation { extension: "100", call_id, from_tag },
        );
        let ((), captured) = tokio::try_join!(outgoing, incoming)?;
        Ok(captured)
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
async fn independent_transfer_parties_use_distinct_invite_transaction_cookies(
) -> Result<(), ControlError> {
    let mut branches = BTreeSet::new();
    for (call_id, from_tag) in [
        ("incoming-test", "caller"),
        ("attended-source", "source-caller"),
        ("attended-consultation", "consultation-caller"),
        ("bob-test", "bob"),
        ("charlie-test", "charlie"),
    ] {
        let [first, acknowledgement, authenticated] = invitation(call_id, from_tag).await?;
        assert_eq!(first.method, Method::Invite);
        assert_eq!(acknowledgement.method, Method::Ack);
        assert_eq!(authenticated.method, Method::Invite);
        let first_via = first
            .top_via_header()
            .map_err(failure)?
            .typed()
            .map_err(failure)?;
        let ack_via = acknowledgement
            .top_via_header()
            .map_err(failure)?
            .typed()
            .map_err(failure)?;
        let next_via = authenticated
            .top_via_header()
            .map_err(failure)?
            .typed()
            .map_err(failure)?;
        assert_eq!(first_via.sent_by(), ack_via.sent_by());
        assert_eq!(first_via.sent_by(), next_via.sent_by());
        assert_eq!(first_via.branch(), ack_via.branch());
        for (packet, via, sequence) in [(&first, first_via, 1), (&authenticated, next_via, 2)] {
            assert_eq!(packet.call_id_header().map_err(failure)?.value(), call_id);
            assert_eq!(
                packet
                    .from_header()
                    .map_err(failure)?
                    .typed()
                    .map_err(failure)?
                    .tag()
                    .map(|tag| tag.value()),
                Some(from_tag)
            );
            assert_eq!(
                packet
                    .cseq_header()
                    .map_err(failure)?
                    .typed()
                    .map_err(failure)?
                    .seq,
                sequence
            );
            let branch = via
                .branch()
                .ok_or(ControlError::Invalid("Missing transaction branch"))?
                .value();
            assert!(branch.starts_with("z9hG4bK"));
            assert!(
                branches.insert(branch.to_owned()),
                "INVITE cookie branch reused between independent parties"
            );
        }
    }
    assert_eq!(branches.len(), 10);
    Ok(())
}

#[tokio::test]
async fn direction_updates_replace_the_actual_ack_branch_for_each_transaction(
) -> Result<(), ControlError> {
    let client = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .map_err(failure)?;
    let server = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
        .await
        .map_err(failure)?;
    let audio = SocketPair::bind(Ipv4Addr::LOCALHOST.into(), 0, 0).map_err(failure)?;
    let local = client.local_addr().map_err(failure)?;
    let target = server.local_addr().map_err(failure)?;
    let addresses = [
        local,
        target,
        audio.local_addr().map_err(failure)?,
        audio.control_addr().map_err(failure)?,
    ];
    let original_branch = "z9hG4bK-0123456789abcdef0123456789abcdef-00000000";
    let ack = format!("ACK sip:fixture@{target} SIP/2.0\r\nVia: SIP/2.0/UDP {local};branch={original_branch};rport\r\nFrom: <sip:bob@localhost>;tag=bob\r\nTo: <sip:fixture@localhost>;tag=fixture\r\nCall-ID: direction-update\r\nCSeq: 2 ACK\r\nMax-Forwards: 70\r\nContent-Length: 0\r\n\r\n");
    let mut result = tokio::time::timeout(Duration::from_secs(2), async {
        let incoming = async {
            let mut packets = Vec::with_capacity(4);
            for _ in 0..2 {
                let invite = request(&server).await?;
                let response = format!("SIP/2.0 200 OK\r\nVia: {}\r\nFrom: {}\r\nTo: {}\r\nCall-ID: {}\r\nCSeq: {}\r\nContent-Length: 0\r\n\r\n", invite.top_via_header().map_err(failure)?.value(), invite.from_header().map_err(failure)?.value(), invite.to_header().map_err(failure)?.value(), invite.call_id_header().map_err(failure)?.value(), invite.cseq_header().map_err(failure)?.value());
                server.send_to(response.as_bytes(), local).await.map_err(failure)?;
                packets.push(invite);
                packets.push(request(&server).await?);
            }
            Ok::<_, ControlError>(packets)
        };
        let outgoing = async {
            for (sequence, version, direction) in [(1, 2, "sendonly"), (2, 3, "sendrecv")] {
                update_direction(&client, target, &ack, &audio, sequence, version, direction).await?;
            }
            Ok::<_, ControlError>(())
        };
        let ((), packets) = tokio::try_join!(outgoing, incoming)?;
        Ok(packets)
    }).await.map_err(failure).and_then(|result| result);
    drop(client);
    drop(server);
    drop(audio);
    let mut rebound = Vec::with_capacity(4);
    for address in addresses {
        match UdpSocket::bind(address).await {
            Ok(socket) => rebound.push(socket),
            Err(error) => result = combine(result, Err(failure(error))),
        }
    }
    let packets = result?;
    let mut branches = BTreeSet::new();
    for (index, packet) in packets.iter().enumerate() {
        assert_eq!(
            packet.method,
            if index % 2 == 0 {
                Method::Invite
            } else {
                Method::Ack
            }
        );
        assert_eq!(
            packet.call_id_header().map_err(failure)?.value(),
            "direction-update"
        );
        assert_eq!(
            packet
                .cseq_header()
                .map_err(failure)?
                .typed()
                .map_err(failure)?
                .seq,
            u32::try_from(index / 2 + 1).map_err(failure)?
        );
        let via = packet
            .top_via_header()
            .map_err(failure)?
            .typed()
            .map_err(failure)?;
        let branch = via
            .branch()
            .ok_or(ControlError::Invalid("Missing direction branch"))?
            .value();
        assert_ne!(branch, original_branch);
        assert!(
            branches.insert(branch.to_owned()),
            "Direction update reused a transaction branch"
        );
    }
    assert_eq!(branches.len(), 4);
    Ok(())
}
