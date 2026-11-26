use crate::{
    media::MediaHub,
    reload::PublicationKey,
    signaling::{
        cleanup, registrations::PreparedRegistrations, transport::DeliveryRoute, Received,
        Signaling,
    },
    ControlError, Controller,
};
use rsipstack::sip::{headers::untyped::ToTypedHeader, HeadersExt, Request, SipMessage};
use rusterisk_config::Config;
use rusterisk_core::SettingsLifetimes;
use rusterisk_test_support::Wait;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use tokio::{net::TcpListener, sync::mpsc::error::TryRecvError};

const PEER: SocketAddr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 5061);

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

fn require(condition: bool, message: &'static str) -> Result<(), ControlError> {
    if condition {
        Ok(())
    } else {
        Err(ControlError::Invalid(message))
    }
}

fn configuration() -> Result<Config, ControlError> {
    Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test\n",
        "[stream]\ntype=transport\nprotocol=tcp\nbind=127.0.0.1:0\n[account]\ntype=auth\nusername=account\npassword=sample\n[provider]\ntype=registration\ntransport=stream\noutbound_auth=account\nserver_uri=sip:127.0.0.1:5061\\;transport=tcp\nclient_uri=sip:account@provider.example\nmax_random_initial_delay=0\n",
        "", "",
    ).map_err(failure)
}

fn request(bytes: &[u8]) -> Result<Request, ControlError> {
    match SipMessage::try_from(bytes).map_err(failure)? {
        SipMessage::Request(request) => Ok(request),
        SipMessage::Response(_) => Err(ControlError::Invalid("Expected a REGISTER request")),
    }
}

fn response(request: &Request, status: u16) -> Result<Vec<u8>, ControlError> {
    Ok(format!("SIP/2.0 {status} Response\r\nVia: {}\r\nFrom: {}\r\nTo: {};tag=registrar\r\nCall-ID: {}\r\nCSeq: {}\r\nContent-Length: 0\r\n\r\n", request.top_via_header().map_err(failure)?.value(), request.from_header().map_err(failure)?.value(), request.to_header().map_err(failure)?.value(), request.call_id_header().map_err(failure)?.value(), request.cseq_header().map_err(failure)?.value()).into_bytes())
}

fn received(signaling: &mut Signaling, route: DeliveryRoute, bytes: Vec<u8>, now: u64) -> bool {
    signaling.registration_received(
        &Received {
            listener: route.listener(),
            peer: route.peer(),
            route,
            bytes,
        },
        now,
    )
}

async fn resolved(signaling: &mut Signaling, now: u64) -> Result<(), ControlError> {
    let completed = Wait::Ready
        .bounded(signaling.registrations.completed())
        .await
        .map_err(failure)??
        .ok_or(ControlError::Invalid(
            "Missing literal resolution completion",
        ))?;
    signaling.registration_completed(completed, now)
}

async fn finish(
    mut signaling: Signaling,
    primary: Result<(), ControlError>,
) -> Result<(), ControlError> {
    let local = signaling.network.local_addr(0);
    let mut result = cleanup::combine(primary, signaling.finish_registrations().await);
    result = cleanup::combine(result, signaling.finish_native().await);
    result = cleanup::combine(result, signaling.network.close().await);
    drop(signaling);
    let rebound = match local {
        Ok(local) => TcpListener::bind(local)
            .await
            .map(|_listener| ())
            .map_err(failure),
        Err(error) => Err(error),
    };
    cleanup::combine(result, rebound)
}

// The fixture checks private route matching without exercising network ingress.
#[tokio::test]
async fn retirement_keeps_the_trying_route_when_peer_lookup_points_at_another_generation(
) -> Result<(), ControlError> {
    let config = configuration()?;
    let (controller, runner, _effects) = Controller::new(config.clone(), 16);
    let (media, _hub, notices) = MediaHub::new(16);
    let mut signaling = Signaling::bind(config.clone(), controller, runner, media, notices).await?;
    let primary = async {
        let mut original = signaling.network.reserve(0, PEER)?;
        let captured = signaling.network.capture(0, PEER)?;
        require(
            captured.matches_connection(original.key),
            "Starting route lost its connection key",
        )?;
        signaling.registration_tick(1000).await?;
        resolved(&mut signaling, 1001).await?;
        signaling.registration_tick(1001).await?;
        let initial = request(&original.packets.try_recv().map_err(failure)?)?;
        require(
            signaling.network.ports.remove(&original.key).is_some(),
            "Missing starting stream port",
        )?;
        let mut replacement = signaling.network.reserve(0, PEER)?;
        require(
            original.key != replacement.key && !captured.matches_connection(replacement.key),
            "Replacement reused the starting connection key",
        )?;
        require(
            !signaling.network.current(captured),
            "The old route still identifies the replacement port",
        )?;
        let prepared = PreparedRegistrations::prepare(&config)?;
        let mut lifetimes = SettingsLifetimes::new(94);
        let base = lifetimes.current();
        let candidate = lifetimes.prepare().map_err(|_| ControlError::Capacity)?;
        let key = PublicationKey::new(candidate, base);
        signaling
            .registrations
            .quiesce(key, &prepared, &signaling.network, 1500, 1500)?;
        let generation = lifetimes
            .commit(candidate)
            .map_err(|_| ControlError::Capacity)?
            .generation();
        signaling
            .registrations
            .publish(key, prepared, generation, 1500)?;
        let final_response = response(&initial, 401)?;
        let replacement_route = signaling.network.capture(0, PEER)?;
        require(
            replacement_route.matches_connection(replacement.key),
            "The replacement route lost its connection key",
        )?;
        require(
            !received(
                &mut signaling,
                replacement_route,
                final_response.clone(),
                1600,
            ),
            "A replacement connection settled the old REGISTER",
        )?;
        signaling.registration_tick(2500).await?;
        require(
            signaling.registrations.setup_count() == 0
                && matches!(replacement.packets.try_recv(), Err(TryRecvError::Empty)),
            "The wrong connection released registrar serialization",
        )?;
        require(
            received(&mut signaling, captured, response(&initial, 100)?, 2501),
            "The original connection lost its provisional response",
        )?;
        signaling.registration_tick(2501).await?;
        require(
            signaling.registrations.setup_count() == 0,
            "A provisional response ended the retiring barrier",
        )?;
        require(
            received(&mut signaling, captured, final_response.clone(), 2502),
            "Retirement captured the replacement peer lookup instead of its starting route",
        )?;
        signaling.registration_tick(2503).await?;
        resolved(&mut signaling, 2504).await?;
        signaling.registration_tick(2504).await?;
        let next = request(&replacement.packets.try_recv().map_err(failure)?)?;
        require(
            next.call_id_header().map_err(failure)?.value()
                == initial.call_id_header().map_err(failure)?.value(),
            "Replacement changed its boot Call-ID",
        )?;
        require(
            next.cseq_header()
                .map_err(failure)?
                .typed()
                .map_err(failure)?
                .seq
                > initial
                    .cseq_header()
                    .map_err(failure)?
                    .typed()
                    .map_err(failure)?
                    .seq,
            "Replacement reused its retired request sequence",
        )?;
        require(
            next.authorization_header().is_none(),
            "A retired challenge restarted authentication",
        )?;
        require(
            !received(&mut signaling, captured, final_response, 2505),
            "A settled stream retirement revived",
        )
    }
    .await;
    finish(signaling, primary).await
}
