use crate::{OracleError, MAX_INPUT};
use rusterisk_config::{Config, Dialplan, Ini, Reload};

const HTTP: &str = "[general]\nbindaddr=127.0.0.1\n";
const ARI: &str = "[fuzzer]\ntype=user\npassword=fuzz-password\n";

fn parse(files: [&str; 7]) -> Result<Config, rusterisk_config::ConfigError> {
    Config::parse(files[0], files[1], files[2], files[3], files[4])?
        .with_websocket_configuration(files[5], files[6])
}

pub fn settings(bytes: &[u8]) -> Result<usize, OracleError> {
    if bytes.len() > MAX_INPUT {
        return Err(OracleError::InputCapacity);
    }
    let Ok(source) = std::str::from_utf8(bytes) else {
        return Ok(0);
    };
    let decorated = format!("{source}\n; fuzz\n");
    let mut accepted = 0;
    if let Ok(ini) = Ini::parse("fuzz.conf", source) {
        if Ini::parse("fuzz.conf", &decorated).as_ref() != Ok(&ini) {
            return Err(OracleError::SettingsChanged);
        }
        accepted += 1;
    }
    if let Ok(dialplan) = Dialplan::parse(source) {
        if Dialplan::parse(&decorated).as_ref() != Ok(&dialplan) {
            return Err(OracleError::SettingsChanged);
        }
        accepted += 1;
    }
    for index in 0..7 {
        let mut files = [HTTP, ARI, "", "", "", "", ""];
        files[index] = source;
        if let Ok(config) = parse(files) {
            files[index] = &decorated;
            if parse(files).as_ref() != Ok(&config) || config.reload(&config) != Reload::Apply {
                return Err(OracleError::SettingsChanged);
            }
            accepted += 1;
        }
    }
    Ok(accepted)
}
