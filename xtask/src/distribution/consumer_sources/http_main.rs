mod core_contract;
mod http_contract;

use std::{net::SocketAddr, time::Duration};

fn setting(name: &str, limit: usize) -> Result<String, std::io::Error> {
    let value = std::env::var(name)
        .map_err(|_| std::io::Error::other("SDK HTTP fixture setting is missing"))?;
    if value.is_empty() || value.len() > limit || value.chars().any(char::is_control) {
        return Err(std::io::Error::other("SDK HTTP fixture setting is invalid"));
    }
    Ok(value)
}

fn main() -> Result<(), std::io::Error> {
    core_contract::verify()?;
    let address = setting("RUSTERISK_SDK_HTTP_ADDRESS", 128)?;
    let socket = address
        .strip_prefix("http://")
        .and_then(|address| address.parse::<SocketAddr>().ok())
        .filter(|address| address.ip().is_loopback() && address.port() != 0)
        .ok_or_else(|| std::io::Error::other("SDK HTTP fixture requires a loopback origin"))?;
    let username = setting("RUSTERISK_SDK_HTTP_USERNAME", 128)?;
    let password = setting("RUSTERISK_SDK_HTTP_PASSWORD", 1024)?;
    let version = setting("RUSTERISK_SDK_HTTP_VERSION", 128)?;
    let identifier: rusterisk_client::BridgeId =
        rusterisk_client::BridgeId::try_from(setting("RUSTERISK_SDK_HTTP_BRIDGE", 128)?)
            .map_err(|_| std::io::Error::other("SDK HTTP fixture bridge identifier is invalid"))?;
    let client = rusterisk_client::Client::new(&format!("http://{socket}"), &username, &password)
        .map_err(|_| std::io::Error::other("SDK HTTP fixture client setup failed"))?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .max_blocking_threads(2)
        .enable_io()
        .enable_time()
        .build()?;
    let result = runtime.block_on(http_contract::verify(&client, &version, &identifier));
    drop(client);
    runtime.shutdown_timeout(Duration::from_secs(1));
    result.map_err(std::io::Error::other)?;
    println!("SDK HTTP candidate consumer passed");
    Ok(())
}
