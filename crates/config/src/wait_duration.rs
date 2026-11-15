#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WaitDuration {
    milliseconds: u32,
    argument: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InvalidWaitDuration;

impl std::fmt::Display for InvalidWaitDuration {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .write_str("Wait requires a nonnegative duration of at most 2147483647 milliseconds")
    }
}
impl std::error::Error for InvalidWaitDuration {}

impl WaitDuration {
    pub fn milliseconds(&self) -> u32 {
        self.milliseconds
    }
    pub fn argument(&self) -> &str {
        &self.argument
    }

    pub fn parse(source: &str) -> Result<Self, InvalidWaitDuration> {
        if source.is_empty()
            || source.len() > 64
            || !source.is_ascii()
            || source
                .bytes()
                .any(|byte| byte.is_ascii_control() && byte != b'\t')
        {
            return Err(InvalidWaitDuration);
        }
        let trimmed = source.trim_matches([' ', '\t']);
        let lower = trimmed.to_ascii_lowercase();
        let (number, scale) = if lower.ends_with("ms") {
            (&trimmed[..trimmed.len() - 2], 1u128)
        } else if lower.ends_with('s') {
            (&trimmed[..trimmed.len() - 1], 1000)
        } else if lower.ends_with('m') {
            (&trimmed[..trimmed.len() - 1], 60000)
        } else if lower.ends_with('h') {
            (&trimmed[..trimmed.len() - 1], 3600000)
        } else {
            (trimmed, 1000)
        };
        let number = number.trim_end_matches([' ', '\t']);
        if number.is_empty() || number.len() > 30 {
            return Err(InvalidWaitDuration);
        }
        let number = number.strip_prefix('+').unwrap_or(number);
        let (mantissa, exponent) = match number.find(['e', 'E']) {
            Some(index) => (
                &number[..index],
                number[index + 1..]
                    .parse::<i32>()
                    .map_err(|_| InvalidWaitDuration)?,
            ),
            None => (number, 0),
        };
        let (whole, fraction) = mantissa.split_once('.').unwrap_or((mantissa, ""));
        if whole.is_empty() && fraction.is_empty() {
            return Err(InvalidWaitDuration);
        }
        let mut coefficient = 0u128;
        for byte in whole.bytes().chain(fraction.bytes()) {
            if !byte.is_ascii_digit() {
                return Err(InvalidWaitDuration);
            }
            coefficient = coefficient
                .checked_mul(10)
                .and_then(|value| value.checked_add(u128::from(byte - b'0')))
                .ok_or(InvalidWaitDuration)?;
        }
        let power = i64::from(exponent) - fraction.len() as i64;
        let scaled = coefficient.checked_mul(scale).ok_or(InvalidWaitDuration)?;
        let milliseconds = if scaled == 0 || power < -38 {
            0
        } else if power < 0 {
            scaled
                / 10u128
                    .checked_pow((-power) as u32)
                    .ok_or(InvalidWaitDuration)?
        } else {
            let power = u32::try_from(power).map_err(|_| InvalidWaitDuration)?;
            scaled
                .checked_mul(10u128.checked_pow(power).ok_or(InvalidWaitDuration)?)
                .ok_or(InvalidWaitDuration)?
        };
        if milliseconds > i32::MAX as u128 {
            return Err(InvalidWaitDuration);
        }
        Ok(Self {
            milliseconds: milliseconds as u32,
            argument: source.into(),
        })
    }
}
