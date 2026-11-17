use super::*;
use crate::signaling::Received;
use tokio::io::AsyncWriteExt;

impl Fixture {
    async fn packet(&mut self) -> Result<Received, ControlError> {
        tokio::time::timeout(Duration::from_secs(2), async {
            for _ in 0..16 {
                match self.transport.network.next().await? {
                    Input::Packet(packet) => return Ok(packet),
                    Input::Closed { result, .. } => {
                        result?;
                        return Err(ControlError::Invalid(
                            "Verified flow closed before delivering its SIP packet",
                        ));
                    }
                    Input::SetupFinished { result, .. } => result?,
                    Input::Prepared(_) | Input::Upgrade(_) => {
                        return Err(ControlError::Invalid("Unexpected extra TLS preparation"))
                    }
                }
            }
            Err(ControlError::Capacity)
        })
        .await
        .map_err(failure)?
    }
}

#[tokio::test]
async fn received_sip_packets_retain_the_exact_verified_tls_flow() -> Result<(), ControlError> {
    let mut fixture = Fixture::new().await?;
    let primary = async {
        let mut old = fixture
            .connect(NonZeroU64::MIN, "old", fixture.old_server.clone())
            .await?;
        fixture.transport.replace_profile()?;
        let mut new = fixture
            .connect(
                NonZeroU64::new(2).ok_or(ControlError::Capacity)?,
                "new",
                fixture.new_server.clone(),
            )
            .await?;
        let local = fixture.transport.network.local_addr(0)?;
        let old_wire = wire(local, "received-old");
        old.socket
            .write_all(old_wire.as_bytes())
            .await
            .map_err(failure)?;
        let received = fixture.packet().await?;
        require(
            received.bytes == old_wire.as_bytes()
                && received.route().matches_connection(old.key)
                && !received.route().matches_connection(new.key),
            "Received old TLS packet lost its original connection key",
        )?;
        let new_wire = wire(local, "received-new");
        new.socket
            .write_all(new_wire.as_bytes())
            .await
            .map_err(failure)?;
        let received = fixture.packet().await?;
        require(
            received.bytes == new_wire.as_bytes()
                && received.route().matches_connection(new.key)
                && !received.route().matches_connection(old.key),
            "Received new TLS packet inherited another connection key",
        )?;
        Ok(())
    }
    .await;
    fixture.finish(primary).await
}
