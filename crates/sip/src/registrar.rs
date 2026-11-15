use rsipstack::sip::Uri;
use rusterisk_config::{Aor, Config, Endpoint};

pub(super) enum Rejection {
    Forbidden,
    NotFound,
}

impl Rejection {
    pub(super) fn response(self) -> (u16, &'static str) {
        match self {
            Self::Forbidden => (403, "Forbidden"),
            Self::NotFound => (404, "Not Found"),
        }
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
