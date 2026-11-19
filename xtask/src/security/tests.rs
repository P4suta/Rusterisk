use super::DenyTemporary;
use std::{
    ffi::OsStr,
    fs, io,
    path::{Path, PathBuf},
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

const KEYS: [&str; 3] = ["TMPDIR", "TMP", "TEMP"];
const REQUESTS: [&[&str]; 5] = [
    &[
        "deny",
        "--config",
        "owned-deny.toml",
        "fetch",
        "db",
        "index",
        "std-replacement",
    ],
    &["fetch", "--locked", "--manifest-path", "Cargo.toml"],
    &[
        "deny",
        "--manifest-path",
        "Cargo.toml",
        "--all-features",
        "--config",
        "owned-deny.toml",
        "--offline",
        "--locked",
        "check",
        "--hide-inclusion-graph",
    ],
    &[
        "fetch",
        "--locked",
        "--manifest-path",
        "xtask/fuzz/Cargo.toml",
    ],
    &[
        "deny",
        "--manifest-path",
        "xtask/fuzz/Cargo.toml",
        "--all-features",
        "--config",
        "owned-deny.toml",
        "--offline",
        "--locked",
        "check",
        "--hide-inclusion-graph",
    ],
];

fn require(valid: bool, reason: &'static str) -> io::Result<()> {
    if valid {
        Ok(())
    } else {
        Err(io::Error::other(reason))
    }
}

fn using_root<T>(test: impl FnOnce(&Path) -> io::Result<T>) -> io::Result<T> {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(io::Error::other)?
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "rusterisk-security-temporary-{}-{stamp}",
        std::process::id()
    ));
    fs::create_dir(&root)?;
    let operation = test(&root);
    let cleanup = fs::remove_dir_all(&root);
    match (operation, cleanup) {
        (operation, Ok(())) => operation,
        (Ok(_), Err(cleanup)) => Err(cleanup),
        (Err(operation), Err(cleanup)) => Err(io::Error::other(format!(
            "{operation}; security fixture cleanup failed: {cleanup}"
        ))),
    }
}

fn temporary_path(command: &Command) -> io::Result<PathBuf> {
    let environment = |name: &str| {
        command
            .get_envs()
            .find_map(|(key, value)| (key == name).then_some(value))
    };
    let path = environment("TMPDIR")
        .flatten()
        .ok_or_else(|| io::Error::other("Deny command inherited its temporary directory"))?;
    for key in KEYS {
        require(
            environment(key) == Some(Some(path)),
            "Deny commands disagree on their temporary environment",
        )?;
    }
    let path = Path::new(path);
    require(
        path.is_absolute() && path.is_dir(),
        "Deny command has no existing absolute temporary directory",
    )?;
    fs::canonicalize(path)
}

#[test]
fn deny_commands_share_one_owned_temporary_directory_until_cleanup() -> io::Result<()> {
    let inherited = KEYS.map(std::env::var_os);
    let temporary = using_root(|root| {
        let context = DenyTemporary::create(root)?;
        let expected = fs::canonicalize(root.join("temporary"))?;
        require(
            fs::read_dir(&expected)?.next().transpose()?.is_none(),
            "Deny execution adopted a populated temporary directory",
        )?;
        let repository = expected.join("cargo-deny/std-replacement-data");
        fs::create_dir_all(&repository)?;
        let marker = repository.join("owned-marker");
        fs::write(&marker, b"owned std replacement snapshot")?;
        for arguments in REQUESTS {
            let command = context.command(arguments);
            require(
                command.get_program() == OsStr::new("cargo")
                    && command
                        .get_args()
                        .eq(arguments.iter().copied().map(OsStr::new)),
                "Temporary isolation changed the maintained deny command",
            )?;
            require(
                temporary_path(&command)? == expected,
                "Fetch and offline checks selected different temporary directories",
            )?;
            require(
                fs::read(&marker)? == b"owned std replacement snapshot",
                "Deny command construction discarded its fetched snapshot",
            )?;
        }
        require(
            KEYS.map(std::env::var_os) == inherited,
            "Deny command preparation changed the parent environment",
        )?;
        Ok(expected)
    })?;
    require(
        !temporary.exists(),
        "Completed security execution retained its temporary directory",
    )
}

#[test]
fn deny_temporary_creation_refuses_an_existing_directory_without_changing_it() -> io::Result<()> {
    using_root(|root| {
        let temporary = root.join("temporary");
        fs::create_dir(&temporary)?;
        let marker = temporary.join("foreign-marker");
        fs::write(&marker, b"preserve existing state")?;
        require(
            DenyTemporary::create(root).is_err(),
            "Deny execution reused a preexisting temporary directory",
        )?;
        require(
            fs::read(marker)? == b"preserve existing state",
            "Deny execution changed a rejected temporary directory",
        )
    })
}

#[test]
fn deny_temporary_creation_refuses_an_existing_file_without_changing_it() -> io::Result<()> {
    using_root(|root| {
        let temporary = root.join("temporary");
        fs::write(&temporary, b"preserve existing file")?;
        require(
            DenyTemporary::create(root).is_err(),
            "Deny execution replaced a preexisting temporary file",
        )?;
        require(
            fs::read(temporary)? == b"preserve existing file",
            "Deny execution changed a rejected temporary file",
        )
    })
}

#[test]
fn separate_security_executions_do_not_share_temporary_state() -> io::Result<()> {
    using_root(|root| {
        let first = root.join("first");
        let second = root.join("second");
        fs::create_dir(&first)?;
        fs::create_dir(&second)?;
        let first = DenyTemporary::create(&first)?;
        let second = DenyTemporary::create(&second)?;
        let first = temporary_path(&first.command(REQUESTS[0]))?;
        let second = temporary_path(&second.command(REQUESTS[0]))?;
        require(
            first != second,
            "Security executions share a temporary directory",
        )?;
        fs::write(first.join("first-execution"), b"first")?;
        require(
            !second.join("first-execution").exists(),
            "Security execution observed another execution's temporary state",
        )
    })
}
