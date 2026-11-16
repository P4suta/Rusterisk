use crate::{parse_registration_contacts, ContactError, Engine, SipError, SipEvent};
use rsipstack::sip::{headers::untyped::ToTypedHeader, HeadersExt, Request, Uri};
use rusterisk_config::{Aor, Config, Endpoint};
use rusterisk_core::ContactPresence;
use std::net::SocketAddr;
mod bindings;
mod security;
pub(super) use bindings::Bindings;

pub(super) enum Rejection {
    Forbidden,
    NotFound,
    Invalid,
    Unsupported,
    TooManyContacts,
    Capacity,
    StaleSequence,
}

impl Rejection {
    pub(super) fn response(self) -> (u16, &'static str) {
        match self {
            Self::Forbidden => (403, "Forbidden"),
            Self::NotFound => (404, "Not Found"),
            Self::Invalid => (400, "Bad Request"),
            Self::Unsupported => (416, "Unsupported URI Scheme"),
            Self::TooManyContacts => (403, "Forbidden"),
            Self::Capacity => (503, "Service Unavailable"),
            Self::StaleSequence => (500, "Server Internal Error"),
        }
    }
}

impl From<ContactError> for Rejection {
    fn from(error: ContactError) -> Self {
        match error {
            ContactError::Invalid | ContactError::Capacity => Self::Invalid,
            ContactError::Unsupported => Self::Unsupported,
        }
    }
}

impl Engine {
    pub(super) fn contact_snapshot(&self) -> Vec<bool> {
        self.config
            .endpoints
            .iter()
            .map(|endpoint| self.contacts.present(&endpoint.aors))
            .collect()
    }

    pub(super) fn contact_changes(&self, before: &[bool], events: &mut Vec<SipEvent>) {
        for (endpoint, before) in self.config.endpoints.iter().zip(before) {
            let present = self.contacts.present(&endpoint.aors);
            if *before != present {
                events.push(SipEvent::EndpointContact {
                    endpoint: endpoint.name.clone(),
                    generation: self.configuration_generation,
                    presence: if present {
                        ContactPresence::Present
                    } else {
                        ContactPresence::Absent
                    },
                });
            }
        }
    }

    pub(super) fn register(
        &mut self,
        request: &Request,
        endpoint: &Endpoint,
        peer: SocketAddr,
        now: u64,
        events: &mut Vec<SipEvent>,
    ) -> Result<Vec<u8>, SipError> {
        let to = request.to_header()?.typed()?;
        let address = match address(&self.config, endpoint, &to.uri) {
            Ok(address) => address,
            Err(rejection) => {
                let (code, reason) = rejection.response();
                return self.render_response(request, code, reason, "", "");
            }
        };
        let prepared = (|| {
            let mut expiration = None;
            for header in request
                .headers
                .iter()
                .filter(|header| header.name().eq_ignore_ascii_case("Expires"))
            {
                if expiration.is_some()
                    || header.value().is_empty()
                    || !header.value().bytes().all(|byte| byte.is_ascii_digit())
                {
                    return Err(Rejection::Invalid);
                }
                expiration = Some(
                    header
                        .value()
                        .parse::<u32>()
                        .map_err(|_| Rejection::Invalid)?,
                );
            }
            let contacts = parse_registration_contacts(
                request
                    .headers
                    .iter()
                    .filter(|header| header.name().eq_ignore_ascii_case("Contact"))
                    .map(|header| header.value()),
                expiration,
            )?;
            security::validate(request, &contacts)?;
            let sequence = request
                .cseq_header()
                .map_err(|_| Rejection::Invalid)?
                .typed()
                .map_err(|_| Rejection::Invalid)?
                .seq;
            let call_id = request
                .call_id_header()
                .map_err(|_| Rejection::Invalid)?
                .value();
            let candidate = self.contacts.prepare(
                &bindings::Update {
                    address,
                    peer,
                    now,
                    call_id,
                    sequence,
                    protocol: self.protocol,
                },
                contacts,
            )?;
            let extra = candidate.response(address, now)?;
            Ok((candidate, extra))
        })();
        let (candidate, extra) = match prepared {
            Ok(prepared) => prepared,
            Err(rejection) => {
                let (code, reason) = rejection.response();
                return self.render_response(request, code, reason, "", "");
            }
        };
        let reply = match self.render_response(request, 200, "OK", &extra, "") {
            Ok(reply) => reply,
            Err(SipError::Capacity) => {
                return self.render_response(request, 503, "Service Unavailable", "", "")
            }
            Err(error) => return Err(error),
        };
        let before = self.contact_snapshot();
        self.contacts = candidate;
        self.contact_changes(&before, events);
        Ok(reply)
    }
}

pub(super) fn address<'a>(
    config: &'a Config,
    endpoint: &Endpoint,
    target: &Uri,
) -> Result<&'a Aor, Rejection> {
    if endpoint.aors.is_empty() {
        return Err(Rejection::Forbidden);
    }
    let user = target.user().ok_or(Rejection::NotFound)?;
    let aor = config
        .aors
        .iter()
        .find(|aor| aor.name == user && endpoint.aors.contains(&aor.name))
        .ok_or(Rejection::NotFound)?;
    if aor.max_contacts == 0 {
        return Err(Rejection::Forbidden);
    }
    Ok(aor)
}
