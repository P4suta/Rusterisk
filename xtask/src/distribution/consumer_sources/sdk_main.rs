mod core_contract;

fn main() -> Result<(), std::io::Error> {
    core_contract::verify()?;
    let identifier = rusterisk_api::ChannelId::try_from("candidate-consumer".to_owned())
        .map_err(std::io::Error::other)?;
    let reexport: &rusterisk_client::api::ChannelId = &identifier;
    if reexport.as_str() != "candidate-consumer" {
        return Err(std::io::Error::other(
            "SDK resource reexport contract failed",
        ));
    }
    let client = rusterisk_client::Client::new("http://127.0.0.1:9", "api", "candidate")
        .map_err(std::io::Error::other)?;
    if rusterisk_client::Client::new("http://example.invalid", "api", "candidate").is_ok() {
        return Err(std::io::Error::other("SDK origin contract failed"));
    }
    drop(client);
    println!("SDK candidate consumer passed");
    Ok(())
}
