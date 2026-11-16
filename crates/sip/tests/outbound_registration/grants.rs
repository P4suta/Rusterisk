use super::{
    responses::{final_response, response},
    settings, transmitted,
};
use rusterisk_sip::{
    RegistrationGrant, RegistrationIdentity, RegistrationResponse, RegistrationTransaction,
    SipError,
};

const OWN: &str = "sip:line+1@127.0.0.1:5068;transport=udp";

fn receipt(extra: &str, expiration: u32) -> Result<RegistrationResponse, SipError> {
    receipt_with_status(extra, expiration, 200)
}

fn receipt_with_status(
    extra: &str,
    expiration: u32,
    status: u16,
) -> Result<RegistrationResponse, SipError> {
    let mut config = settings("udp", "")?;
    config.expiration = expiration;
    let mut pending = RegistrationTransaction::new(
        &config,
        "127.0.0.1:5068".parse()?,
        "127.0.0.2:5070".parse()?,
        RegistrationIdentity::new(0x1234, 7, 0x5678)?,
        0,
    )?;
    let packet = transmitted(pending.poll(0)?)?;
    final_response(pending.receive(&response(&packet, status, extra)?, packet.peer, 1000)?)
}

#[test]
fn only_successful_final_responses_can_grant_bindings() -> Result<(), SipError> {
    let headers = format!("Contact: <{OWN}>;expires=30\r\n");
    for status in [200, 201, 299] {
        assert_eq!(
            receipt_with_status(&headers, 3600, status)?.grant()?,
            RegistrationGrant::Active {
                expires_at: 31_000,
                refresh_at: 21_000
            }
        );
    }
    for status in [300, 401, 423, 500, 699] {
        assert!(receipt_with_status(&headers, 3600, status)?
            .grant()
            .is_err());
    }
    Ok(())
}

#[test]
fn matching_contacts_supply_the_granted_expiration_and_reference_refresh_buffer(
) -> Result<(), SipError> {
    let headers = "Contact: <sip:other@127.0.0.1:5069>;expires=900\r\nContact: <SIP:l%69ne+1@127.0.0.1:5068;transport=UDP>;expires=45\r\nExpires: 99\r\n";
    assert_eq!(
        receipt(headers, 3600)?.grant()?,
        RegistrationGrant::Active {
            expires_at: 46_000,
            refresh_at: 36_000
        }
    );
    let headers = format!("Contact: <{OWN}>\r\nExpires: 120\r\n");
    assert_eq!(
        receipt(&headers, 3600)?.grant()?,
        RegistrationGrant::Active {
            expires_at: 121_000,
            refresh_at: 111_000
        }
    );
    Ok(())
}

#[test]
fn unrelated_uri_schemes_do_not_hide_the_owned_contact() -> Result<(), SipError> {
    let headers = format!("Contact: <tel:+12125550123>;expires=900, <{OWN}>;expires=45\r\n");
    assert_eq!(
        receipt(&headers, 3600)?.grant()?,
        RegistrationGrant::Active {
            expires_at: 46_000,
            refresh_at: 36_000
        }
    );
    Ok(())
}

#[test]
fn missing_or_multiple_matches_never_infer_a_grant_from_the_requested_interval(
) -> Result<(), SipError> {
    for headers in [
        "".to_owned(),
        "Contact: <sip:other@127.0.0.1:5068;transport=udp>;expires=90\r\n".to_owned(),
        format!("Contact: <{OWN}>\r\n"),
        format!("Contact: <{OWN};extension=one>;expires=30, <{OWN};extension=two>;expires=40\r\n"),
        format!("Contact: <{OWN}>;expires=30\r\nContact: <{OWN}>;expires=40\r\n"),
        "Contact: <sip:line%2b1@127.0.0.1:5068;transport=udp>;expires=90\r\n".to_owned(),
    ] {
        assert!(receipt(&headers, 3600)?.grant().is_err(), "{headers}");
    }
    Ok(())
}

#[test]
fn short_expiration_grants_refresh_immediately_without_extending_the_binding(
) -> Result<(), SipError> {
    for expires in [1_u64, 5, 10, 11] {
        let headers = format!("Contact: <{OWN}>;expires={expires}\r\n");
        let grant = receipt(&headers, 3600)?.grant()?;
        assert_eq!(
            grant,
            RegistrationGrant::Active {
                expires_at: 1000 + expires * 1000,
                refresh_at: 1000 + expires.saturating_sub(10) * 1000
            }
        );
    }
    Ok(())
}

#[test]
fn removal_requires_an_absent_or_zero_lifetime_owned_contact() -> Result<(), SipError> {
    for headers in [
        "".to_owned(),
        "Contact: <sip:other@127.0.0.1:5069>;expires=90\r\n".to_owned(),
        format!("Contact: <{OWN}>;expires=0\r\n"),
    ] {
        assert_eq!(receipt(&headers, 0)?.grant()?, RegistrationGrant::Removed);
    }
    assert!(receipt(&format!("Contact: <{OWN}>;expires=90\r\n"), 0)?
        .grant()
        .is_err());
    assert!(receipt(&format!("Contact: <{OWN}>;expires=0\r\n"), 3600)?
        .grant()
        .is_err());
    assert!(receipt("Contact: *\r\nExpires: 0\r\n", 0)?.grant().is_err());
    Ok(())
}

#[test]
fn malformed_or_ambiguous_expirations_and_contact_capacity_are_rejected() -> Result<(), SipError> {
    for expires in ["-1", "1.5", "4294967296", "\"30\""] {
        assert!(
            receipt(&format!("Contact: <{OWN}>\r\nExpires: {expires}\r\n"), 3600)
                .and_then(|response| response.grant())
                .is_err()
        );
        assert!(
            receipt(&format!("Contact: <{OWN}>;expires={expires}\r\n"), 3600)
                .and_then(|response| response.grant())
                .is_err()
        );
    }
    assert!(receipt(
        &format!("Contact: <{OWN}>\r\nExpires: 30\r\nExpires: 40\r\n"),
        3600
    )
    .and_then(|response| response.grant())
    .is_err());
    let mut headers = format!("Contact: <{OWN}>;expires=30\r\n");
    for index in 0..32 {
        headers.push_str(&format!(
            "Contact: <sip:other{index}@127.0.0.1:5069>;expires=90\r\n"
        ));
    }
    assert!(matches!(
        receipt(&headers, 3600)?.grant(),
        Err(SipError::Capacity)
    ));
    Ok(())
}

#[test]
fn unsigned_expiration_seconds_are_checked_before_absolute_deadline_construction(
) -> Result<(), SipError> {
    let headers = format!("Contact: <{OWN}>;expires={}\r\n", u32::MAX);
    assert_eq!(
        receipt(&headers, 3600)?.grant()?,
        RegistrationGrant::Active {
            expires_at: 1000 + u64::from(u32::MAX) * 1000,
            refresh_at: 1000 + u64::from(u32::MAX - 10) * 1000
        }
    );
    let config = settings("udp", "")?;
    let now = u64::MAX - 32_000;
    let mut pending = RegistrationTransaction::new(
        &config,
        "127.0.0.1:5068".parse()?,
        "127.0.0.2:5070".parse()?,
        RegistrationIdentity::new(0x1234, 7, 0x5678)?,
        now,
    )?;
    let packet = transmitted(pending.poll(now)?)?;
    let final_event = final_response(pending.receive(
        &response(&packet, 200, &format!("Contact: <{OWN}>;expires=30\r\n"))?,
        packet.peer,
        u64::MAX - 1,
    )?)?;
    assert!(matches!(final_event.grant(), Err(SipError::Capacity)));
    Ok(())
}
