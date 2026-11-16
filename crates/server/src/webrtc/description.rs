use super::{checked_description, negotiation_error};
use rusterisk_media::MediaError;
use rusterisk_sip::{DtmfFormat, WebRtcDescription};

pub(super) fn for_transport(description: &WebRtcDescription) -> Result<String, MediaError> {
    let source = description.as_str();
    let telephones = DtmfFormat::parse_all(source).map_err(negotiation_error)?;
    let security = ["a=ice-ufrag:", "a=ice-pwd:", "a=fingerprint:", "a=setup:"];
    let mut effective = String::with_capacity(source.len());
    let mut in_media = false;
    for line in source.lines() {
        if line.starts_with("m=") {
            in_media = true;
        }
        if !in_media && security.iter().any(|prefix| line.starts_with(*prefix)) {
            continue;
        }
        if let Some(attribute) = line.strip_prefix("a=fmtp:") {
            let (payload, _parameters) =
                attribute.split_once(' ').ok_or(MediaError::InvalidPacket)?;
            let payload = payload.parse::<u8>().map_err(negotiation_error)?;
            if telephones
                .iter()
                .any(|format| format.payload_type() == payload)
            {
                effective.push_str(&format!("a=fmtp:{payload} 0-15\r\n"));
                continue;
            }
        }
        effective.push_str(line);
        effective.push_str("\r\n");
    }
    for prefix in security {
        if source
            .lines()
            .skip_while(|line| !line.starts_with("m="))
            .any(|line| line.starts_with(prefix))
        {
            continue;
        }
        for line in source
            .lines()
            .take_while(|line| !line.starts_with("m="))
            .filter(|line| line.starts_with(prefix))
        {
            effective.push_str(line);
            effective.push_str("\r\n");
        }
    }
    Ok(effective)
}

pub(super) fn telephone_answer(
    source: &str,
    offer: &WebRtcDescription,
) -> Result<String, MediaError> {
    let offered = DtmfFormat::parse_all(offer.as_str()).map_err(negotiation_error)?;
    let local = DtmfFormat::parse_all(source).map_err(negotiation_error)?;
    let mut accepted = Vec::with_capacity(16);
    for local in &local {
        if let Some(peer) = offered
            .iter()
            .find(|peer| peer.payload_type() == local.payload_type() && peer.rate() == local.rate())
        {
            if local.events() & peer.events() != peer.events() {
                return Err(MediaError::Unsupported);
            }
            accepted.push(*peer);
        }
    }
    let mut output = String::with_capacity(source.len());
    let mut parameters = Vec::with_capacity(16);
    for line in source.lines() {
        if let Some(media) = line.strip_prefix("m=") {
            let fields: Vec<&str> = media.split_whitespace().take(20).collect();
            let ["audio", port, profile, payloads @ ..] = fields.as_slice() else {
                return Err(MediaError::Unsupported);
            };
            output.push_str(&format!("m=audio {port} {profile}"));
            for payload in payloads {
                let number = payload.parse::<u8>().map_err(negotiation_error)?;
                if local.iter().any(|format| format.payload_type() == number)
                    && !accepted
                        .iter()
                        .any(|format| format.payload_type() == number)
                {
                    continue;
                }
                output.push(' ');
                output.push_str(payload);
            }
            output.push_str("\r\n");
            continue;
        }
        if let Some(attribute) = line
            .strip_prefix("a=rtpmap:")
            .or_else(|| line.strip_prefix("a=fmtp:"))
        {
            let (payload, _value) = attribute.split_once(' ').ok_or(MediaError::InvalidPacket)?;
            let payload = payload.parse::<u8>().map_err(negotiation_error)?;
            if local.iter().any(|format| format.payload_type() == payload) {
                let Some(format) = accepted
                    .iter()
                    .find(|format| format.payload_type() == payload)
                else {
                    continue;
                };
                if line.starts_with("a=fmtp:") {
                    output.push_str(&format!("a=fmtp:{payload} {}\r\n", format.parameters()));
                    parameters.push(payload);
                    continue;
                }
            }
        }
        output.push_str(line);
        output.push_str("\r\n");
    }
    for format in accepted {
        if !parameters.contains(&format.payload_type()) {
            output.push_str(&format!(
                "a=fmtp:{} {}\r\n",
                format.payload_type(),
                format.parameters()
            ));
        }
    }
    checked_description(&output)?;
    Ok(output)
}
