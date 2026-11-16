use super::{failure, network, ControlError, Input, Reserved, OPTIONS};
use tokio::{io::AsyncWriteExt, net::TcpStream};

#[tokio::test]
async fn cancelled_resolutions_keep_shared_capacity_until_reaped() -> Result<(), ControlError> {
    let mut network = network().await?;
    let mut pending = Vec::with_capacity(16);
    for _ in 0..16 {
        pending.push(network.reserve_setup(0)?);
    }
    assert!(matches!(
        network.reserve_setup(0),
        Err(ControlError::Capacity)
    ));
    for key in &pending {
        network.cancel_setup(*key);
    }
    assert!(matches!(
        network.reserve_setup(0),
        Err(ControlError::Capacity)
    ));
    let previous = pending.pop().ok_or(ControlError::Capacity)?;
    network.finish_resolution(previous);
    let replacement = network.reserve_setup(0)?;
    assert_ne!(previous, replacement);
    assert!(matches!(
        network.reserve_setup(0),
        Err(ControlError::Capacity)
    ));
    network.close().await?;
    assert!(network.preparations.is_empty());
    assert!(network.ports.is_empty());
    assert!(network.jobs.is_empty());
    Ok(())
}

#[tokio::test]
async fn registered_setup_slots_share_capacity_without_reserving_more_connections(
) -> Result<(), ControlError> {
    let mut network = network().await?;
    let mut receivers = Vec::with_capacity(64);
    for port in 20_000..20_064 {
        receivers.push(network.reserve(0, ([127, 0, 0, 1], port).into())?);
    }
    assert!(matches!(
        network.reserve_setup(0),
        Err(ControlError::Capacity)
    ));
    let mut slots = Vec::with_capacity(16);
    for _ in 0..16 {
        slots.push(network.reserve_registered_setup(0)?);
    }
    assert!(matches!(
        network.reserve_registered_setup(0),
        Err(ControlError::Capacity)
    ));
    for slot in &slots {
        network.cancel_setup(*slot);
    }
    assert!(matches!(
        network.reserve_registered_setup(0),
        Err(ControlError::Capacity)
    ));
    let previous = slots.pop().ok_or(ControlError::Capacity)?;
    network.finish_resolution(previous);
    let replacement = network.reserve_registered_setup(0)?;
    assert_ne!(previous, replacement);
    assert!(matches!(
        network.reserve_registered_setup(0),
        Err(ControlError::Capacity)
    ));
    network.close().await?;
    drop(receivers);
    Ok(())
}

#[tokio::test]
async fn registered_routes_reject_closing_and_replaced_connection_generations(
) -> Result<(), ControlError> {
    let mut network = network().await?;
    let mut peer = TcpStream::connect(network.local_addr(0)?)
        .await
        .map_err(failure)?;
    let source = peer.local_addr().map_err(failure)?;
    peer.write_all(OPTIONS).await.map_err(failure)?;
    assert!(matches!(network.next().await?, Input::Packet(_)));
    let previous = network.registered_route(0, source)?;
    assert!(network.route_is_current(previous));
    network.refuse(0, source);
    assert!(!network.route_is_current(previous));
    assert!(matches!(
        network.registered_route(0, source),
        Err(ControlError::Conflict(_))
    ));
    network.ports.remove(&(0, source));
    let Reserved { packets, stop, .. } = network.reserve(0, source)?;
    let current = network.registered_route(0, source)?;
    assert!(!network.route_is_current(previous));
    assert!(network.route_is_current(current));
    network.close().await?;
    drop((peer, packets, stop));
    Ok(())
}
