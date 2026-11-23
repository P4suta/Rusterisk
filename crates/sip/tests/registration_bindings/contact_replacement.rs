use super::{contacts, response, Registrar};
use rusterisk_sip::SipError;
use std::net::SocketAddr;

const FIRST: &str = "Contact: <sip:first@old.invalid>\r\n";
const SECOND: &str = "Contact: <sip:second@old.invalid>\r\n";
const CURRENT: &str = "Contact: <sip:current@new.invalid>\r\n";
const SPARE: &str = "Contact: <sip:spare@new.invalid>\r\n";
const THIRD: &str = "Contact: <sip:third@new.invalid>\r\n";

struct Telephone {
    registrar: Registrar,
    old: SocketAddr,
    current: SocketAddr,
}

impl Telephone {
    fn with_two_earlier_contacts() -> Result<Self, SipError> {
        let mut telephone = Self {
            registrar: Registrar::new("remove_existing=yes\n")?,
            old: "127.0.0.1:5100".parse()?,
            current: "127.0.0.1:5101".parse()?,
        };
        telephone
            .registrar
            .request(telephone.old, "phone", FIRST, Some(30), 0)?;
        telephone
            .registrar
            .request(telephone.old, "phone", SECOND, Some(10), 2)?;
        Ok(telephone)
    }

    fn bindings(&mut self, address: &str, now: u64) -> Result<Vec<(String, u32)>, SipError> {
        contacts(&self.registrar.request(self.old, address, "", None, now)?)
    }
}

#[test]
fn a_registration_over_the_limit_replaces_the_earlier_contact_that_expires_soonest(
) -> Result<(), SipError> {
    let mut telephone = Telephone::with_two_earlier_contacts()?;
    let replaced = telephone
        .registrar
        .request(telephone.current, "phone", CURRENT, Some(60), 4)?;
    assert_eq!(
        contacts(&replaced)?,
        vec![
            ("sip:first@old.invalid".into(), 30),
            ("sip:current@new.invalid".into(), 60)
        ]
    );
    Ok(())
}

#[test]
fn a_contact_that_the_registration_updates_is_never_the_one_replaced() -> Result<(), SipError> {
    let mut telephone = Telephone::with_two_earlier_contacts()?;
    let updated = telephone.registrar.request(
        telephone.current,
        "phone",
        &format!("{SECOND}{CURRENT}"),
        Some(5),
        4,
    )?;
    assert_eq!(
        contacts(&updated)?,
        vec![
            ("sip:second@old.invalid".into(), 5),
            ("sip:current@new.invalid".into(), 5)
        ]
    );
    Ok(())
}

#[test]
fn a_registration_that_adds_more_contacts_than_the_limit_changes_nothing() -> Result<(), SipError> {
    let mut telephone = Telephone::with_two_earlier_contacts()?;
    let refused = telephone.registrar.request(
        telephone.current,
        "phone",
        &format!("{CURRENT}{SPARE}{THIRD}"),
        Some(60),
        4,
    )?;
    assert_eq!(response(&refused)?.status_code.code(), 403);
    assert_eq!(
        telephone.bindings("phone", 6)?,
        vec![
            ("sip:first@old.invalid".into(), 30),
            ("sip:second@old.invalid".into(), 10)
        ]
    );
    Ok(())
}

#[test]
fn replacement_leaves_the_contacts_of_another_address_of_record() -> Result<(), SipError> {
    let mut telephone = Telephone::with_two_earlier_contacts()?;
    telephone
        .registrar
        .request(telephone.old, "backup", FIRST, Some(20), 4)?;
    telephone
        .registrar
        .request(telephone.current, "phone", CURRENT, Some(60), 6)?;
    assert_eq!(
        telephone.bindings("backup", 8)?,
        vec![("sip:first@old.invalid".into(), 20)]
    );
    Ok(())
}
