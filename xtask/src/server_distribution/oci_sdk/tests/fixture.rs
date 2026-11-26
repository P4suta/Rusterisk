use rusterisk_test_support::Wait;
use serde::{Deserialize, Serialize};
use std::{
    io::{self, Read, Write},
    net::{SocketAddr, TcpListener, TcpStream, UdpSocket},
    path::{Path, PathBuf},
};

#[derive(Serialize, Deserialize)]
struct State {
    id: String,
    image: String,
    owner: String,
    manifest: Option<ImageDescriptor>,
}

#[derive(Serialize)]
struct ImageView {
    id: String,
    os: &'static str,
    architecture: &'static str,
    descriptor: Option<ImageDescriptor>,
}

#[derive(Serialize, Deserialize)]
struct ImageDescriptor {
    digest: String,
    media_type: String,
    size: u64,
}

pub(super) fn state(
    root: &Path,
    name: &str,
    owner: &str,
    image: &str,
    manifest: Option<&str>,
) -> io::Result<()> {
    use sha2::{Digest, Sha256};
    let id = Sha256::digest(format!("{name}:{owner}:{image}").as_bytes())
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    std::fs::write(
        root.join(format!("{name}.json")),
        serde_json::to_vec(&State {
            id,
            image: image.into(),
            owner: owner.into(),
            manifest: manifest.map(|digest| ImageDescriptor {
                digest: digest.into(),
                media_type: "application/vnd.oci.image.manifest.v1+json".into(),
                size: 1014,
            }),
        })
        .map_err(io::Error::other)?,
    )
}

fn named(root: &Path, identity: &str) -> io::Result<String> {
    if identity.starts_with("rusterisk-") {
        return Ok(identity.into());
    }
    for (count, entry) in std::fs::read_dir(root)?.enumerate() {
        let entry = entry?;
        if count >= 32 {
            return Err(io::Error::other("fixture state count exceeded"));
        }
        let path = entry.path();
        if path.extension().and_then(|part| part.to_str()) != Some("json") {
            continue;
        }
        let state: State =
            serde_json::from_slice(&std::fs::read(&path)?).map_err(io::Error::other)?;
        if state.id == identity {
            return path
                .file_stem()
                .and_then(|name| name.to_str())
                .map(str::to_owned)
                .ok_or_else(|| io::Error::other("fixture state name changed"));
        }
    }
    Err(io::Error::other(
        "fixture container identity no longer exists",
    ))
}

fn trace(root: &Path, entry: &str) -> io::Result<()> {
    let mut output = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(root.join("trace"))?;
    writeln!(output, "{entry}")
}

fn after(arguments: &[String], flag: &str) -> io::Result<String> {
    arguments
        .windows(2)
        .find(|pair| pair[0] == flag)
        .map(|pair| pair[1].clone())
        .ok_or_else(|| io::Error::other("fixture argument missing"))
}

fn env(name: &str) -> io::Result<String> {
    std::env::var(name).map_err(io::Error::other)
}

pub(super) fn request(address: SocketAddr, byte: u8) -> io::Result<()> {
    let mut stream = TcpStream::connect_timeout(&address, Wait::Responsive.duration())?;
    stream.set_read_timeout(Some(Wait::Responsive.duration()))?;
    stream.set_write_timeout(Some(Wait::Responsive.duration()))?;
    stream.write_all(&[byte])?;
    let mut response = [0];
    stream.read_exact(&mut response)?;
    if response != [byte] {
        return Err(io::Error::other("fixture response changed"));
    }
    Ok(())
}

fn serve(root: &Path, args: &[String], stdout: &Path, case: &str) -> io::Result<()> {
    let name = after(args, "--name")?;
    let image = args
        .iter()
        .find(|arg| arg.starts_with("sha256:"))
        .ok_or_else(|| io::Error::other("fixture image missing"))?;
    let owner = after(args, "--label")?;
    let (_, owner) = owner
        .split_once('=')
        .ok_or_else(|| io::Error::other("fixture label missing"))?;
    let manifest = (case == "manifest-store").then_some(image.as_str());
    state(root, &name, owner, image, manifest)?;
    let mount = after(args, "--mount")?;
    let config = mount
        .strip_prefix("type=bind,source=")
        .and_then(|value| value.split_once(",target="))
        .map(|(path, _)| PathBuf::from(path))
        .ok_or_else(|| io::Error::other("fixture mount missing"))?;
    let http = std::fs::read_to_string(config.join("http"))?;
    let sip = std::fs::read_to_string(config.join("sip"))?;
    let listener = TcpListener::bind(&http)?;
    let _sip = UdpSocket::bind(sip)?;
    listener.set_nonblocking(true)?;
    std::fs::write(root.join(format!("{name}.address")), &http)?;
    if case != "missing-ready" {
        std::fs::write(
            stdout,
            format!("{{\"fields\":{{\"message\":\"server ready\",\"http\":\"{http}\"}}}}\n"),
        )?;
    }
    trace(root, "serve")?;
    loop {
        let mut stream = rusterisk_test_support::accept(&listener, Wait::Shutdown)?;
        stream.set_read_timeout(Some(Wait::Responsive.duration()))?;
        stream.set_write_timeout(Some(Wait::Responsive.duration()))?;
        let mut request = [0];
        stream.read_exact(&mut request)?;
        stream.write_all(&request)?;
        if request == *b"Q" {
            if matches!(case, "foreign-cleanup" | "combined-failure") {
                state(root, &name, "foreign", image, manifest)?;
            } else {
                std::fs::remove_file(root.join(format!("{name}.json")))?;
            }
            return Ok(());
        }
    }
}

pub(super) fn execute(root: &Path) -> io::Result<()> {
    let args: Vec<String> =
        serde_json::from_str(&env("RUSTERISK_OCI_FIXTURE_ARGS")?).map_err(io::Error::other)?;
    let stdout = PathBuf::from(env("RUSTERISK_OCI_FIXTURE_STDOUT")?);
    let stderr = PathBuf::from(env("RUSTERISK_OCI_FIXTURE_STDERR")?);
    std::fs::write(&stdout, [])?;
    std::fs::write(&stderr, [])?;
    let case = env("RUSTERISK_OCI_FIXTURE_CASE")?;
    let verb = args
        .first()
        .map(String::as_str)
        .ok_or_else(|| io::Error::other("fixture verb missing"))?;
    match verb {
        "load" => {
            trace(root, "load")?;
            let source = after(&args, "--input")?;
            if std::fs::read(source)? != b"owned archive fixture" {
                return Err(io::Error::other("fixture archive changed"));
            }
            let line = match case.as_str() {
                "manifest-store" => format!("Loaded image ID: sha256:{}\n", "b".repeat(64)),
                "index-store" => format!("Loaded image ID: sha256:{}\n", "c".repeat(64)),
                "tag-load" => "Loaded image: unrelated:latest\n".into(),
                "multi-load" => format!(
                    "Loaded image ID: sha256:{}\nLoaded image ID: sha256:{}\n",
                    "a".repeat(64),
                    "b".repeat(64)
                ),
                _ => format!("Loaded image ID: sha256:{}\n", "a".repeat(64)),
            };
            std::fs::write(stdout, line)?;
        }
        "image" => {
            trace(root, "image")?;
            let image = if matches!(case.as_str(), "changed-image" | "manifest-store") {
                format!("sha256:{}", "b".repeat(64))
            } else {
                format!("sha256:{}", "a".repeat(64))
            };
            if case == "manifest-store" && args.last() != Some(&image) {
                return Err(io::Error::other(
                    "manifest store rejects configuration IDs as image identifiers",
                ));
            }
            let descriptor = (case == "manifest-store").then(|| ImageDescriptor {
                digest: image.clone(),
                media_type: "application/vnd.oci.image.manifest.v1+json".into(),
                size: 1014,
            });
            std::fs::write(
                stdout,
                serde_json::to_vec(&ImageView {
                    id: image,
                    os: "linux",
                    architecture: "arm64",
                    descriptor,
                })
                .map_err(io::Error::other)?,
            )?;
        }
        "container" => {
            let sub = args
                .get(1)
                .map(String::as_str)
                .ok_or_else(|| io::Error::other("fixture subcommand missing"))?;
            match sub {
                "ls" => {
                    trace(root, "list")?;
                    let name = after(&args, "--filter")?;
                    let name = name
                        .strip_prefix("name=^/")
                        .and_then(|value| value.strip_suffix('$'))
                        .ok_or_else(|| io::Error::other("fixture filter changed"))?;
                    if root.join(format!("{name}.json")).exists() {
                        std::fs::write(stdout, format!("{name}\n"))?;
                    }
                }
                "inspect" => {
                    trace(root, "inspect")?;
                    let name = args
                        .last()
                        .ok_or_else(|| io::Error::other("fixture name missing"))?;
                    std::fs::copy(root.join(format!("{name}.json")), stdout)?;
                    if case == "replacement"
                        && std::fs::read_to_string(root.join("trace"))?
                            .lines()
                            .any(|line| line == "stop")
                    {
                        let image = format!("sha256:{}", "a".repeat(64));
                        state(root, name, "foreign", &image, None)?;
                    }
                }
                _ => return Err(io::Error::other("fixture container subcommand changed")),
            }
        }
        "run" => {
            if args.iter().any(|arg| arg == "--version") {
                trace(root, "version")?;
                std::fs::write(stdout, b"rusterisk 0.1.0\n")?;
            } else if args.iter().any(|arg| arg == "--check-config") {
                trace(root, "configuration")?;
                std::fs::write(
                    stdout,
                    if case == "config-output" {
                        b"Configuration invalid.\n".as_slice()
                    } else {
                        b"Configuration valid.\n".as_slice()
                    },
                )?;
            } else {
                serve(root, &args, &stdout, &case)?;
            }
        }
        "stop" => {
            trace(root, "stop")?;
            if args.get(1).map(String::as_str) != Some("--timeout=5") {
                return Err(io::Error::other("fixture stop timeout option changed"));
            }
            if matches!(case.as_str(), "stop-failure" | "replacement") {
                return Err(io::Error::other("requested stop failure"));
            }
            let identity = args
                .last()
                .ok_or_else(|| io::Error::other("fixture stop name missing"))?;
            if identity.starts_with("rusterisk-") {
                trace(root, "stop-by-name")?;
            }
            let name = named(root, identity)?;
            let address = std::fs::read_to_string(root.join(format!("{name}.address")))?
                .parse()
                .map_err(io::Error::other)?;
            request(address, b'Q')?;
        }
        "rm" => {
            trace(root, "remove")?;
            let identity = args
                .last()
                .ok_or_else(|| io::Error::other("fixture removal name missing"))?;
            if identity.starts_with("rusterisk-") {
                trace(root, "remove-by-name")?;
            }
            let name = named(root, identity)?;
            std::fs::remove_file(root.join(format!("{name}.json")))?;
        }
        _ => return Err(io::Error::other("fixture command changed")),
    }
    Ok(())
}
