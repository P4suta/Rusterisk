use super::{failure, Config, ControlError, Input, Network};
use crate::signaling::sip_websocket::Scheme;
use std::time::Duration;

async fn network() -> Result<Network, ControlError> {
    let mut config = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test\n",
        "[browser]\ntype=transport\nprotocol=ws\nbind=0.0.0.0\n",
        "",
        "",
    )
    .map_err(failure)?;
    Network::bind(&mut config).await
}

#[tokio::test]
async fn pending_upgrades_share_connection_capacity_and_cancel_without_a_socket(
) -> Result<(), ControlError> {
    let mut network = network().await?;
    let mut reservations = Vec::with_capacity(64);
    for port in 5061..5125 {
        let peer = format!("127.0.0.1:{port}").parse().map_err(failure)?;
        reservations.push(network.upgrade(Scheme::Ws, peer)?);
    }
    assert!(matches!(
        network.upgrade(Scheme::Ws, "127.0.0.1:5125".parse().map_err(failure)?),
        Err(ControlError::Capacity)
    ));
    drop(reservations);
    for _ in 0..64 {
        let input = tokio::time::timeout(Duration::from_secs(1), network.next())
            .await
            .map_err(failure)??;
        assert!(matches!(input, Input::Closed { result: Err(_), .. }));
    }
    assert!(network.ports.is_empty());
    network.close().await
}

#[tokio::test]
async fn expired_upgrade_senders_cannot_attach_to_a_replacement_generation(
) -> Result<(), ControlError> {
    let mut network = network().await?;
    let peer = "127.0.0.1:5061".parse().map_err(failure)?;
    let expired = network.upgrade(Scheme::Ws, peer)?;
    let Input::Closed {
        key: previous,
        result,
    } = tokio::time::timeout(Duration::from_secs(6), network.next())
        .await
        .map_err(failure)??
    else {
        return Err(ControlError::Capacity);
    };
    assert!(result.is_err());
    assert!(expired.is_closed());
    let replacement = network.upgrade(Scheme::Ws, peer)?;
    assert!(
        network
            .legacy_connection(0, peer)?
            .ok_or(ControlError::Capacity)?
            .generation
            > previous.generation
    );
    drop(replacement);
    let input = tokio::time::timeout(Duration::from_secs(1), network.next())
        .await
        .map_err(failure)??;
    assert!(matches!(input, Input::Closed { result: Err(_), .. }));
    network.close().await
}

#[tokio::test]
async fn shutdown_cancels_reserved_upgrades_without_waiting_for_a_socket(
) -> Result<(), ControlError> {
    let mut network = network().await?;
    let reservation = network.upgrade(Scheme::Ws, "127.0.0.1:5061".parse().map_err(failure)?)?;
    tokio::time::timeout(Duration::from_secs(1), network.close())
        .await
        .map_err(failure)??;
    assert!(reservation.is_closed());
    assert!(network.jobs.is_empty());
    Ok(())
}
