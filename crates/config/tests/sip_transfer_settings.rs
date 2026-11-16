use rusterisk_config::{Config, ConfigError};

const ENDPOINT: &str = "[udp]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:5060\n[credential]\ntype=auth\nusername=phone\npassword=sample\n[phone]\ntype=aor\nmax_contacts=1\n[phone-endpoint]\ntype=endpoint\ntransport=udp\nauth=credential\naors=phone\ncontext=from-phones\n";

fn config(settings: &str) -> Result<Config, ConfigError> {
    Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=sample\n",
        &format!("{ENDPOINT}{settings}"),
        "",
        "",
    )
}

#[test]
fn endpoint_transfer_policy_defaults_match_the_reference() -> Result<(), ConfigError> {
    let config = config("")?;
    let endpoint = &config.endpoints[0];
    assert!(endpoint.allow_transfer);
    assert!(endpoint.refer_blind_progress);
    assert_eq!(endpoint.name, "phone-endpoint");
    assert_eq!(endpoint.context, "from-phones");
    assert_eq!(endpoint.auth, "credential");
    assert_eq!(endpoint.aors, ["phone"]);
    Ok(())
}

#[test]
fn explicit_transfer_flags_preserve_independent_endpoint_policy() -> Result<(), ConfigError> {
    for (allow, progress) in [(true, true), (true, false), (false, true), (false, false)] {
        let boolean = |value| if value { "yes" } else { "no" };
        let source = format!(
            "allow_transfer={}\nrefer_blind_progress={}\n",
            boolean(allow),
            boolean(progress)
        );
        let config = config(&source)?;
        let endpoint = &config.endpoints[0];
        assert_eq!(endpoint.allow_transfer, allow);
        assert_eq!(endpoint.refer_blind_progress, progress);
        assert_eq!(endpoint.context, "from-phones");
    }
    Ok(())
}

#[test]
fn malformed_transfer_flags_identify_the_option_line_and_expected_values() {
    for key in ["allow_transfer", "refer_blind_progress"] {
        for value in ["", "maybe", "2", "yes,no"] {
            let settings = format!("\n{key}={value}\n");
            let line = ENDPOINT.lines().count() + 2;
            assert!(
                matches!(config(&settings), Err(error) if error.file == "pjsip.conf" && error.line == line && error.reason == format!("{key} must be yes or no")),
                "{key}={value}"
            );
        }
    }
}
