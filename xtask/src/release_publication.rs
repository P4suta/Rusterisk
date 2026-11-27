mod candidate;
mod github;
mod http;
#[cfg(test)]
mod protocol_tests;
mod registry;
mod remote;

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
    if let [operation, version, source, digest, file] = args {
        let release = Release::new(version, source)?;
        if !hex(digest, 64) {
            return Err(invalid("expected an approved SHA-256 digest"));
        }
        return match operation.as_str() {
            "export-cli" => {
                let snapshot = remote::Snapshot::load(&release, digest)?;
                snapshot.registry_present(&release)?;
                use std::io::Write;
                std::fs::File::create_new(file)?.write_all(&snapshot.archive)
            }
            "publish-archive" => {
                remote::signed_tag(&release)?;
                let archive = candidate::small_file(Path::new(file), 16 * 1024 * 1024)?;
                http::publish(&release, digest, &archive)
            }
            _ => Err(invalid("unknown archive operation")),
        };
    }
    let [operation, version, source, input] = args else {
        return Err(invalid(
            "expected release-publication <operation> <version> <commit> <input>",
        ));
    };
    let release = Release::new(version, source)?;
    match operation.as_str() {
        "plan" => {
            let files = candidate::Local::verify(&release, Path::new(input), None)?;
            println!(
                "{} assets for {} at {} with SHA256SUMS {}",
                files.assets.len(),
                version,
                source,
                files.manifest_hash()
            );
            Ok(())
        }
        "stage" | "submit" => remote::stage(&release, Path::new(input), operation == "submit"),
        "preflight" | "publish-page" => {
            let snapshot = remote::Snapshot::load(&release, input)?;
            let present = snapshot.registry_present(&release)?;
            if operation == "publish-page" {
                if present {
                    return remote::publish_page(&release, input, &snapshot);
                }
                return Err(invalid(
                    "crates.io must confirm the approved release before GitHub publication",
                ));
            }
            Ok(())
        }
        _ => Err(invalid("unknown release publication operation")),
    }
}
