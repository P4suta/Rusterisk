use rusterisk_config::{Config, ConfigError, Ini};

fn management_value(source: &str) -> Result<String, ConfigError> {
    let config = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        &format!("[app]\ntype=user\n{source}\n"),
        "",
        "",
        "",
    )?;
    Ok(config.users[0].password.expose().into())
}

#[test]
fn escaped_semicolons_keep_values_and_remove_only_the_adjacent_backslash() -> Result<(), ConfigError>
{
    for (source, expected) in [
        (r"password=one\;two ; comment", "one;two"),
        (r"password=one\\;two", r"one\;two"),
        (r"password=one\\\;two", r"one\\;two"),
        (r"password=one\;two\;three; comment", "one;two;three"),
        (r"password=C:\folder\file", r"C:\folder\file"),
        (r"password=trailing\", r"trailing\"),
        ("password=Jäsøn\\;Doe ; comment", "Jäsøn;Doe"),
    ] {
        assert_eq!(management_value(source)?, expected, "{source}");
    }
    Ok(())
}

#[test]
fn quotation_marks_do_not_protect_unescaped_semicolons() -> Result<(), ConfigError> {
    for (source, expected) in [
        (r#"password="one;two""#, "\"one"),
        (r#"password="one\;two""#, "\"one;two\""),
        (r"password=literal;\;not-a-value", "literal"),
    ] {
        assert_eq!(management_value(source)?, expected, "{source}");
    }
    Ok(())
}

#[test]
fn signaling_uris_and_management_secrets_share_the_same_escape_rule() -> Result<(), ConfigError> {
    let config = Config::parse("[general]\nbindaddr=127.0.0.1\n", "[app]\ntype=user\npassword=sample\\;secret\n", "[stream]\ntype=transport\nprotocol=tcp\nbind=127.0.0.1:5060\n[credential]\ntype=auth\nusername=phone\npassword=sample\\;secret\n[phone]\ntype=aor\ncontact=sip:phone@127.0.0.1:5061\\;transport=tcp\n[phone]\ntype=endpoint\ntransport=stream\nauth=credential\naors=phone\n", "", "")?;
    assert_eq!(config.users[0].password.expose(), "sample;secret");
    assert_eq!(config.auth[0].password.expose(), "sample;secret");
    assert_eq!(
        config.aors[0].contact.as_deref(),
        Some("sip:phone@127.0.0.1:5061;transport=tcp")
    );
    Ok(())
}

#[test]
fn comment_processing_preserves_physical_error_lines_and_source_limits() {
    assert!(
        matches!(Ini::parse("example.conf", "[section]\nvalue=one\\;two\nvalue=duplicate\n"), Err(error) if error.file == "example.conf" && error.line == 3)
    );
    assert!(
        matches!(Ini::parse("example.conf", "[section]\nvalue=one\\;two\nmalformed\n"), Err(error) if error.file == "example.conf" && error.line == 3)
    );
    let source = format!("[section]\n;{}\n", "x".repeat(4096));
    assert!(
        matches!(Ini::parse("example.conf", &source), Err(error) if error.file == "example.conf" && error.line == 2)
    );
}
