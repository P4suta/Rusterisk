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
