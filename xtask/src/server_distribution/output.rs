use std::io::{self, Write};

pub(super) struct Bounded<W> {
    inner: W,
    remaining: u64,
}

impl<W> Bounded<W> {
    pub(super) fn new(inner: W) -> Self {
        Self {
            inner,
            remaining: 272 * 1024 * 1024,
        }
    }
}

impl<W: Write> Write for Bounded<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() as u64 > self.remaining {
            return Err(io::Error::other(
                "server archive exceeds its compressed size limit",
            ));
        }
        let written = self.inner.write(bytes)?;
        self.remaining -= written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::Bounded;
    use std::io::Write;

    #[test]
    fn output_limits_refuse_extra_bytes_before_writing_them() -> std::io::Result<()> {
        let mut bytes = Vec::new();
        let mut writer = Bounded {
            inner: &mut bytes,
            remaining: 8,
        };
        writer.write_all(b"first")?;
        assert!(writer.write_all(b"more").is_err());
        assert_eq!(bytes, b"first");
        Ok(())
    }
}
