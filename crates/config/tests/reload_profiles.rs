use rusterisk_config::{Config, ConfigError, Reload, TlsFiles};

fn configuration() -> Result<Config, ConfigError> {
    configuration_with_rtp("[general]\nrtpstart=10000\nrtpend=10100\n")
}

fn configuration_with_rtp(rtp: &str) -> Result<Config, ConfigError> {
    Config::parse(
        "[general]\nbindaddr=127.0.0.1\ntlsenable=yes\ntlsbindaddr=127.0.0.1:8089\ntlscertfile=http-certificate.pem\ntlsprivatekey=http-key.pem\n",
        "[app]\ntype=user\npassword=test-password\n",
        "[secure]\ntype=transport\nprotocol=tls\nbind=127.0.0.1:5061\ncert_file=sip-certificate.pem\npriv_key_file=sip-key.pem\nca_list_file=roots.pem\n[datagram]\ntype=transport\nprotocol=udp\nbind=127.0.0.1:5060\n",
        rtp,
        "",
    )
}

#[test]
fn replacement_http_certificates_preserve_the_listener_identity() -> Result<(), ConfigError> {
    let current = configuration()?;
    let mut replacement = current.clone();
    replacement.http.tls = Some(TlsFiles {
        certificate: "replacement-certificate.pem".into(),
        private_key: "replacement-key.pem".into(),
    });
    assert_eq!(current.reload(&replacement), Reload::Apply);
    assert_eq!(replacement.http.bind, current.http.bind);
    assert_ne!(replacement.http.tls, current.http.tls);
    Ok(())
}

#[test]
fn sip_certificates_and_trust_roots_are_starting_profiles() -> Result<(), ConfigError> {
    let current = configuration()?;
    let mut replacement = current.clone();
    replacement.transports[0].tls = Some(TlsFiles {
        certificate: "replacement-sip-certificate.pem".into(),
        private_key: "replacement-sip-key.pem".into(),
    });
    replacement.transports[0].ca_list_file = Some("replacement-roots.pem".into());
    assert_eq!(current.reload(&replacement), Reload::Apply);
    assert_eq!(replacement.transports[0].bind, current.transports[0].bind);
    Ok(())
}

#[test]
fn advertised_media_addresses_apply_to_new_calls_without_rebinding() -> Result<(), ConfigError> {
    let current = configuration()?;
    let mut replacement = current.clone();
    replacement.transports[1].external_media_address = Some([192, 0, 2, 10].into());
    assert_eq!(current.reload(&replacement), Reload::Apply);
    assert_eq!(replacement.transports[1].bind, current.transports[1].bind);
    Ok(())
}

#[test]
fn ice_profile_replacement_preserves_the_rtp_socket_range() -> Result<(), ConfigError> {
    let current = configuration()?;
    let replacement = configuration_with_rtp("[general]\nrtpstart=10000\nrtpend=10100\nstunaddr=stun.example:3478\nturnaddr=turn.example:3478\nturnusername=replacement-user\nturnpassword=replacement-password\n")?;
    assert_eq!(current.reload(&replacement), Reload::Apply);
    assert_eq!(replacement.rtp.start, current.rtp.start);
    assert_eq!(replacement.rtp.end, current.rtp.end);
    Ok(())
}

#[test]
fn listener_addresses_encryption_modes_and_transport_identity_require_restart(
) -> Result<(), ConfigError> {
    let current = configuration()?;
    let mut moved_http = current.clone();
    moved_http.http.bind.set_port(8090);
    let mut plaintext_http = current.clone();
    plaintext_http.http.tls = None;
    let mut moved_sip = current.clone();
    moved_sip.transports[1].bind.set_port(5070);
    let mut renamed_sip = current.clone();
    renamed_sip.transports[1].name = "replacement-listener".into();
    let mut changed_protocol = current.clone();
    changed_protocol.transports[1].protocol = rusterisk_config::Protocol::Tcp;
    let mut reordered = current.clone();
    reordered.transports.swap(0, 1);
    let mut removed = current.clone();
    assert!(removed.transports.pop().is_some());
    for replacement in [
        moved_http,
        plaintext_http,
        moved_sip,
        renamed_sip,
        changed_protocol,
        reordered,
        removed,
    ] {
        assert_eq!(current.reload(&replacement), Reload::RestartRequired);
    }
    Ok(())
}

#[test]
fn rtp_socket_range_changes_require_restart() -> Result<(), ConfigError> {
    let current = configuration()?;
    for (start, end) in [(10002, 10100), (10000, 10102)] {
        let mut replacement = current.clone();
        replacement.rtp.start = start;
        replacement.rtp.end = end;
        assert_eq!(current.reload(&replacement), Reload::RestartRequired);
    }
    Ok(())
}
