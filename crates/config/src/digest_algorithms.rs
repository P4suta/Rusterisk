use std::{fmt, str::FromStr};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DigestAlgorithm {
    Md5,
    Sha256,
    Sha512256,
}

impl DigestAlgorithm {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Md5 => "MD5",
            Self::Sha256 => "SHA-256",
            Self::Sha512256 => "SHA-512-256",
        }
    }

    fn parse(source: &str) -> Result<Self, InvalidDigestAlgorithms> {
        [Self::Md5, Self::Sha256, Self::Sha512256]
            .into_iter()
            .find(|algorithm| algorithm.name().eq_ignore_ascii_case(source.trim()))
            .ok_or(InvalidDigestAlgorithms)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Preference {
    One([DigestAlgorithm; 1]),
    Two([DigestAlgorithm; 2]),
    Three([DigestAlgorithm; 3]),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DigestAlgorithms(Preference);

impl Default for DigestAlgorithms {
    fn default() -> Self {
        Self(Preference::One([DigestAlgorithm::Md5]))
    }
}

impl DigestAlgorithms {
    pub fn iter(&self) -> impl ExactSizeIterator<Item = DigestAlgorithm> + '_ {
        let algorithms = match &self.0 {
            Preference::One(algorithms) => algorithms.as_slice(),
            Preference::Two(algorithms) => algorithms.as_slice(),
            Preference::Three(algorithms) => algorithms.as_slice(),
        };
        algorithms.iter().copied()
    }
}

impl FromStr for DigestAlgorithms {
    type Err = InvalidDigestAlgorithms;

    fn from_str(source: &str) -> Result<Self, Self::Err> {
        let mut parts = source.split(',');
        let first = DigestAlgorithm::parse(parts.next().ok_or(InvalidDigestAlgorithms)?)?;
        let Some(second) = parts.next() else {
            return Ok(Self(Preference::One([first])));
        };
        let second = DigestAlgorithm::parse(second)?;
        if first == second {
            return Err(InvalidDigestAlgorithms);
        }
        let Some(third) = parts.next() else {
            return Ok(Self(Preference::Two([first, second])));
        };
        let third = DigestAlgorithm::parse(third)?;
        if first == third || second == third || parts.next().is_some() {
            return Err(InvalidDigestAlgorithms);
        }
        Ok(Self(Preference::Three([first, second, third])))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidDigestAlgorithms;

impl fmt::Display for InvalidDigestAlgorithms {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("expected one to three distinct supported digest algorithms")
    }
}

impl std::error::Error for InvalidDigestAlgorithms {}
