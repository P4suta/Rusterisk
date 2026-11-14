use crate::{response, DialogReply, SipError, SipEvent};
use rsipstack::sip::Request;
use rusterisk_core::DtmfDigit;

enum InfoError {
    Unsupported,
    Invalid,
}

struct Digit {
    digit: DtmfDigit,
    duration_ms: u32,
}

fn symbol(value: &str) -> Result<DtmfDigit, InfoError> {
    if let Ok(code) = value.parse::<u8>() {
        return DtmfDigit::from_event(code).ok_or(InfoError::Invalid);
    }
    let mut symbols = value.chars();
    let symbol = symbols
        .next()
        .ok_or(InfoError::Invalid)?
        .to_ascii_uppercase();
    if symbols.next().is_some() {
        return Err(InfoError::Invalid);
    }
    DtmfDigit::from_symbol(symbol).ok_or(InfoError::Invalid)
}

fn parse(request: &Request) -> Result<Option<Digit>, InfoError> {
    if request.body.is_empty() {
        return Ok(None);
    }
    let content_type = request
        .headers
        .iter()
        .find(|header| header.name().eq_ignore_ascii_case("Content-Type"))
        .ok_or(InfoError::Unsupported)?
        .value();
    let content_type = content_type.split(';').next().unwrap_or("").trim();
    if !content_type.eq_ignore_ascii_case("application/dtmf-relay")
        && !content_type.eq_ignore_ascii_case("application/dtmf")
    {
        return Err(InfoError::Unsupported);
    }
    if request.body.len() > 1024 {
        return Err(InfoError::Invalid);
    }
    let body = std::str::from_utf8(&request.body).map_err(|_| InfoError::Invalid)?;
    if content_type.eq_ignore_ascii_case("application/dtmf") {
        return Ok(Some(Digit {
            digit: symbol(body.trim())?,
            duration_ms: 100,
        }));
    }
    let mut digit = None;
    let mut duration_ms = 100;
    for line in body.lines().filter(|line| !line.is_empty()) {
        let Some((name, value)) = line.split_once('=') else {
            continue;
        };
        if name.trim().eq_ignore_ascii_case("signal") {
            digit = Some(symbol(value.trim())?);
        } else if name.trim().eq_ignore_ascii_case("duration") {
            duration_ms = value.trim().parse().map_err(|_| InfoError::Invalid)?;
        }
    }
    Ok(Some(Digit {
        digit: digit.ok_or(InfoError::Invalid)?,
        duration_ms,
    }))
}

pub(crate) fn receive(
    request: &Request,
    id: &str,
    generation: u64,
) -> Result<DialogReply, SipError> {
    let parsed = parse(request);
    let (code, phrase) = match &parsed {
        Ok(_) => (200, "OK"),
        Err(InfoError::Unsupported) => (415, "Unsupported Media Type"),
        Err(InfoError::Invalid) => (500, "Server Internal Error"),
    };
    let mut events = Vec::new();
    if let Ok(Some(digit)) = parsed {
        events.push(SipEvent::DtmfReceived {
            id: id.into(),
            generation,
            digit: digit.digit,
            duration_ms: digit.duration_ms,
        });
    }
    Ok(DialogReply::Accepted {
        bytes: response(request, code, phrase, "", "")?,
        events,
    })
}
