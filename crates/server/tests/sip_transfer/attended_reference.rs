use super::super::transfer_reference_configuration::Configuration;
use super::{combine, exercise, failure, finish, Exercise, Peer, RequestRoute};
use rusterisk_server::ControlError;
use std::{
    collections::BTreeSet,
    net::{IpAddr, Ipv4Addr, SocketAddr},
};
use tokio::{net::UdpSocket, sync::mpsc};

pub(super) async fn compare() -> Result<(), ControlError> {
    let configuration = Configuration::load().await?;
    let previous: BTreeSet<_> = configuration
        .channels()
        .await?
        .into_iter()
        .map(|channel| channel.id)
        .collect();
    if !previous.is_empty() {
        return Err(ControlError::Invalid(
            "Reference channels were not quiescent before attended transfer",
        ));
    }
    let callback: Ipv4Addr = std::env::var("RUSTERISK_REFERENCE_CALLBACK_IP")
        .map_err(failure)?
        .parse()
        .map_err(failure)?;
    let server: Ipv4Addr = std::env::var("RUSTERISK_REFERENCE_MEDIA_IP")
        .map_err(failure)?
        .parse()
        .map_err(failure)?;
    if callback.is_unspecified()
        || callback.is_multicast()
        || callback.is_broadcast()
        || !server.is_private()
    {
        return Err(ControlError::Invalid(
            "Invalid isolated attended reference peer address",
        ));
    }
    let target = SocketAddr::new(server.into(), 5060);
    let first = Peer::bind(Ipv4Addr::UNSPECIFIED.into(), callback.into()).await?;
    let bob = Peer::bind(Ipv4Addr::UNSPECIFIED.into(), callback.into()).await?;
    let second = Peer::bind(Ipv4Addr::UNSPECIFIED.into(), callback.into()).await?;
    let charlie = Peer::bind(Ipv4Addr::UNSPECIFIED.into(), callback.into()).await?;
    let bob_contact = bob.contact()?;
    let charlie_contact = charlie.contact()?;
    let mut addresses = Vec::with_capacity(12);
    for peer in [&first, &bob, &second, &charlie] {
        addresses.extend(peer.addresses()?);
    }
    let (media_ports, mut server_ports) = mpsc::channel(4);
    let mut result = async {
        configuration
            .configure(bob_contact, charlie_contact)
            .await?;
        finish(tokio::spawn(exercise(
            [first, bob, second, charlie],
            [target, target],
            media_ports,
            Exercise::RetireSource,
            [
                RequestRoute::CallbackPending { server: target },
                RequestRoute::CallbackPending { server: target },
            ],
        )))
        .await
    }
    .await;
    let expected = result.is_ok();
    let mut observed = 0;
    for _ in 0..4 {
        match server_ports.try_recv() {
            Ok(address) => {
                observed += 1;
                if address.ip() != IpAddr::V4(server) {
                    result = combine(
                        result,
                        Err(ControlError::Invalid(
                            "Attended reference media escaped its isolated server",
                        )),
                    );
                }
            }
            Err(mpsc::error::TryRecvError::Disconnected) => break,
            Err(error) => {
                result = combine(result, Err(failure(error)));
                break;
            }
        }
    }
    if expected && observed != 4 {
        result = combine(
            result,
            Err(ControlError::Invalid(
                "Missing attended reference media addresses",
            )),
        );
    }
    if result.is_ok() {
        result = configuration.wait_for_channels(&previous).await;
    }
    if result.is_err() {
        result = combine(result, configuration.end_channels(&previous).await);
    }
    result = combine(result, configuration.restore().await);
    let mut rebound = Vec::with_capacity(12);
    for address in addresses {
        match UdpSocket::bind(address).await {
            Ok(socket) => rebound.push(socket),
            Err(error) => {
                result = combine(
                    result,
                    Err(failure(format!(
                        "attended reference rebind {address}: {error}"
                    ))),
                )
            }
        }
    }
    result
}
