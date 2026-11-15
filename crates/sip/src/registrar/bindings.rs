use super::Rejection;
use crate::{RegistrationContact, RegistrationContacts};
use rusterisk_config::{Aor, Protocol};
use std::net::SocketAddr;

#[derive(Clone)]
pub(crate) struct Binding {
    pub(super) address: String,
    pub(crate) contact: RegistrationContact,
    pub(crate) peer: SocketAddr,
    pub(super) expires: u64,
    call_id: String,
    sequence: u32,
}

#[derive(Clone, Default)]
pub(crate) struct Bindings {
    values: Vec<Binding>,
}

pub(super) struct Update<'a> {
    pub(super) address: &'a Aor,
    pub(super) peer: SocketAddr,
    pub(super) now: u64,
    pub(super) call_id: &'a str,
    pub(super) sequence: u32,
    pub(super) protocol: Protocol,
}

impl Bindings {
    pub(crate) fn best(&self, addresses: &[String], now: u64) -> Option<&Binding> {
        addresses.iter().find_map(|address| {
            self.values
                .iter()
                .filter(|binding| binding.address == *address && binding.expires > now)
                .max_by(|left, right| {
                    left.contact
                        .preference()
                        .cmp(&right.contact.preference())
                        .then_with(|| right.contact.source_uri().cmp(left.contact.source_uri()))
                })
        })
    }

    pub(crate) fn present(&self, addresses: &[String]) -> bool {
        self.values
            .iter()
            .any(|binding| addresses.contains(&binding.address))
    }

    pub(crate) fn expire(&mut self, now: u64) {
        self.values.retain(|binding| binding.expires > now);
    }
    pub(crate) fn disconnected(&mut self, peer: SocketAddr) {
        self.values.retain(|binding| binding.peer != peer);
    }

    fn matching(
        &self,
        address: &str,
        contact: &RegistrationContact,
    ) -> Result<Option<usize>, Rejection> {
        let mut matching = self.values.iter().enumerate().filter(|(_, binding)| {
            binding.address == address && binding.contact.equivalent(contact)
        });
        let result = matching.next().map(|(index, _)| index);
        if matching.next().is_some() {
            return Err(Rejection::Invalid);
        }
        Ok(result)
    }

    pub(super) fn prepare(
        &self,
        update: &Update<'_>,
        contacts: RegistrationContacts,
    ) -> Result<Self, Rejection> {
        if update.address.minimum_expiration > update.address.maximum_expiration
            || update.sequence >= 1 << 31
        {
            return Err(Rejection::Invalid);
        }
        if update.address.max_contacts > 16 {
            return Err(Rejection::TooManyContacts);
        }
        let mut candidate = self.clone();
        candidate.expire(update.now);
        let changes = match contacts {
            RegistrationContacts::Query => return Ok(candidate),
            RegistrationContacts::RemoveAll => {
                for binding in candidate
                    .values
                    .iter()
                    .filter(|binding| binding.address == update.address.name)
                {
                    ordering(binding, update)?;
                }
                candidate
                    .values
                    .retain(|binding| binding.address != update.address.name);
                return Ok(candidate);
            }
            RegistrationContacts::Bindings(contacts) => contacts,
        };
        for (index, contact) in changes.as_slice().iter().enumerate() {
            if changes.as_slice()[..index]
                .iter()
                .any(|other| other.equivalent(contact))
            {
                return Err(Rejection::Invalid);
            }
            let secure = match update.protocol {
                Protocol::Tls | Protocol::Wss => true,
                Protocol::Udp | Protocol::Tcp | Protocol::Ws => false,
            };
            if matches!(
                contact.uri().scheme,
                Some(rsipstack::sip::uri::Scheme::Sips)
            ) && !secure
            {
                return Err(Rejection::Forbidden);
            }
            if let Some(index) = candidate.matching(&update.address.name, contact)? {
                ordering(&candidate.values[index], update)?;
                candidate.values.remove(index);
            }
        }
        for contact in changes.as_slice() {
            let requested = contact
                .expires()
                .unwrap_or(update.address.default_expiration);
            if requested == 0 {
                continue;
            }
            let expires = requested.clamp(
                update.address.minimum_expiration,
                update.address.maximum_expiration,
            );
            if expires == 0 {
                continue;
            }
            if candidate
                .values
                .iter()
                .filter(|binding| binding.address == update.address.name)
                .count()
                >= usize::from(update.address.max_contacts)
            {
                return Err(Rejection::TooManyContacts);
            }
            if candidate.values.len() == 128 {
                return Err(Rejection::Capacity);
            }
            let expires = update
                .now
                .checked_add(u64::from(expires) * 1000)
                .ok_or(Rejection::Capacity)?;
            candidate.values.push(Binding {
                address: update.address.name.clone(),
                contact: contact.clone(),
                peer: update.peer,
                expires,
                call_id: update.call_id.into(),
                sequence: update.sequence,
            });
        }
        Ok(candidate)
    }

    pub(super) fn response(&self, address: &Aor, now: u64) -> Result<String, Rejection> {
        let mut output = String::new();
        if let Some(contact) = &address.contact {
            append(&mut output, &format!("Contact: <{contact}>\r\n"))?;
        }
        for binding in self
            .values
            .iter()
            .filter(|binding| binding.address == address.name && binding.expires > now)
        {
            let expires = binding.expires.saturating_sub(now).div_ceil(1000);
            let quality = binding.contact.preference();
            append(
                &mut output,
                &format!(
                    "Contact: <{}>;expires={expires};q={}.{:03}\r\n",
                    binding.contact.source_uri(),
                    quality / 1000,
                    quality % 1000
                ),
            )?;
        }
        Ok(output)
    }
}

fn ordering(binding: &Binding, update: &Update<'_>) -> Result<(), Rejection> {
    if binding.call_id == update.call_id && binding.sequence >= update.sequence {
        Err(Rejection::StaleSequence)
    } else {
        Ok(())
    }
}

fn append(output: &mut String, line: &str) -> Result<(), Rejection> {
    if output
        .len()
        .checked_add(line.len())
        .is_none_or(|length| length > crate::MAX_MESSAGE)
    {
        return Err(Rejection::Capacity);
    }
    output.push_str(line);
    Ok(())
}
