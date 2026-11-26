use super::super::{transfer_peer::Peer, transfer_reference_configuration::Configuration};
use super::{
    combine,
    exercise::{self, Departure, ObservedTransfer},
    failure, finish,
    signals::RequestRoute,
};
use rusterisk_server::ControlError;
use std::{
    collections::BTreeSet,
    net::{IpAddr, Ipv4Addr, SocketAddr},
};
use tokio::{net::UdpSocket, sync::mpsc};

pub(super) async fn compare() -> Result<ObservedTransfer, ControlError> {
    let configuration = Configuration::load().await?;
    let previous: BTreeSet<_> = configuration
        .channels()
        .await?
        .into_iter()
        .map(|channel| channel.id)
        .collect();
    if !previous.is_empty() {
        return Err(ControlError::Invalid(
            "Reference channels were not quiescent before transfer",
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
            "Invalid isolated reference peer address",
        ));
    }
    let target = SocketAddr::new(server.into(), 5060);
    let alice = Peer::bind(Ipv4Addr::UNSPECIFIED.into(), callback.into()).await?;
    let bob = Peer::bind(Ipv4Addr::UNSPECIFIED.into(), callback.into()).await?;
    let charlie = Peer::bind(Ipv4Addr::UNSPECIFIED.into(), callback.into()).await?;
    let bob_contact = bob.contact()?;
    let charlie_contact = charlie.contact()?;
    let mut addresses = Vec::with_capacity(9);
    for peer in [&alice, &bob, &charlie] {
        addresses.extend(peer.addresses()?);
    }
    let (media_ports, mut server_ports) = mpsc::channel(3);
    let mut result = async {
        configuration
            .configure(bob_contact, charlie_contact)
            .await?;
        finish(tokio::spawn(exercise::run(
            [alice, bob, charlie],
            target,
            media_ports,
            Departure::Transferor,
            RequestRoute::CallbackPending { server: target },
        )))
        .await
    }
    .await;
    let expected = result.is_ok();
    let mut observed = 0;
    for _ in 0..3 {
        match server_ports.try_recv() {
            Ok(address) => {
                observed += 1;
                if address.ip() != IpAddr::V4(server) {
                    result = combine(
                        result,
                        Err(ControlError::Invalid(
                            "Reference media escaped its isolated server",
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
    if expected && observed != 3 {
        result = combine(
            result,
            Err(ControlError::Invalid(
                "Missing received reference media addresses",
            )),
        );
    }
    result = combine(result, configuration.end_channels(&previous).await);
    result = combine(result, configuration.restore().await);
    let mut rebound = Vec::with_capacity(9);
    for address in addresses {
        match UdpSocket::bind(address).await {
            Ok(socket) => rebound.push(socket),
            Err(error) => {
                result = combine(
                    result,
                    Err(failure(format!("reference rebind {address}: {error}"))),
                )
            }
        }
    }
    result
}
