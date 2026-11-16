use std::{
    collections::BTreeMap,
    io::{Cursor, Write},
    path::{Path, PathBuf},
};
use xtask::{
    distribution::SdkPackage,
    regular_archive::{read_regular_gzip, ArchiveLimits, ArchiveRoot, RegularArchive},
};

pub struct Fixture {
    pub source: BTreeMap<PathBuf, Vec<u8>>,
    pub files: BTreeMap<String, Vec<u8>>,
    package: SdkPackage,
}

impl Fixture {
    pub fn new(package: SdkPackage) -> Self {
        let name = package.name();
        let rust_version = if package == SdkPackage::Core {
            "1.89"
        } else {
            "1.99"
        };
        let mut original = format!(
            "[package]\nname='{name}'\nversion.workspace=true\nedition.workspace=true\nrust-version='{rust_version}'\nlicense.workspace=true\nrepository.workspace=true\ndescription='SDK fixture'\nreadme='../../docs/rust-sdk.md'\n"
        );
        let mut published = format!(
            "[package]\nname='{name}'\nversion='0.1.0'\nedition='2021'\nrust-version='{rust_version}'\nlicense='MIT OR Apache-2.0'\nrepository='https://github.com/P4suta/Rusterisk'\ndescription='SDK fixture'\nreadme='rust-sdk.md'\nbuild=false\nautolib=false\nautobins=false\nautoexamples=false\nautotests=false\nautobenches=false\n"
        );
        if package == SdkPackage::Client {
            original.push_str("exclude=['tests/**','examples/**']\n");
            published.push_str("exclude=['tests/**','examples/**']\n");
            original.push_str("[dependencies]\nrusterisk-api={version='0.1.0',path='../api'}\nrusterisk-core={version='0.1.0',path='../core'}\n[dev-dependencies]\nrusterisk-server={path='../server'}\nrusterisk-config={path='../config'}\nrusterisk-media={path='../media'}\n");
        }
        original.push_str("[lints]\nworkspace=true\n");
        published.push_str(&format!(
            "[lib]\nname='{}'\npath='src/lib.rs'\n",
            name.replace('-', "_")
        ));
        if package != SdkPackage::Client {
            published.push_str("[[test]]\nname='contract'\npath='tests/contract.rs'\n");
        }
        published.push_str("[lints.rust]\nunsafe_code='forbid'\n");
        if package == SdkPackage::Client {
            published.push_str("[dependencies.rusterisk-api]\nversion='0.1.0'\n[dependencies.rusterisk-core]\nversion='0.1.0'\n");
        }
        let mut source = BTreeMap::from([
            (PathBuf::from("Cargo.toml"), b"[workspace.package]\nversion='0.1.0'\nedition='2021'\nrust-version='1.99'\nlicense='MIT OR Apache-2.0'\nrepository='https://github.com/P4suta/Rusterisk'\n[workspace.lints.rust]\nunsafe_code='forbid'\n".to_vec()),
            (PathBuf::from("LICENSE-MIT"), b"MIT fixture\n".to_vec()),
            (PathBuf::from("LICENSE-APACHE"), b"Apache fixture\n".to_vec()),
            (PathBuf::from("docs/rust-sdk.md"), b"SDK fixture documentation.\n".to_vec()),
        ]);
        let mut files = BTreeMap::from([
            ("Cargo.toml".to_owned(), published.into_bytes()),
            ("Cargo.toml.orig".to_owned(), original.as_bytes().to_vec()),
            (
                "Cargo.lock".to_owned(),
                format!("version=4\n[[package]]\nname='{name}'\nversion='0.1.0'\n").into_bytes(),
            ),
            ("LICENSE-MIT".to_owned(), b"MIT fixture\n".to_vec()),
            ("LICENSE-APACHE".to_owned(), b"Apache fixture\n".to_vec()),
            (
                "rust-sdk.md".to_owned(),
                b"SDK fixture documentation.\n".to_vec(),
            ),
            (
                "src/lib.rs".to_owned(),
                b"pub const VALUE: u8 = 1;\n".to_vec(),
            ),
            (
                "src/nested/module.rs".to_owned(),
                b"pub const NESTED: bool = true;\n".to_vec(),
            ),
        ]);
        source.insert(
            Path::new(package.directory()).join("Cargo.toml"),
            original.into_bytes(),
        );
        for path in [
            "LICENSE-MIT",
            "LICENSE-APACHE",
            "src/lib.rs",
            "src/nested/module.rs",
        ] {
            if let Some(bytes) = files.get(path) {
                source.insert(Path::new(package.directory()).join(path), bytes.clone());
            }
        }
        let test = b"#[test]\nfn contract() { assert_eq!(1, 1); }\n".to_vec();
        source.insert(
            Path::new(package.directory()).join("tests/contract.rs"),
            test.clone(),
        );
        if package != SdkPackage::Client {
            files.insert("tests/contract.rs".to_owned(), test);
        } else {
            source.insert(
                Path::new(package.directory()).join("examples/workspace.rs"),
                b"fn main() {}\n".to_vec(),
            );
        }
        Self {
            source,
            files,
            package,
        }
    }

    pub fn captured(&self) -> impl Iterator<Item = (&Path, &[u8])> {
        self.source
            .iter()
            .map(|(path, bytes)| (path.as_path(), bytes.as_slice()))
    }

    pub fn bytes(&self) -> Result<Vec<u8>, std::io::Error> {
        let root = format!("{}-0.1.0", self.package.name());
        let mut archive = tar::Builder::new(Vec::new());
        for (path, bytes) in &self.files {
            let mut header = tar::Header::new_ustar();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_entry_type(tar::EntryType::Regular);
            archive.append_data(&mut header, format!("{root}/{path}"), bytes.as_slice())?;
        }
        let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gzip.write_all(&archive.into_inner()?)?;
        gzip.finish()
    }

    pub fn archive(&self) -> Result<RegularArchive, std::io::Error> {
        let root = format!("{}-0.1.0", self.package.name());
        let bytes = self.bytes()?;
        let root = ArchiveRoot::new(&root).map_err(std::io::Error::other)?;
        read_regular_gzip(Cursor::new(bytes), &root, ArchiveLimits::SDK)
            .map_err(std::io::Error::other)
    }
}
