use super::Rejection;
use crate::{Disconnection, RegistrationContact, RegistrationContacts, SignalingRoute};
use rusterisk_config::{Aor, Protocol};

#[derive(Clone)]
pub(crate) struct Binding {
    pub(super) address: String,
    pub(crate) contact: RegistrationContact,
    pub(crate) route: SignalingRoute,
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
    pub(super) route: SignalingRoute,
    pub(super) now: u64,
    pub(super) call_id: &'a str,
    pub(super) sequence: u32,
    pub(super) protocol: Protocol,
}

pub(crate) struct BindingIdentity {
    address: String,
    contact: String,
    pub(crate) route: SignalingRoute,
}

impl Binding {
    pub(crate) fn identity(&self) -> BindingIdentity {
        BindingIdentity {
            address: self.address.clone(),
            contact: self.contact.source_uri().into(),
            route: self.route,
        }
    }
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
    pub(crate) fn disconnected(&mut self, closed: Disconnection) {
        self.values.retain(|binding| !closed.matches(binding.route));
    }

    pub(crate) fn contains(&self, identity: &BindingIdentity, now: u64) -> bool {
        self.values.iter().any(|binding| {
            binding.address == identity.address
                && binding.contact.source_uri() == identity.contact
                && binding.route == identity.route
                && binding.expires > now
        })
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

    fn replace_earlier(&mut self, earlier: usize, update: &Update<'_>) -> Result<usize, Rejection> {
        let (index, binding) = self
            .values
            .iter()
            .take(earlier)
            .enumerate()
            .filter(|(_, binding)| binding.address == update.address.name)
            .min_by_key(|(_, binding)| binding.expires)
            .ok_or(Rejection::TooManyContacts)?;
        ordering(binding, update)?;
        self.values.remove(index);
        Ok(earlier.saturating_sub(1))
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
        let mut earlier = candidate.values.len();
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
            while candidate
                .values
                .iter()
                .filter(|binding| binding.address == update.address.name)
                .count()
                >= usize::from(update.address.max_contacts)
            {
                if !update.address.remove_existing {
                    return Err(Rejection::TooManyContacts);
                }
                earlier = candidate.replace_earlier(earlier, update)?;
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
                route: update.route,
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
