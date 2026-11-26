mod candidate;

use std::{io, path::Path};

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

struct Release<'a> {
    version: &'a str,
    source: &'a str,
}

impl<'a> Release<'a> {
    fn new(version: &'a str, source: &'a str) -> io::Result<Self> {
        let parsed = semver::Version::parse(version).map_err(io::Error::other)?;
        if version.len() > 64 || !parsed.build.is_empty() || !hex(source, 40) {
            return Err(invalid(
                "expected a portable release version and full source commit",
            ));
        }
        Ok(Self { version, source })
    }

    fn crate_name(&self) -> String {
        format!("rusterisk-cli-{}.crate", self.version)
    }
}

pub(super) fn execute(args: &[String]) -> io::Result<()> {
    let [operation, version, source, input] = args else {
        return Err(invalid(
            "expected release-publication <operation> <version> <commit> <input>",
        ));
    };
    let release = Release::new(version, source)?;
    match operation.as_str() {
        "plan" => {
            let files = candidate::Local::verify(&release, Path::new(input))?;
            println!(
                "{} assets for {} at {} with SHA256SUMS {}",
                files.assets.len(),
                version,
                source,
                files.manifest_hash()
            );
            Ok(())
        }
        _ => Err(invalid("unknown release publication operation")),
    }
}
