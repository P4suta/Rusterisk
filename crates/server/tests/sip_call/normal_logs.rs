use super::{failure, Config, ControlError};
use base64::{engine::general_purpose::STANDARD, Engine};

pub(super) fn check(log: &[u8], config: &Config) -> Result<(), ControlError> {
    let contains =
        |value: &[u8]| !value.is_empty() && log.windows(value.len()).any(|part| part == value);
    let accounts = config
        .users
        .iter()
        .map(|user| (&user.name, &user.password))
        .chain(
            config
                .auth
                .iter()
                .map(|auth| (&auth.username, &auth.password)),
        );
    for (name, password) in accounts {
        let password = password.expose();
        if contains(password.as_bytes())
            || contains(STANDARD.encode(format!("{name}:{password}")).as_bytes())
        {
            return Err(ControlError::Invalid("A call log contains a credential"));
        }
    }
    for sample in [4000, -7000, -3000, 8000] {
        let payload = [rusterisk_media::mulaw_encode(sample); 160];
        if contains(&payload)
            || contains(STANDARD.encode(payload).as_bytes())
            || [",", ", "].iter().any(|separator| {
                contains(
                    format!("{sample}{separator}{sample}{separator}{sample}{separator}{sample}")
                        .as_bytes(),
                )
            })
        {
            return Err(ControlError::Invalid("A call log contains received audio"));
        }
    }
    Ok(())
}

pub(super) fn files(directory: &std::path::Path, config: &Config) -> Result<(), ControlError> {
    let mut observed = false;
    for entry in std::fs::read_dir(directory).map_err(failure)? {
        let path = entry.map_err(failure)?.path();
        if matches!(
            path.extension().and_then(|ext| ext.to_str()),
            Some("log" | "stderr")
        ) {
            let content = std::fs::read(path).map_err(failure)?;
            observed |= !content.is_empty();
            check(&content, config)?;
        }
    }
    if !observed {
        return Err(ControlError::Invalid("Expected a server startup log"));
    }
    Ok(())
}

#[test]
fn normal_logs_reject_credentials_and_received_audio() -> Result<(), ControlError> {
    let config = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[operator]\ntype=user\npassword=log-test-pass\n",
        "",
        "",
        "",
    )
    .map_err(failure)?;
    assert!(check(b"server ready", &config).is_ok());
    let payload = [rusterisk_media::mulaw_encode(-7000); 160];
    for log in [
        b"log-test-pass".as_slice(),
        STANDARD.encode("operator:log-test-pass").as_bytes(),
        &payload,
        STANDARD.encode(payload).as_bytes(),
        b"[-7000,-7000,-7000,-7000]",
    ] {
        assert!(check(log, &config).is_err());
    }
    Ok(())
}
