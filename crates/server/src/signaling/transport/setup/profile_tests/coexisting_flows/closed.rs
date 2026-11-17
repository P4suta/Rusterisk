use super::*;
use tokio::io::AsyncWriteExt;

#[tokio::test]
async fn closing_an_old_tls_flow_preserves_the_replacement_flow() -> Result<(), ControlError> {
    let mut fixture = Fixture::new().await?;
    let primary = async {
        let mut old = fixture
            .connect(NonZeroU64::MIN, "old", fixture.old_server.clone())
            .await?;
        let old_route = fixture.transport.network.capture_connection(old.key)?;
        fixture.transport.replace_profile()?;
        let mut new = fixture
            .connect(
                NonZeroU64::new(2).ok_or(ControlError::Capacity)?,
                "new",
                fixture.new_server.clone(),
            )
            .await?;
        let new_route = fixture.transport.network.capture_connection(new.key)?;
        old.socket.shutdown().await.map_err(failure)?;
        let old_key = old.key;
        drop(old);
        let closed = tokio::time::timeout(Duration::from_secs(2), async {
            for _ in 0..16 {
                match fixture.transport.network.next().await? {
                    Input::Closed { key, result } => {
                        result?;
                        return Ok(key);
                    }
                    Input::SetupFinished { result, .. } => result?,
                    Input::Packet(_) | Input::Prepared(_) => {
                        return Err(ControlError::Invalid(
                            "Unexpected event while closing the old TLS flow",
                        ))
                    }
                }
            }
            Err(ControlError::Capacity)
        })
        .await
        .map_err(failure)??;
        require(
            closed == old_key
                && !fixture.transport.network.current(old_route)
                && fixture.transport.network.current(new_route),
            "Closing the old TLS flow removed another generation",
        )?;
        require(
            fixture
                .transport
                .network
                .capture_connection(old_key)
                .is_err(),
            "Closed connection key was rebound to the surviving peer flow",
        )?;
        let bytes = wire(fixture.transport.network.local_addr(0)?, "surviving-new");
        require(
            !fixture
                .transport
                .network
                .send_on(
                    old_route,
                    Datagram::new(old_key.peer, bytes.as_bytes().into()),
                )
                .await?,
            "Closed old route fell back to the replacement TLS flow",
        )?;
        require(
            fixture
                .transport
                .network
                .send_on(
                    new_route,
                    Datagram::new(new.key.peer, bytes.as_bytes().into()),
                )
                .await?,
            "Replacement TLS flow was stopped with the old flow",
        )?;
        require(
            new.message().await? == bytes.as_bytes(),
            "Replacement TLS flow did not retain its socket",
        )?;
        Ok(())
    }
    .await;
    fixture.finish(primary).await
}
