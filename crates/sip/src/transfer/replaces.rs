use super::{request::visit_parameters, ReferError};
use std::collections::BTreeSet;

#[derive(Clone, PartialEq, Eq)]
pub struct Replaces {
    call_id: String,
    to_tag: String,
    from_tag: String,
    early_only: bool,
}

impl std::fmt::Debug for Replaces {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Replaces")
            .field("early_only", &self.early_only)
            .finish_non_exhaustive()
    }
}

impl Replaces {
    pub fn parse(source: &str) -> Result<Self, ReferError> {
        if source.len() > crate::MAX_MESSAGE {
            return Err(ReferError::Capacity);
        }
        if source
            .bytes()
            .any(|byte| byte.is_ascii_control() && byte != b'\t')
        {
            return Err(ReferError::Invalid);
        }
        let (call_id, parameters) = source.split_once(';').ok_or(ReferError::Invalid)?;
        let call_id = call_id.trim_matches([' ', '\t']);
        let (left, right) = call_id
            .split_once('@')
            .map_or((call_id, None), |(left, right)| (left, Some(right)));
        if !word(left) || right.is_some_and(|right| !word(right)) {
            return Err(ReferError::Invalid);
        }
        let mut names = BTreeSet::new();
        let mut to_tag = None;
        let mut from_tag = None;
        let mut early_only = false;
        visit_parameters(parameters, |name, value| {
            if !names.insert(name.to_ascii_lowercase()) {
                return Err(ReferError::Invalid);
            }
            if name.eq_ignore_ascii_case("to-tag") || name.eq_ignore_ascii_case("from-tag") {
                let tag = value
                    .filter(|tag| !tag.is_empty() && tag.bytes().all(token))
                    .ok_or(ReferError::Invalid)?;
                if name.eq_ignore_ascii_case("to-tag") {
                    to_tag = Some(tag);
                } else {
                    from_tag = Some(tag);
                }
            } else if name.eq_ignore_ascii_case("early-only") {
                if value.is_some() {
                    return Err(ReferError::Invalid);
                }
                early_only = true;
            }
            Ok(())
        })?;
        Ok(Self {
            call_id: call_id.to_owned(),
            to_tag: to_tag.ok_or(ReferError::Invalid)?.to_owned(),
            from_tag: from_tag.ok_or(ReferError::Invalid)?.to_owned(),
            early_only,
        })
    }

    pub fn call_id(&self) -> &str {
        &self.call_id
    }

    pub fn to_tag(&self) -> &str {
        &self.to_tag
    }

    pub fn from_tag(&self) -> &str {
        &self.from_tag
    }

    pub const fn early_only(&self) -> bool {
        self.early_only
    }
}

fn word(source: &str) -> bool {
    !source.is_empty()
        && source
            .bytes()
            .all(|byte| token(byte) || b"()<>:\\\"/[]?{}".contains(&byte))
}

fn token(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || b"-.!%*_+`'~".contains(&byte)
}
