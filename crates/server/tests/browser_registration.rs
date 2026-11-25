use rusterisk_config::Config;
use rusterisk_server::{runtime::Server, ControlError};
use rusterisk_test_support::Wait;
use std::path::Path;

fn failure(error: impl std::fmt::Display) -> ControlError {
    ControlError::Adapter(error.to_string())
}

#[tokio::test]
async fn browser_signaling_adapter_registers_reconnects_and_refuses_incorrect_credentials(
) -> Result<(), ControlError> {
    let mut config = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test-password\n",
        "[browser]\ntype=transport\nprotocol=ws\nbind=127.0.0.1\n[auth]\ntype=auth\nusername=phone\npassword=test-password\n[phone]\ntype=aor\nmax_contacts=1\n[phone-endpoint]\ntype=endpoint\ntransport=browser\nauth=auth\naors=phone\nallow=ulaw\nwebrtc=yes\ndtmf_mode=info\ncontext=internal\n",
        "",
        "[internal]\nexten => 100,1,Stasis(calls)\n",
    ).map_err(failure)?;
    config.http.bind.set_port(0);
    config.rtp.start = 0;
    config.rtp.end = 0;
    let data = tempfile::tempdir().map_err(failure)?;
    let server = Server::bind(config, 4, data.path().to_owned()).await?;
    let address = server.local_addr()?;
    let controller = server.controller();
    let mut serving = tokio::spawn(server.run());
    let result = async {
        let script = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../examples/browser/tests/registration.ts");
        let mut child = tokio::process::Command::new("node")
            .arg("--max-old-space-size=128")
            .arg(script)
            .env("RUSTERISK_BROWSER_SIGNALING", format!("ws://{address}/ws"))
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .map_err(failure)?;
        let status = match Wait::Shutdown.bounded(child.wait()).await {
            Ok(result) => result.map_err(failure)?,
            Err(primary) => {
                child.kill().await.map_err(failure)?;
                return Err(failure(primary));
            }
        };
        if !status.success() {
            return Err(failure(format!(
                "browser signaling fixture failed: {status}"
            )));
        }
        Ok(())
    }
    .await;
    controller.shutdown();
    let cleanup = match Wait::Shutdown.bounded(&mut serving).await {
        Ok(joined) => joined.map_err(failure)?,
        Err(primary) => {
            serving.abort();
            match serving.await {
                Err(error) if error.is_cancelled() => Err(failure(primary)),
                Err(error) => Err(failure(format!("{primary}; server task failed: {error}"))),
                Ok(Err(error)) => Err(failure(format!(
                    "{primary}; server cleanup failed: {error}"
                ))),
                Ok(Ok(())) => Err(failure(primary)),
            }
        }
    };
    match (result, cleanup) {
        (Ok(()), Ok(())) => Ok(()),
        (Err(error), Ok(())) | (Ok(()), Err(error)) => Err(error),
        (Err(primary), Err(cleanup)) => Err(failure(format!(
            "{primary}; browser server cleanup failed: {cleanup}"
        ))),
    }
}
