use super::{invalid, Release};
use serde_json::{json, Value};
use std::io;

pub(super) fn verify_receipt(
    release: &Release<'_>,
    checksum: &str,
    body: &Value,
) -> io::Result<()> {
    let version = &body["version"];
    let publisher = &version["trustpub_data"];
    if version["crate"] != "rusterisk-cli"
        || version["num"] != release.version
        || version["checksum"] != checksum
        || version["yanked"] != false
        || publisher["provider"] != "github"
        || publisher["repository"] != "P4suta/Rusterisk"
    {
        return Err(invalid(
            "crates.io version, checksum or trusted publisher differs from the release",
        ));
    }
    Ok(())
}

pub(super) fn upload_body(
    release: &Release<'_>,
    readme: &str,
    archive: &[u8],
) -> io::Result<Vec<u8>> {
    let metadata = serde_json::to_vec(
        &json!({"name":"rusterisk-cli", "vers":release.version, "readme":readme, "readme_file":"README.md"}),
    )?;
    let mut body = Vec::new();
    for block in [metadata.as_slice(), archive] {
        body.extend_from_slice(
            &u32::try_from(block.len())
                .map_err(io::Error::other)?
                .to_le_bytes(),
        );
        body.extend_from_slice(block);
    }
    Ok(body)
}
