use rusterisk_config::{Config, Registration, RegistrationAddress};
use rusterisk_sip::{
    RegistrationIdentity, RegistrationTransaction, RegistrationTransactionEvent, SipError,
};

fn settings(scheme: &str) -> Result<Registration, SipError> {
    let config = Config::parse(
        "[general]\nbindaddr=127.0.0.1\n",
        "[app]\ntype=user\npassword=test\n",
        &format!("[native]\ntype=transport\nprotocol=tls\nbind=127.0.0.1:5061\ncert_file=certificate.pem\npriv_key_file=private.pem\n[auth]\ntype=auth\nusername=account\npassword=sample\n[provider]\ntype=registration\ntransport=native\noutbound_auth=auth\nserver_uri={scheme}:provider.example\nclient_uri={scheme}:account@provider.example\ncontact_user=line\n"),
        "",
        "",
    )?;
    config
        .registrations
        .into_iter()
        .next()
        .ok_or(SipError::InvalidPacket)
}

#[test]
fn encrypted_register_contacts_follow_the_address_role_and_use_the_tcp_parameter(
) -> Result<(), SipError> {
    for scheme in ["sip", "sips"] {
        let mut transaction = RegistrationTransaction::new(
            &settings(scheme)?,
            "127.0.0.1:5061".parse()?,
            "127.0.0.2:5071".parse()?,
            RegistrationIdentity::new(1, 1, 2)?,
            0,
        )?;
        let Some(RegistrationTransactionEvent::Transmit(packet)) = transaction.poll(0)? else {
            return Err(SipError::InvalidPacket);
        };
        let wire = std::str::from_utf8(&packet.bytes)?;
        assert!(wire.contains("Via: SIP/2.0/TLS "));
        assert!(wire.contains(&format!(
            "Contact: <{scheme}:line@127.0.0.1:5061;transport=tcp>\r\n"
        )));
        assert!(wire.contains(&format!("To: <{scheme}:account@provider.example>\r\n")));
        assert!(!wire.contains("transport=tls"));
    }
    Ok(())
}

#[test]
fn mutating_public_registration_settings_cannot_bypass_scheme_requirements() -> Result<(), SipError>
{
    for (server_scheme, client_scheme) in [("sip", "sips"), ("sips", "sip")] {
        let mut settings = settings(server_scheme)?;
        settings.client_uri =
            RegistrationAddress::parse(&format!("{client_scheme}:account@provider.example"))
                .map_err(|_| SipError::InvalidPacket)?;
        assert!(matches!(
            RegistrationTransaction::new(
                &settings,
                "127.0.0.1:5061".parse()?,
                "127.0.0.2:5071".parse()?,
                RegistrationIdentity::new(1, 1, 2)?,
                0,
            ),
            Err(SipError::Unsupported)
        ));
    }
    Ok(())
}
