use sha2::{Digest, Sha256};
use std::{
    io::{Read, Write},
    path::Path,
};

#[derive(Debug)]
pub enum BrowserError {
    Io(std::io::Error),
    Process(crate::process::ProcessError),
    Package,
    Declaration,
    Cleanup {
        operation: String,
        cleanup: std::io::Error,
    },
}

impl std::fmt::Display for BrowserError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => error.fmt(formatter),
            Self::Process(error) => error.fmt(formatter),
            Self::Package => formatter.write_str(
                "browser signaling package must be the locked sip.js 0.21.2 installation",
            ),
            Self::Declaration => formatter.write_str(
                "browser signaling declarations differ from the bound source or exact correction",
            ),
            Self::Cleanup { operation, cleanup } => write!(
                formatter,
                "{operation}; declaration cleanup failed: {cleanup}"
            ),
        }
    }
}

impl std::error::Error for BrowserError {}
impl From<std::io::Error> for BrowserError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl From<crate::process::ProcessError> for BrowserError {
    fn from(error: crate::process::ProcessError) -> Self {
        Self::Process(error)
    }
}

pub fn check_with(
    root: &Path,
    mut execute: impl FnMut(
        &mut std::process::Command,
        std::time::Duration,
    ) -> Result<(), crate::process::ProcessError>,
) -> Result<(), BrowserError> {
    let directory = root.join("examples/browser");
    let npm = if cfg!(windows) { "npm.cmd" } else { "npm" };
    let mut command = std::process::Command::new(npm);
    command
        .current_dir(&directory)
        .args(["ci", "--ignore-scripts", "--no-audit", "--no-fund"]);
    println!("browser check: installing locked dependencies without scripts");
    execute(&mut command, std::time::Duration::from_secs(120))?;
    prepare_types(root)?;
    for arguments in [
        ["run", "check"].as_slice(),
        ["test"].as_slice(),
        ["run", "build"].as_slice(),
    ] {
        println!("browser check: npm {}", arguments.join(" "));
        execute(
            std::process::Command::new(npm)
                .current_dir(&directory)
                .args(arguments),
            std::time::Duration::from_secs(120),
        )?;
    }
    Ok(())
}

pub fn check(root: &Path) -> Result<(), BrowserError> {
    check_with(root, crate::process::run)
}

#[derive(Clone, Copy)]
pub enum Declaration {
    Incoming,
    Outgoing,
    Invite,
    Subscribe,
}

impl Declaration {
    pub const ALL: [Self; 4] = [
        Self::Incoming,
        Self::Outgoing,
        Self::Invite,
        Self::Subscribe,
    ];
    pub const fn path(self) -> &'static str {
        match self {
            Self::Incoming => "lib/core/messages/incoming-request.d.ts",
            Self::Outgoing => "lib/core/messages/outgoing-request.d.ts",
            Self::Invite => "lib/core/messages/methods/invite.d.ts",
            Self::Subscribe => "lib/core/messages/methods/subscribe.d.ts",
        }
    }
    pub const fn original(self) -> &'static str {
        match self {
            Self::Incoming => "    delegate?: IncomingRequestDelegate;\n",
            Self::Outgoing => "    delegate?: OutgoingRequestDelegate;\n",
            Self::Invite => "    delegate?: OutgoingInviteRequestDelegate;\n",
            Self::Subscribe => "    delegate?: OutgoingSubscribeRequestDelegate;\n",
        }
    }
    pub const fn patched(self) -> &'static str {
        match self {
            Self::Incoming => "    delegate?: IncomingRequestDelegate | undefined;\n",
            Self::Outgoing => "    delegate?: OutgoingRequestDelegate | undefined;\n",
            Self::Invite => "    delegate?: OutgoingInviteRequestDelegate | undefined;\n",
            Self::Subscribe => "    delegate?: OutgoingSubscribeRequestDelegate | undefined;\n",
        }
    }
    const fn digest(self) -> [u8; 32] {
        match self {
            Self::Invite => [
                0xf5, 0x30, 0x56, 0xbf, 0x7a, 0x51, 0x87, 0xfb, 0x83, 0xce, 0x63, 0x4c, 0xec, 0xa6,
                0x07, 0x54, 0x8e, 0x41, 0xd8, 0xe9, 0xef, 0x50, 0x0b, 0x41, 0x25, 0xa0, 0x1e, 0xae,
                0x89, 0x78, 0x33, 0x64,
            ],
            Self::Subscribe => [
                0x78, 0xbf, 0x33, 0x77, 0x03, 0x48, 0xd5, 0x44, 0xfb, 0x50, 0xf4, 0xae, 0xdc, 0xd0,
                0x4f, 0x53, 0x46, 0xb0, 0x97, 0x31, 0x95, 0xef, 0x33, 0xa3, 0x72, 0xe6, 0x9e, 0x37,
                0xb8, 0xb1, 0xc5, 0xed,
            ],
            Self::Incoming => [
                0x7d, 0x54, 0x2e, 0x50, 0xcc, 0xc3, 0x7c, 0x32, 0xde, 0xdb, 0xa2, 0xde, 0x94, 0x84,
                0xca, 0x1d, 0xee, 0xa6, 0x63, 0x39, 0x52, 0x98, 0x30, 0xf6, 0x82, 0x8c, 0xa7, 0xf5,
                0xf1, 0x49, 0x0c, 0xe8,
            ],
            Self::Outgoing => [
                0xe5, 0x24, 0x3f, 0x0d, 0xe6, 0x70, 0xf8, 0xed, 0xd1, 0xac, 0x83, 0x79, 0xb9, 0xe2,
                0xbe, 0xe1, 0x10, 0x12, 0xce, 0x90, 0x8a, 0x9f, 0xfd, 0x1d, 0xc2, 0x9a, 0xa5, 0xbc,
                0xcb, 0x17, 0x44, 0xd2,
            ],
        }
    }
}

pub fn patch_declaration(
    declaration: Declaration,
    version: &str,
    bytes: &[u8],
) -> Result<Vec<u8>, BrowserError> {
    if version != "0.21.2" || bytes.len() > 16384 {
        return Err(BrowserError::Declaration);
    }
    let source = std::str::from_utf8(bytes).map_err(|_| BrowserError::Declaration)?;
    let original = match (
        source.matches(declaration.original()).count(),
        source.matches(declaration.patched()).count(),
    ) {
        (1, 0) => source.to_owned(),
        (0, 1) => source.replacen(declaration.patched(), declaration.original(), 1),
        _ => return Err(BrowserError::Declaration),
    };
    let digest: [u8; 32] = Sha256::digest(original.as_bytes()).into();
    if digest != declaration.digest() {
        return Err(BrowserError::Declaration);
    }
    let corrected = original.replacen(declaration.original(), declaration.patched(), 1);
    if corrected.matches(declaration.original()).count() != 0
        || corrected.matches(declaration.patched()).count() != 1
    {
        return Err(BrowserError::Declaration);
    }
    Ok(corrected.into_bytes())
}

fn read(path: &Path) -> Result<Vec<u8>, BrowserError> {
    let kind = std::fs::symlink_metadata(path)?.file_type();
    if !kind.is_file() || kind.is_symlink() {
        return Err(BrowserError::Package);
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(16385)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 16384 {
        return Err(BrowserError::Package);
    }
    Ok(bytes)
}

fn replace(path: &Path, bytes: &[u8]) -> Result<(), BrowserError> {
    let temporary = path.with_extension(format!("rusterisk-types-{}", std::process::id()));
    let mut file = std::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)?;
    let operation = (|| {
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, path)
    })();
    match operation {
        Ok(()) => Ok(()),
        Err(operation) => match std::fs::remove_file(temporary) {
            Ok(()) => Err(BrowserError::Io(operation)),
            Err(cleanup) => Err(BrowserError::Cleanup {
                operation: operation.to_string(),
                cleanup,
            }),
        },
    }
}

pub fn prepare_types(root: &Path) -> Result<(), BrowserError> {
    let mut package = root.join("examples/browser");
    for component in [
        "node_modules",
        "sip.js",
        "lib",
        "core",
        "messages",
        "methods",
    ] {
        package.push(component);
        let kind = std::fs::symlink_metadata(&package)?.file_type();
        if !kind.is_dir() || kind.is_symlink() {
            return Err(BrowserError::Package);
        }
    }
    let package = root.join("examples/browser/node_modules/sip.js");
    let metadata: serde_json::Value = serde_json::from_slice(&read(&package.join("package.json"))?)
        .map_err(|_| BrowserError::Package)?;
    if metadata.get("name").and_then(serde_json::Value::as_str) != Some("sip.js")
        || metadata.get("version").and_then(serde_json::Value::as_str) != Some("0.21.2")
    {
        return Err(BrowserError::Package);
    }
    let mut changes = Vec::with_capacity(4);
    for declaration in Declaration::ALL {
        let path = package.join(declaration.path());
        let source = read(&path)?;
        let patched = patch_declaration(declaration, "0.21.2", &source)?;
        if source != patched {
            changes.push((path, patched));
        }
    }
    for (path, bytes) in changes {
        replace(&path, &bytes)?;
    }
    println!("browser types: 4 bound signaling declarations checked");
    Ok(())
}
