use std::io::{self, Write};

pub(super) fn password_input(bytes: &[u8]) -> io::Result<io::PipeReader> {
    let (reader, mut writer) = io::pipe()?;
    writer.write_all(bytes)?;
    Ok(reader)
}
