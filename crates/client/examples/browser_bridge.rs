use rusterisk_client::{api::ChannelId, Client, Error};
#[path = "browser_bridge/mod.rs"]
mod conversation;

fn setting(name: &str, fallback: Option<&str>) -> Result<String, Error> {
    match std::env::var(name) {
        Ok(value) => Ok(value),
        Err(std::env::VarError::NotPresent) => fallback
            .map(str::to_owned)
            .ok_or(Error::Invalid("required example setting is missing")),
        Err(std::env::VarError::NotUnicode(_)) => {
            Err(Error::Invalid("example settings must use Unicode"))
        }
    }
}

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<(), Error> {
    let client = Client::new(
        &setting("RUSTERISK_URL", Some("http://127.0.0.1:8088"))?,
        &setting("RUSTERISK_USERNAME", Some("api"))?,
        &setting("RUSTERISK_PASSWORD", None)?,
    )?;
    let application = ChannelId::try_from(setting("RUSTERISK_APP", Some("calls"))?)?;
    let endpoint = setting("RUSTERISK_ENDPOINT", Some("PJSIP/bob"))?;
    conversation::run(client, application, endpoint, async {
        tokio::signal::ctrl_c().await.map_err(Error::from)
    })
    .await
}
