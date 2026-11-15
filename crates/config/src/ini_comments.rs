use std::borrow::Cow;

pub(crate) fn content(source: &str) -> Cow<'_, str> {
    let Some((before, after)) = source.split_once(';') else {
        return Cow::Borrowed(source);
    };
    let Some(prefix) = before.strip_suffix('\\') else {
        return Cow::Borrowed(before);
    };
    let mut output = String::with_capacity(source.len());
    output.push_str(prefix);
    output.push(';');
    for character in after.chars() {
        if character == ';' {
            let Some(length) = output.strip_suffix('\\').map(str::len) else {
                break;
            };
            output.truncate(length);
        }
        output.push(character);
    }
    Cow::Owned(output)
}
