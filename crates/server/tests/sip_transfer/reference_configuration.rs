use super::{failure, transfer_lifetime::combine};
use reqwest::{Client, StatusCode, Url};
use rusterisk_api::{Channel, ChannelId};
use rusterisk_server::ControlError;
use std::{
    collections::BTreeSet,
    net::SocketAddr,
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::io::AsyncReadExt;

pub(super) struct Configuration {
    root: PathBuf,
    sip: Vec<u8>,
    extensions: Vec<u8>,
    client: Client,
    address: Url,
}

async fn original(path: &Path) -> Result<Vec<u8>, ControlError> {
    let metadata = tokio::fs::symlink_metadata(path).await.map_err(failure)?;
    if !metadata.is_file() || metadata.len() > 65_536 {
        return Err(ControlError::Invalid(
            "Invalid owned reference configuration file",
        ));
    }
    let mut bytes = Vec::with_capacity(65_537);
    tokio::fs::File::open(path)
        .await
        .map_err(failure)?
        .take(65_537)
        .read_to_end(&mut bytes)
        .await
        .map_err(failure)?;
    if bytes.len() > 65_536 {
        return Err(ControlError::Capacity);
    }
    Ok(bytes)
}

impl Configuration {
    pub(super) async fn load() -> Result<Self, ControlError> {
        let root = std::env::var_os("RUSTERISK_REFERENCE_CONFIGURATION")
            .map(PathBuf::from)
            .ok_or(ControlError::Invalid(
                "Reference configuration directory is required",
            ))?;
        let expected = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/interop")
            .canonicalize()
            .map_err(failure)?;
        let metadata = tokio::fs::symlink_metadata(&root).await.map_err(failure)?;
        let root = root.canonicalize().map_err(failure)?;
        if !metadata.is_dir()
            || root.parent() != Some(expected.as_path())
            || !root
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".configuration"))
        {
            return Err(ControlError::Invalid(
                "Reference configuration is not an owned fixture",
            ));
        }
        let address = Url::parse(&std::env::var("RUSTERISK_REFERENCE_URL").map_err(failure)?)
            .map_err(failure)?;
        if address.scheme() != "http"
            || address.host_str() != Some("127.0.0.1")
            || address.port().is_none_or(|port| port == 0)
            || !address.username().is_empty()
            || address.password().is_some()
            || address.query().is_some()
            || address.fragment().is_some()
        {
            return Err(ControlError::Invalid(
                "Reference URL must name the isolated local listener",
            ));
        }
        Ok(Self {
            sip: original(&root.join("pjsip.conf")).await?,
            extensions: original(&root.join("extensions.conf")).await?,
            root,
            client: Client::builder()
                .timeout(Duration::from_secs(2))
                .build()
                .map_err(failure)?,
            address,
        })
    }

    async fn reload(&self) -> Result<(), ControlError> {
        let mut result = Ok(());
        for module in ["res_pjsip.so", "pbx_config.so"] {
            let response = self
                .client
                .put(
                    self.address
                        .join(&format!("/ari/asterisk/modules/{module}"))
                        .map_err(failure)?,
                )
                .basic_auth("app", Some("test-password"))
                .send()
                .await
                .map_err(failure);
            let observed = response.and_then(|response| {
                if response.status() == StatusCode::NO_CONTENT {
                    Ok(())
                } else {
                    Err(failure(format!(
                        "Reference module reload returned {}",
                        response.status()
                    )))
                }
            });
            result = combine(result, observed);
        }
        result
    }

    pub(super) async fn configure(
        &self,
        bob: SocketAddr,
        charlie: SocketAddr,
    ) -> Result<(), ControlError> {
        let sip = format!("[global]\ntype=global\nendpoint_identifier_order=auth_username,username,ip\n[udp]\ntype=transport\nprotocol=udp\nbind=0.0.0.0:5060\n[auth]\ntype=auth\nauth_type=userpass\nusername=phone\npassword=test-password\nsupported_algorithms_uas=MD5\n[dynamic]\ntype=aor\nmax_contacts=1\n[phone]\ntype=endpoint\ntransport=udp\nauth=auth\naors=dynamic\ncontext=internal\nidentify_by=auth_username\ndisallow=all\nallow=ulaw\ndirect_media=no\nallow_transfer=yes\nrefer_blind_progress=yes\n[spoofed]\ntype=endpoint\ntransport=udp\nauth=auth\naors=dynamic\ncontext=outside\nidentify_by=auth_username\ndisallow=all\nallow=ulaw\ndirect_media=no\n[bob-aor]\ntype=aor\ncontact=sip:bob@{bob}\n[transfer-bob]\ntype=endpoint\ntransport=udp\naors=bob-aor\ndisallow=all\nallow=ulaw\ndirect_media=no\n[charlie-aor]\ntype=aor\ncontact=sip:charlie@{charlie}\n[transfer-charlie]\ntype=endpoint\ntransport=udp\naors=charlie-aor\ndisallow=all\nallow=ulaw\ndirect_media=no\n");
        tokio::fs::write(self.root.join("pjsip.conf"), sip)
            .await
            .map_err(failure)?;
        tokio::fs::write(self.root.join("extensions.conf"), "[internal]\nexten => 100,1,Dial(PJSIP/transfer-bob,5)\nsame => n,Hangup()\nexten => 200,1,Dial(PJSIP/transfer-charlie,5)\nsame => n,Hangup()\n[outside]\nexten => 100,1,Hangup()\n").await.map_err(failure)?;
        self.reload().await
    }

    pub(super) async fn restore(&self) -> Result<(), ControlError> {
        let sip = tokio::fs::write(self.root.join("pjsip.conf"), &self.sip)
            .await
            .map_err(failure);
        let extensions = tokio::fs::write(self.root.join("extensions.conf"), &self.extensions)
            .await
            .map_err(failure);
        combine(combine(sip, extensions), self.reload().await)
    }

    pub(super) async fn channels(&self) -> Result<Vec<Channel>, ControlError> {
        let response = self
            .client
            .get(self.address.join("/ari/channels").map_err(failure)?)
            .basic_auth("app", Some("test-password"))
            .send()
            .await
            .map_err(failure)?
            .error_for_status()
            .map_err(failure)?;
        let channels: Vec<Channel> = response.json().await.map_err(failure)?;
        if channels.len() > 16 {
            return Err(ControlError::Capacity);
        }
        Ok(channels)
    }

    pub(super) async fn end_channels(
        &self,
        previous: &BTreeSet<ChannelId>,
    ) -> Result<(), ControlError> {
        let channels = self.channels().await?;
        let mut result = Ok(());
        for channel in channels
            .into_iter()
            .filter(|channel| !previous.contains(&channel.id))
        {
            let mut target = self.address.clone();
            target
                .path_segments_mut()
                .map_err(|()| ControlError::Invalid("Reference channel URL is unavailable"))?
                .clear()
                .extend(["ari", "channels", channel.id.as_str()]);
            let deleted = self
                .client
                .delete(target)
                .basic_auth("app", Some("test-password"))
                .send()
                .await
                .map_err(failure)
                .and_then(|response| {
                    if matches!(
                        response.status(),
                        StatusCode::NO_CONTENT | StatusCode::NOT_FOUND
                    ) {
                        Ok(())
                    } else {
                        Err(failure(format!(
                            "Reference hangup returned {}",
                            response.status()
                        )))
                    }
                });
            result = combine(result, deleted);
        }
        let completed = tokio::time::timeout(Duration::from_secs(2), async {
            let mut tick = tokio::time::interval(Duration::from_millis(20));
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            for _ in 0..128 {
                tick.tick().await;
                if self
                    .channels()
                    .await?
                    .iter()
                    .all(|channel| previous.contains(&channel.id))
                {
                    return Ok(());
                }
            }
            Err(ControlError::Capacity)
        })
        .await
        .map_err(failure)
        .and_then(|result| result);
        combine(result, completed)
    }
}
