use super::fixture::{self, SOURCE, VERSION};
use serde_json::json;
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs, io,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::{Command, Output},
};

const GH: &str = r#"#!/bin/sh
set -eu
case "$*" in
  *git/ref/tags/*) cat "$STATE/ref" ;;
  *git/tags/*) cat "$STATE/tag" ;;
  *immutable-releases*) echo '{"enabled":true}' ;;
  *'--method DELETE'*'/releases/assets/333')
    cp "$STATE/after-delete" "$STATE/assets"; echo cleanup >> "$STATE/writes" ;;
  *releases/assets/21*) cat "$STATE/manifest" ;;
  *releases/assets/20*) cat "$STATE/archive" ;;
  *'/releases/1/assets?'*) cat "$STATE/assets" ;;
  *'/releases?per_page='*) cat "$STATE/pages" ;;
  *'--method PATCH'*|*'-X PATCH'*) cp "$STATE/public" "$STATE/pages"; echo page >> "$STATE/writes"; echo '{}' ;;
  *'release create'*'--verify-tag'*'--draft'*) cp "$STATE/draft" "$STATE/pages"; echo draft >> "$STATE/writes" ;;
  *'release upload'*)
    if [ -e "$STATE/change-source" ]; then
      printf 'replacement' > "$STATE/local/${4##*/}"
      cp "$4" "$STATE/uploaded"
    fi
    echo asset >> "$STATE/writes"; cp "$STATE/complete" "$STATE/assets" ;;
  *'workflow run'*) echo dispatch >> "$STATE/writes" ;;
  *) echo "unexpected gh $*" >&2; exit 1 ;;
esac
"#;
const CURL: &str = r#"#!/bin/sh
set -eu
out=
upload=no
while [ "$#" -gt 0 ]; do
 case "$1" in
   --output) shift; out="$1" ;;
   --request|-X) shift; if [ "$1" = PUT ]; then upload=yes; fi ;;
 esac
 shift
done
if [ "$upload" = yes ]; then
 IFS= read -r auth
 test "$auth" = 'Authorization: ephemeral-fixture'
 echo registry >> "$STATE/writes"
 cp "$STATE/match" "$STATE/receipt"
 echo '{}' > "$out"
 printf '200'
elif [ -e "$STATE/receipt" ]; then
 cat "$STATE/receipt" > "$out"
 printf '200'
else
 echo '{"errors":[{"detail":"missing version"}]}' > "$out"
 printf '404'
fi
"#;
struct Provider {
    state: tempfile::TempDir,
    path: OsString,
    manifest: String,
}

impl Provider {
    fn new() -> io::Result<Self> {
        let state = tempfile::tempdir()?;
        let dir = state.path();
        let bin = dir.join("bin");
        fs::create_dir(&bin)?;
        for (name, script) in [("gh", GH), ("curl", CURL)] {
            let path = bin.join(name);
            fs::write(&path, script)?;
            fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
        }
        let inherited = std::env::var_os("PATH").unwrap_or_default();
        let paths = std::iter::once(bin).chain(std::env::split_paths(&inherited));
        let path = std::env::join_paths(paths).map_err(io::Error::other)?;
        let files = fixture::files(SOURCE)?;
        let local = dir.join("local");
        fs::create_dir(&local)?;
        fixture::write(&local, &files)?;
        let flat: BTreeMap<_, _> = files
            .iter()
            .map(|(name, bytes)| {
                (
                    name.rsplit('/').next().unwrap_or(name),
                    fixture::digest(bytes),
                )
            })
            .collect();
        let manifest: String = flat
            .iter()
            .map(|(name, hash)| format!("{hash}  {name}\n"))
            .collect();
        let manifest_sum = fixture::digest(manifest.as_bytes());
        fs::write(dir.join("manifest"), manifest)?;
        let mut assets = Vec::new();
        for (name, sum) in &flat {
            let id = if name.ends_with(".crate") {
                20
            } else {
                100 + assets.len()
            };
            assets.push(
                json!({"id":id, "name":name, "state":"uploaded","digest":format!("sha256:{sum}")}),
            );
        }
        assets.push(json!({"id":21,"name":"SHA256SUMS","state":"uploaded","digest":format!("sha256:{manifest_sum}")}));
        fs::write(dir.join("assets"), serde_json::to_vec(&[assets])?)?;
        fs::copy(dir.join("assets"), dir.join("complete"))?;
        let mut page = json!({"id":1,"tag_name":"v0.2.0","draft":true,"immutable":false,"prerelease":false,"body":"Release notes","name":"Rusterisk 0.2.0"});
        fs::write(dir.join("pages"), serde_json::to_vec(&[[&page]])?)?;
        fs::copy(dir.join("pages"), dir.join("draft"))?;
        page["draft"] = false.into();
        page["immutable"] = true.into();
        fs::write(dir.join("public"), serde_json::to_vec(&[[page]])?)?;
        fs::write(
            dir.join("ref"),
            serde_json::to_vec(&json!({"object":{"type":"tag","sha":SOURCE}}))?,
        )?;
        fs::write(
            dir.join("tag"),
            serde_json::to_vec(
                &json!({"tag":"v0.2.0","object":{"type":"commit","sha":SOURCE},"verification":{"verified":true}}),
            )?,
        )?;
        let archive = &files["rusterisk-cli-0.2.0.crate"];
        fs::write(dir.join("archive"), archive)?;
        let receipt = json!({"version":{"crate":"rusterisk-cli","num":VERSION,"checksum":fixture::digest(archive),"yanked":false,"trustpub_data":{"provider":"github","repository":"P4suta/Rusterisk"}}});
        fs::write(dir.join("match"), serde_json::to_vec(&receipt)?)?;
        fs::write(dir.join("writes"), "")?;
        Ok(Self {
            state,
            path,
            manifest: manifest_sum,
        })
    }

    fn file(&self, name: &str) -> PathBuf {
        self.state.path().join(name)
    }

    fn command(&self, operation: &str, input: &Path) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_xtask"));
        command
            .args(["release-publication", operation, VERSION, SOURCE])
            .arg(input)
            .env("STATE", self.state.path())
            .env("PATH", &self.path)
            .env("CRATES_IO_PUBLISH_TOKEN", "ephemeral-fixture");
        command
    }

    fn run(&self, operation: &str, input: &Path) -> io::Result<Output> {
        self.command(operation, input).output()
    }

    fn succeeds(&self, operation: &str, input: &Path) -> io::Result<()> {
        let output = self.run(operation, input)?;
        assert!(
            output.status.success(),
            "{operation}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(())
    }
}

#[test]
fn publishing_resumes_from_the_registry_and_the_immutable_release() -> io::Result<()> {
    let provider = Provider::new()?;
    let sum = Path::new(&provider.manifest);
    provider.succeeds("preflight", sum)?;
    assert!(!provider.run("publish-page", sum)?.status.success());
    assert_eq!(fs::read_to_string(provider.file("writes"))?, "");
    let exported = provider.file("exported.crate");
    let output = provider
        .command("export-cli", sum)
        .arg(&exported)
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let archive = fs::read(provider.file("archive"))?;
    assert_eq!(fs::read(&exported)?, archive);
    let digest = fixture::digest(&archive);
    let upload = || {
        provider
            .command("publish-archive", Path::new(&digest))
            .arg(&exported)
            .output()
    };
    fs::write(&exported, "different bytes")?;
    assert!(!upload()?.status.success());
    fs::write(&exported, archive)?;
    assert!(upload()?.status.success());
    assert!(upload()?.status.success());
    provider.succeeds("publish-page", sum)?;
    provider.succeeds("publish-page", sum)?;
    assert_eq!(
        fs::read_to_string(provider.file("writes"))?,
        "registry\npage\n"
    );
    Ok(())
}

#[test]
fn only_empty_expected_starter_assets_on_a_draft_can_be_recovered() -> io::Result<()> {
    let provider = Provider::new()?;
    let mut assets: Vec<Vec<serde_json::Value>> =
        serde_json::from_slice(&fs::read(provider.file("complete"))?)?;
    let name = "rusterisk-0.2.0-aarch64-apple-darwin.tar.gz";
    assets[0].retain(|asset| asset["name"] != name);
    fs::write(provider.file("after-delete"), serde_json::to_vec(&assets)?)?;
    let starter = json!({"id":333,"name":name,"state":"starter","size":0,"digest":null});
    for (page, change) in [
        ("draft", json!({"size":1})),
        ("draft", json!({"size":null})),
        ("draft", json!({"name":"unrelated.zip"})),
        ("draft", json!({"state":"uploaded"})),
        ("public", json!({})),
    ] {
        let mut invalid = starter.clone();
        if let Some(fields) = change.as_object() {
            for (key, value) in fields {
                invalid[key] = value.clone();
            }
        }
        let mut inventory = assets.clone();
        inventory[0].push(invalid);
        fs::write(provider.file("assets"), serde_json::to_vec(&inventory)?)?;
        fs::copy(provider.file(page), provider.file("pages"))?;
        assert!(!provider
            .run("stage", &provider.file("local"))?
            .status
            .success());
        assert_eq!(fs::read_to_string(provider.file("writes"))?, "");
    }
    assets[0].push(starter);
    fs::write(provider.file("assets"), serde_json::to_vec(&assets)?)?;
    fs::copy(provider.file("draft"), provider.file("pages"))?;
    provider.succeeds("stage", &provider.file("local"))?;
    assert_eq!(
        fs::read_to_string(provider.file("writes"))?,
        "cleanup\nasset\n"
    );
    Ok(())
}

#[test]
fn staging_uploads_the_verified_bytes_after_the_original_file_changes() -> io::Result<()> {
    let provider = Provider::new()?;
    let name = "rusterisk-0.2.0-aarch64-apple-darwin.tar.gz";
    let original = fs::read(provider.file("local").join(name))?;
    let mut assets: Vec<Vec<serde_json::Value>> =
        serde_json::from_slice(&fs::read(provider.file("assets"))?)?;
    assets[0].retain(|asset| asset["name"] != name);
    fs::write(provider.file("assets"), serde_json::to_vec(&assets)?)?;
    fs::write(provider.file("change-source"), "")?;
    provider.succeeds("stage", &provider.file("local"))?;
    assert_eq!(fs::read(provider.file("local").join(name))?, b"replacement");
    assert_eq!(fs::read(provider.file("uploaded"))?, original);
    Ok(())
}

#[test]
fn staging_resumes_only_missing_assets_and_dispatches_the_full_release() -> io::Result<()> {
    let provider = Provider::new()?;
    let mut pages: Vec<Vec<serde_json::Value>> =
        serde_json::from_slice(&fs::read(provider.file("assets"))?)?;
    pages[0].pop();
    fs::write(provider.file("assets"), serde_json::to_vec(&pages)?)?;
    assert!(!provider
        .run("preflight", Path::new(&provider.manifest))?
        .status
        .success());
    provider.succeeds("submit", &provider.file("local"))?;
    provider.succeeds("stage", &provider.file("local"))?;
    assert_eq!(
        fs::read_to_string(provider.file("writes"))?,
        "asset\ndispatch\n"
    );
    Ok(())
}

#[test]
fn a_published_page_without_a_registry_receipt_cannot_resume() -> io::Result<()> {
    let provider = Provider::new()?;
    fs::copy(provider.file("public"), provider.file("pages"))?;
    assert!(!provider
        .run("submit", &provider.file("local"))?
        .status
        .success());
    assert!(!provider
        .command("export-cli", Path::new(&provider.manifest))
        .arg(provider.file("exported"))
        .output()?
        .status
        .success());
    assert_eq!(fs::read_to_string(provider.file("writes"))?, "");
    Ok(())
}

#[test]
fn the_first_staging_operation_creates_a_draft_with_notes_and_attaches_every_asset(
) -> io::Result<()> {
    let provider = Provider::new()?;
    fs::write(provider.file("pages"), "[[]]")?;
    fs::write(provider.file("assets"), "[[]]")?;
    provider.succeeds("stage", &provider.file("local"))?;
    let events = fs::read_to_string(provider.file("writes"))?;
    assert_eq!(events.lines().next(), Some("draft"));
    assert_eq!(events.lines().filter(|line| *line == "asset").count(), 21);
    assert!(!events.contains("page") && !events.contains("dispatch"));
    Ok(())
}
