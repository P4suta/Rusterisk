use super::{failure, Config, ControlError, Input, Network};
use crate::signaling::sip_websocket::Scheme;
use rusterisk_test_support::Wait;

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
        let input = Wait::Ready
            .bounded(network.next())
            .await
            .map_err(failure)??;
        assert!(matches!(input, Input::Closed { result: Err(_), .. }));
    }
    assert!(network.ports.is_empty());
    network.close().await
}

#[tokio::test(start_paused = true)]
async fn expired_upgrade_senders_cannot_attach_to_a_replacement_generation(
) -> Result<(), ControlError> {
    let mut network = network().await?;
    let peer = "127.0.0.1:5061".parse().map_err(failure)?;
    let start = tokio::time::Instant::now();
    let expired = network.upgrade(Scheme::Ws, peer)?;
    let Input::Closed {
        key: previous,
        result,
    } = rusterisk_test_support::virtual_window(std::time::Duration::from_secs(6), network.next())
        .await
        .map_err(failure)??
    else {
        return Err(ControlError::Capacity);
    };
    assert_eq!(start.elapsed(), std::time::Duration::from_secs(5));
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
    let input = Wait::Ready
        .bounded(network.next())
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
    Wait::Ready
        .bounded(network.close())
        .await
        .map_err(failure)??;
    assert!(reservation.is_closed());
    assert!(network.jobs.is_empty());
    Ok(())
}
