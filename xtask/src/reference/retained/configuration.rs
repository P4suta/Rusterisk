use super::{files, ReferenceNativeContext};
use crate::{
    reference::{
        commands::{combine, read, write},
        copy_configuration, ReferenceRunner,
    },
    server_distribution::ServerRuntimeConfig,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs, io,
    net::Ipv4Addr,
    num::NonZeroU16,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};
use syn::parse::Parser;

const SOUNDS: [&str; 4] = [
    "en/rusterisk-position.wav",
    "en/digits/rusterisk-one.wav",
    "ja/digits/rusterisk-one.wav",
    "en/rusterisk-welcome.wav",
];

#[derive(Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(in crate::reference) struct CaseRuntime {
    websocket_port: NonZeroU16,
    callback: Ipv4Addr,
}

fn invalid(reason: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, reason)
}

fn format_template(method: &syn::ImplItemFn) -> io::Result<String> {
    let [syn::Stmt::Expr(syn::Expr::Macro(expression), None)] = method.block.stmts.as_slice()
    else {
        return Err(invalid("reference WebSocket generator shape differs"));
    };
    if !expression.mac.path.is_ident("format") {
        return Err(invalid(
            "reference WebSocket generator is not a format expression",
        ));
    }
    let arguments = syn::punctuated::Punctuated::<syn::Expr, syn::Token![,]>::parse_terminated
        .parse2(expression.mac.tokens.clone())
        .map_err(io::Error::other)?;
    let Some(syn::Expr::Lit(literal)) = arguments.first() else {
        return Err(invalid(
            "reference WebSocket generator has no literal template",
        ));
    };
    let syn::Lit::Str(literal) = &literal.lit else {
        return Err(invalid(
            "reference WebSocket generator template is not a string",
        ));
    };
    let port = matches!(arguments.last(), Some(syn::Expr::Field(field))
        if matches!(&field.member, syn::Member::Named(member) if member == "port")
            && matches!(field.base.as_ref(), syn::Expr::Path(path) if path.path.is_ident("self")));
    let template = literal.value();
    if arguments.len() != 2
        || !port
        || template.matches("{host}").count() != 1
        || template.matches("{}").count() != 1
    {
        return Err(invalid("reference WebSocket generator arguments differ"));
    }
    Ok(template.replace("{host}", "host.docker.internal"))
}

fn websocket_template(captured: &Path) -> io::Result<String> {
    let source = read(
        &captured.join("crates/client/tests/reference/external_websocket_clients.rs"),
        65_536,
    )?;
    let source = syn::parse_file(std::str::from_utf8(&source).map_err(io::Error::other)?)
        .map_err(io::Error::other)?;
    let mut selected = None;
    for item in source.items {
        let syn::Item::Impl(implementation) = item else {
            continue;
        };
        if !matches!(implementation.self_ty.as_ref(), syn::Type::Path(path) if path.path.is_ident("Fixture"))
        {
            continue;
        }
        for item in implementation.items {
            let syn::ImplItem::Fn(method) = item else {
                continue;
            };
            if method.sig.ident == "configuration" {
                if selected.is_some() {
                    return Err(invalid("reference WebSocket generator is repeated"));
                }
                selected = Some(format_template(&method)?);
            }
        }
    }
    selected.ok_or_else(|| invalid("reference WebSocket generator is missing"))
}

impl CaseRuntime {
    pub(in crate::reference) fn capture(
        captured: &Path,
        configuration: &Path,
        callback: Ipv4Addr,
    ) -> io::Result<Self> {
        if callback.is_unspecified()
            || callback.is_multicast()
            || callback.is_broadcast()
            || callback.is_loopback()
        {
            return Err(invalid("reference WebSocket callback address differs"));
        }
        let template = websocket_template(captured)?;
        let (prefix, suffix) = template
            .split_once("{}")
            .ok_or_else(|| invalid("reference WebSocket port placeholder is missing"))?;
        let actual = read(&configuration.join("websocket_client.conf"), 65_536)?;
        let actual = std::str::from_utf8(&actual).map_err(io::Error::other)?;
        let port = actual
            .strip_prefix(prefix)
            .and_then(|value| value.strip_suffix(suffix))
            .ok_or_else(|| {
                invalid("reference WebSocket output differs from its source template")
            })?;
        let websocket_port = port.parse::<NonZeroU16>().map_err(io::Error::other)?;
        if websocket_port.to_string() != port {
            return Err(invalid("reference WebSocket output port is not canonical"));
        }
        Ok(Self {
            websocket_port,
            callback,
        })
    }

    fn check(&self, captured: &Path, configuration: &Path, callback: Ipv4Addr) -> io::Result<()> {
        if Self::capture(captured, configuration, callback)? != *self {
            return Err(invalid(
                "reference WebSocket native configuration identity differs",
            ));
        }
        check_sounds(configuration)?;
        let data = configuration.join("native/data");
        if !read(&data.join(".rusterisk.lock"), 0)?.is_empty()
            || !fs::symlink_metadata(data.join("recordings"))?.is_dir()
            || fs::read_dir(data.join("recordings"))?.next().is_some()
        {
            return Err(invalid("reference generated runtime storage differs"));
        }
        Ok(())
    }
}

fn sound_paths(root: &Path, relative: &Path, remaining: &mut BTreeSet<PathBuf>) -> io::Result<()> {
    if !fs::symlink_metadata(root.join(relative))?.is_dir() {
        return Err(invalid(
            "reference sound output is not a physical directory",
        ));
    }
    for entry in fs::read_dir(root.join(relative))? {
        let entry = entry?;
        let path = relative.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_dir() {
            if !remaining.iter().any(|file| file.starts_with(&path)) {
                return Err(invalid("reference sound output directory differs"));
            }
            sound_paths(root, &path, remaining)?;
        } else if !kind.is_file() || !remaining.remove(&path) {
            return Err(invalid("reference sound output inventory differs"));
        }
    }
    Ok(())
}

fn check_sounds(configuration: &Path) -> io::Result<()> {
    let sounds = configuration.join("sounds");
    let native = configuration.join("native/data/sounds");
    let mut remaining = SOUNDS.map(PathBuf::from).into_iter().collect();
    sound_paths(&sounds, Path::new(""), &mut remaining)?;
    if !remaining.is_empty() {
        return Err(invalid("reference sound native outputs are incomplete"));
    }
    for name in SOUNDS {
        let bytes = read(&sounds.join(name), 8 * 1024 * 1024)?;
        if bytes.len() < 44
            || bytes.get(..4) != Some(b"RIFF".as_slice())
            || bytes.get(8..12) != Some(b"WAVE".as_slice())
        {
            return Err(invalid("reference sound output is not a bounded WAVE file"));
        }
    }
    files::compare(&sounds, &native)
}

impl ReferenceNativeContext {
    pub(super) fn check_configuration(&self, native: &Path, captured: &Path) -> io::Result<()> {
        let stamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_nanos();
        let root = fs::canonicalize(std::env::temp_dir())?.join(format!(
            "rusterisk-reference-configuration-{}-{stamp}",
            std::process::id()
        ));
        fs::create_dir(&root)?;
        let operation = (|| {
            let fixture = captured.join("tests/interop/asterisk");
            let cases = root.join("cases");
            ReferenceRunner::prepare_configuration(&fixture, &cases)?;
            let initial = native.join("reference-comparisons/initial-configuration");
            files::compare(&cases, &initial)?;
            let actual = native.join("case-configuration");
            files::compare_case(&initial, &actual)?;
            self.cases
                .run
                .runtime
                .as_ref()
                .ok_or_else(|| invalid("reference native runtime configuration is missing"))?
                .check(captured, &actual, self.cases.run.callback)?;
            let sipp = root.join("sipp");
            copy_configuration(&fixture, &sipp)?;
            fs::create_dir(sipp.join("sounds"))?;
            write(
                &sipp.join("sipp-registration.xml"),
                &read(
                    &captured.join("tests/interop/sipp/registration.xml"),
                    65_536,
                )?,
            )?;
            files::compare(&sipp, &native.join("sipp/asterisk"))?;
            let context = &self.sipp.run;
            let expected = ServerRuntimeConfig::for_registration_comparison(
                context.http,
                context.sip.port(),
                |path| read(&captured.join(path), 65_536),
            )?;
            let runtime = root.join("runtime");
            fs::create_dir(&runtime)?;
            for (path, bytes) in expected.files() {
                let path = runtime.join(path);
                fs::create_dir_all(path.parent().ok_or_else(|| {
                    io::Error::other("reference runtime configuration has no parent")
                })?)?;
                write(&path, bytes)?;
            }
            files::compare(&runtime.join("config"), &native.join("sipp/native/config"))
        })();
        combine(
            operation,
            fs::remove_dir_all(root),
            "reference configuration validation cleanup",
        )
    }
}
