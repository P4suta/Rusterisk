const MAX_EXPANDED: usize = 4096;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ExpansionError<E> {
    Syntax(&'static str),
    Capacity,
    Resolve(E),
}

impl<E: std::fmt::Display> std::fmt::Display for ExpansionError<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Syntax(reason) => write!(f, "variable expression: {reason}"),
            Self::Capacity => f.write_str("expanded value exceeds 4096 bytes"),
            Self::Resolve(error) => write!(f, "variable lookup: {error}"),
        }
    }
}

impl<E: std::error::Error> std::error::Error for ExpansionError<E> {}

fn append<E>(output: &mut String, value: &str) -> Result<(), ExpansionError<E>> {
    if output
        .len()
        .checked_add(value.len())
        .is_none_or(|length| length > MAX_EXPANDED)
    {
        return Err(ExpansionError::Capacity);
    }
    output.push_str(value);
    Ok(())
}

pub fn expand_variables<E>(
    source: &str,
    mut resolve: impl FnMut(&str) -> Result<String, E>,
) -> Result<String, ExpansionError<E>> {
    if source.len() > MAX_EXPANDED {
        return Err(ExpansionError::Capacity);
    }
    super::validate_expansion(source).map_err(ExpansionError::Syntax)?;
    let mut output = String::with_capacity(source.len());
    let mut remaining = source;
    while let Some((literal, variable)) = remaining.split_once("${") {
        append(&mut output, literal)?;
        let (name, rest) = variable
            .split_once('}')
            .ok_or(ExpansionError::Syntax("unclosed variable expansion"))?;
        let value = resolve(name).map_err(ExpansionError::Resolve)?;
        append(&mut output, &value)?;
        remaining = rest;
    }
    append(&mut output, remaining)?;
    Ok(output)
}
