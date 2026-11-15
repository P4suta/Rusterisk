use crate::SipError;
use rusterisk_core::DtmfDigit;
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DtmfFormat {
    payload_type: u8,
    rate: u32,
    events: u16,
}

impl DtmfFormat {
    pub fn parse_all(source: &str) -> Result<Vec<Self>, SipError> {
        if source.len() > crate::MAX_MESSAGE {
            return Err(SipError::Capacity);
        }
        let mut payloads = None;
        let mut mappings = BTreeMap::new();
        let mut parameters = BTreeMap::new();
        for line in source.lines().map(str::trim) {
            if let Some(media) = line.strip_prefix("m=") {
                if payloads.is_some() {
                    return Err(SipError::Unsupported);
                }
                let parts: Vec<&str> = media.split_whitespace().take(20).collect();
                let ["audio", _port, _protocol, formats @ ..] = parts.as_slice() else {
                    return Err(SipError::Unsupported);
                };
                if formats.is_empty() || formats.len() > 16 {
                    return Err(SipError::InvalidPacket);
                }
                let mut values = Vec::with_capacity(16);
                for format in formats {
                    let payload = format.parse::<u8>().map_err(|_| SipError::InvalidPacket)?;
                    if payload > 127 {
                        return Err(SipError::InvalidPacket);
                    }
                    values.push(payload);
                }
                payloads = Some(values);
            } else if let Some(value) = line.strip_prefix("a=rtpmap:") {
                let (payload, encoding) = value.split_once(' ').ok_or(SipError::InvalidPacket)?;
                let payload = payload.parse::<u8>().map_err(|_| SipError::InvalidPacket)?;
                if payload > 127 {
                    return Err(SipError::InvalidPacket);
                }
                let rate = match encoding.to_ascii_lowercase().as_str() {
                    "telephone-event/8000" => 8000,
                    "telephone-event/48000" => 48000,
                    _ => continue,
                };
                mappings.insert(payload, rate);
            } else if let Some(value) = line.strip_prefix("a=fmtp:") {
                let (payload, value) = value.split_once(' ').ok_or(SipError::InvalidPacket)?;
                let payload = payload.parse::<u8>().map_err(|_| SipError::InvalidPacket)?;
                if payload > 127 || parameters.insert(payload, value).is_some() {
                    return Err(SipError::InvalidPacket);
                }
            }
        }
        let payloads = payloads.ok_or(SipError::InvalidPacket)?;
        let mut formats = Vec::with_capacity(16);
        for (payload, rate) in mappings {
            if !payloads.contains(&payload) {
                continue;
            }
            let mut format = Self::new(payload, rate)?;
            if let Some(value) = parameters.get(&payload) {
                format = format.with_parameters(value)?;
            }
            if format.events != 0 {
                formats.push(format);
            }
        }
        Ok(formats)
    }

    pub fn new(payload_type: u8, rate: u32) -> Result<Self, SipError> {
        if payload_type > 127 || !matches!(rate, 8000 | 48000) {
            return Err(SipError::Unsupported);
        }
        Ok(Self {
            payload_type,
            rate,
            events: u16::MAX,
        })
    }

    pub const fn payload_type(self) -> u8 {
        self.payload_type
    }
    pub const fn rate(self) -> u32 {
        self.rate
    }
    pub const fn events(self) -> u16 {
        self.events
    }
    pub const fn supports(self, digit: DtmfDigit) -> bool {
        self.events & (1 << digit.event()) != 0
    }

    pub(crate) fn with_parameters(mut self, value: &str) -> Result<Self, SipError> {
        self.events = 0;
        for part in value.split(',') {
            let (first, last) = part.split_once('-').unwrap_or((part, part));
            let first: u8 = first.parse().map_err(|_| SipError::InvalidPacket)?;
            let last: u8 = last.parse().map_err(|_| SipError::InvalidPacket)?;
            if first > last {
                return Err(SipError::InvalidPacket);
            }
            for event in first..=last.min(15) {
                self.events |= 1 << event;
            }
        }
        Ok(self)
    }

    pub fn parameters(self) -> String {
        let mut ranges = Vec::with_capacity(8);
        let mut event = 0u8;
        while event < 16 {
            if self.events & (1 << event) == 0 {
                event += 1;
                continue;
            }
            let first = event;
            while event < 15 && self.events & (1 << (event + 1)) != 0 {
                event += 1;
            }
            ranges.push(if first == event {
                first.to_string()
            } else {
                format!("{first}-{event}")
            });
            event += 1;
        }
        ranges.join(",")
    }
}
