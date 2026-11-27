use super::{
    candidate,
    github::{missing, verify_tag, Asset},
    hex, invalid, Release,
};
use serde::{de::DeserializeOwned, Deserialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{self, Write},
    path::Path,
    process::Command,
};

pub(super) const REPO: &str = "P4suta/Rusterisk";

fn gh(args: &[&str]) -> io::Result<Vec<u8>> {
    gh_with_token(args, None)
}

fn gh_with_token(args: &[&str], token: Option<std::ffi::OsString>) -> io::Result<Vec<u8>> {
    let mut command = Command::new("gh");
    if let Some(token) = token {
        command.env_remove("GITHUB_TOKEN").env("GH_TOKEN", token);
    }
    let result = command
        .args(args)
        .env_remove("CRATES_IO_PUBLISH_TOKEN")
        .env_remove("RELEASE_SETTINGS_TOKEN")
        .output()?;
    if !result.status.success() {
        return Err(io::Error::other(format!(
            "GitHub request failed: {}",
            String::from_utf8_lossy(&result.stderr).trim()
        )));
    }
    Ok(result.stdout)
}

fn api(path: &str) -> String {
    format!("repos/{REPO}/{path}")
}

fn get(path: &str) -> io::Result<serde_json::Value> {
    Ok(serde_json::from_slice(&gh(&["api", &api(path)])?)?)
}

fn require_immutable_releases() -> io::Result<()> {
    let response = gh_with_token(
        &["api", &api("immutable-releases")],
        std::env::var_os("RELEASE_SETTINGS_TOKEN"),
    )?;
    let settings: serde_json::Value = serde_json::from_slice(&response)?;
    if settings["enabled"] != true {
        return Err(invalid("GitHub immutable releases must be enabled"));
    }
    Ok(())
}

fn list<T: DeserializeOwned>(path: &str) -> io::Result<Vec<T>> {
    let pages: Vec<Vec<T>> =
        serde_json::from_slice(&gh(&["api", "--paginate", "--slurp", &api(path)])?)?;
    Ok(pages.into_iter().flatten().collect())
}

pub(super) fn signed_tag(release: &Release<'_>) -> io::Result<()> {
    let reference = get(&format!("git/ref/tags/v{}", release.version))?;
    let object = &reference["object"];
    let sha = object["sha"]
        .as_str()
        .filter(|s| hex(s, 40) && object["type"] == "tag")
        .ok_or_else(|| invalid("expected an annotated release tag"))?;
    verify_tag(release, &get(&format!("git/tags/{sha}"))?)
}

#[derive(Deserialize)]
pub(super) struct Page {
    pub id: u64,
    tag_name: String,
    pub draft: bool,
    immutable: bool,
    body: Option<String>,
    prerelease: bool,
}

fn page(release: &Release<'_>) -> io::Result<Option<Page>> {
    let tag = format!("v{}", release.version);
    let mut matches = list::<Page>("releases?per_page=100")?
        .into_iter()
        .filter(|p| p.tag_name == tag);
    let page = matches.next();
    if matches.next().is_some() {
        return Err(invalid("multiple releases match the signed tag"));
    }
    if let Some(page) = &page {
        if (!page.draft && !page.immutable)
            || page.body.as_ref().is_none_or(|s| s.trim().is_empty())
            || page.prerelease != release.version.contains('-')
        {
            return Err(invalid(
                "release notes, prerelease status or immutability differs from the candidate",
            ));
        }
    }
    Ok(page)
}

fn assets(page: &Page) -> io::Result<Vec<Asset>> {
    list(&format!("releases/{}/assets?per_page=100", page.id))
}

fn pending_uploads(page: &Page, expected: &BTreeMap<String, String>) -> io::Result<Vec<String>> {
    let (failed, uploaded): (Vec<_>, Vec<_>) = assets(page)?
        .into_iter()
        .partition(|asset| page.draft && asset.state == "starter" && asset.size == Some(0));
    let pending = missing(expected, &uploaded)?;
    let mut replaceable: BTreeSet<_> = pending.iter().collect();
    for asset in &failed {
        if !replaceable.remove(&asset.name) {
            return Err(invalid(format!("unexpected failed upload {}", asset.name)));
        }
    }
    for asset in failed {
        gh(&[
            "api",
            "--method",
            "DELETE",
            &api(&format!("releases/assets/{}", asset.id)),
        ])?;
    }
    Ok(pending)
}

fn download(assets: &[Asset], name: &str, checksum: &str, cap: usize) -> io::Result<Vec<u8>> {
    let asset = assets
        .iter()
        .find(|a| {
            a.name == name
                && a.state == "uploaded"
                && a.digest.as_deref().and_then(|v| v.strip_prefix("sha256:")) == Some(checksum)
        })
        .ok_or_else(|| invalid(format!("missing verified GitHub asset {name}")))?;
    if asset.size.is_some_and(|size| size > cap as u64) {
        return Err(invalid("GitHub asset exceeds its size limit"));
    }
    let bytes = gh(&[
        "api",
        "-H",
        "Accept: application/octet-stream",
        &api(&format!("releases/assets/{}", asset.id)),
    ])?;
    if bytes.len() > cap || candidate::sha256(&bytes) != checksum {
        return Err(invalid(format!("downloaded bytes differ for {name}")));
    }
    Ok(bytes)
}

pub(super) struct Snapshot {
    pub page: Page,
    pub archive: Vec<u8>,
    pub checksum: String,
    expected: BTreeMap<String, String>,
}

fn complete_inventory(expected: &BTreeMap<String, String>, assets: &[Asset]) -> io::Result<()> {
    if !missing(expected, assets)?.is_empty() {
        return Err(invalid("incomplete GitHub release assets"));
    }
    Ok(())
}

impl Snapshot {
    pub fn registry_present(&self, release: &Release<'_>) -> io::Result<bool> {
        let present = super::http::present(release, &self.checksum)?;
        if !present && !self.page.draft {
            return Err(invalid(
                "the published release is missing its registry receipt",
            ));
        }
        Ok(present)
    }

    pub fn load(release: &Release<'_>, manifest_hash: &str) -> io::Result<Self> {
        if !hex(manifest_hash, 64) {
            return Err(invalid("expected the SHA256SUMS digest"));
        }
        signed_tag(release)?;
        let page = page(release)?.ok_or_else(|| invalid("release draft is missing"))?;
        let assets = assets(&page)?;
        let manifest = download(&assets, "SHA256SUMS", manifest_hash, 16 * 1024)?;
        let mut expected =
            candidate::checksums(std::str::from_utf8(&manifest).map_err(io::Error::other)?)?;
        if expected.keys().ne(candidate::names(release)?.keys()) {
            return Err(invalid("incomplete release asset inventory"));
        }
        let checksum = expected[&release.crate_name()].clone();
        expected.insert("SHA256SUMS".into(), manifest_hash.into());
        complete_inventory(&expected, &assets)?;
        let archive = download(&assets, &release.crate_name(), &checksum, 16 * 1024 * 1024)?;
        candidate::check_crate(release, &archive)?;
        Ok(Self {
            page,
            archive,
            checksum,
            expected,
        })
    }
}

pub(super) fn stage(release: &Release<'_>, directory: &Path, submit: bool) -> io::Result<()> {
    let mut builder = tempfile::Builder::new();
    builder.prefix("rusterisk-upload-");
    #[cfg(unix)]
    builder.permissions(<fs::Permissions as std::os::unix::fs::PermissionsExt>::from_mode(0o700));
    let scratch = builder.tempdir()?;
    let local = candidate::Local::verify(release, directory, Some(scratch.path()))?;
    signed_tag(release)?;
    require_immutable_releases()?;
    let tag = format!("v{}", release.version);
    let page = match page(release)? {
        Some(page) => page,
        None => {
            let title = format!("Rusterisk {}", release.version);
            let mut args = Vec::from([
                "release",
                "create",
                &tag,
                "--repo",
                REPO,
                "--verify-tag",
                "--draft",
                "--target",
                release.source,
                "--title",
                &title,
                "--generate-notes",
            ]);
            if release.version.contains('-') {
                args.push("--prerelease");
            }
            let creation = gh(&args);
            match page(release)? {
                Some(page) => page,
                None => {
                    creation?;
                    return Err(invalid("GitHub did not retain the draft"));
                }
            }
        }
    };
    let manifest_hash = local.manifest_hash();
    let manifest_path = scratch.path().join("SHA256SUMS");
    fs::File::create_new(&manifest_path)?.write_all(local.manifest.as_bytes())?;
    let mut files = local.assets;
    files.insert("SHA256SUMS".into(), (manifest_path, manifest_hash.clone()));
    let expected: BTreeMap<_, _> = files
        .iter()
        .map(|(name, (_, sum))| (name.clone(), sum.clone()))
        .collect();
    for name in pending_uploads(&page, &expected)? {
        if !page.draft {
            return Err(invalid("cannot change a published release"));
        }
        let path = files[&name]
            .0
            .to_str()
            .ok_or_else(|| invalid("non-UTF-8 release asset path"))?;
        let result = gh(&["release", "upload", &tag, path, "--repo", REPO]);
        if let Err(error) = result {
            if missing(&expected, &assets(&page)?)?.contains(&name) {
                return Err(error);
            }
        }
    }
    let snapshot = Snapshot::load(release, &manifest_hash)?;
    if !snapshot.page.draft {
        snapshot.registry_present(release)?;
    }
    println!("Verified {tag} with SHA256SUMS {manifest_hash}");
    if submit && snapshot.page.draft {
        let out = gh(&[
            "workflow",
            "run",
            "publish-cli.yml",
            "--repo",
            REPO,
            "--ref",
            "main",
            "-f",
            "operation=publish-release",
            "-f",
            &format!("version={}", release.version),
            "-f",
            &format!("source={}", release.source),
            "-f",
            &format!("manifest={manifest_hash}"),
        ])?;
        print!("{}", String::from_utf8_lossy(&out));
    }
    Ok(())
}

pub(super) fn publish_page(
    release: &Release<'_>,
    manifest: &str,
    snapshot: &Snapshot,
) -> io::Result<()> {
    if !snapshot.page.draft {
        return Ok(());
    }
    signed_tag(release)?;
    let live = page(release)?.ok_or_else(|| invalid("release draft is missing"))?;
    if live.id != snapshot.page.id {
        return Err(invalid("the approved release draft was replaced"));
    }
    complete_inventory(&snapshot.expected, &assets(&live)?)?;
    if !live.draft {
        return Ok(());
    }
    require_immutable_releases()?;
    let update = gh(&[
        "api",
        "--method",
        "PATCH",
        &api(&format!("releases/{}", live.id)),
        "-F",
        "draft=false",
    ]);
    let current = Snapshot::load(release, manifest)?;
    if current.page.id != snapshot.page.id || current.page.draft {
        update?;
        return Err(invalid("GitHub did not publish the verified release"));
    }
    println!(
        "https://github.com/{REPO}/releases/tag/v{}",
        release.version
    );
    Ok(())
}
