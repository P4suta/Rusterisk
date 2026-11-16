use super::*;

#[derive(Deserialize)]
struct ContainerIdentity {
    image: String,
    manifest: Descriptor,
}

fn descriptor(digest: &str, size: u64) -> String {
    format!(
        r#"{{"digest":"{digest}","media_type":"application/vnd.oci.image.manifest.v1+json","size":{size}}}"#
    )
}

fn view(id: &str, descriptor: &str) -> String {
    format!(r#"{{"id":"{id}","os":"linux","architecture":"arm64","descriptor":{descriptor}}}"#)
}

#[test]
fn image_profiles_bind_identifiers_descriptors_and_platforms_to_the_verified_graph(
) -> io::Result<()> {
    let config = format!("sha256:{}", "a".repeat(64));
    let manifest = format!("sha256:{}", "b".repeat(64));
    let other = format!("sha256:{}", "c".repeat(64));
    let expected = ExpectedImage {
        configuration: &config,
        manifest: &manifest,
        manifest_size: 1014,
        target: ServerTarget::LinuxArm64,
    };
    let legacy = EngineImage::loaded(format!("Loaded image ID: {config}\n").as_bytes(), expected)?;
    let modern = EngineImage::loaded(
        format!("Loaded image ID: {manifest}\n").as_bytes(),
        expected,
    )?;
    let raw_descriptor = descriptor(&manifest, 1014);
    let legacy_view = view(&config, "null");
    let modern_view = view(&manifest, &raw_descriptor);
    legacy.check(legacy_view.as_bytes())?;
    modern.check(modern_view.as_bytes())?;
    assert!(legacy
        .check(view(&config, &raw_descriptor).as_bytes())
        .is_err());
    for invalid in [
        view(&config, &raw_descriptor),
        view(&manifest, "null"),
        view(&manifest, &descriptor(&other, 1014)),
        view(&manifest, &descriptor(&manifest, 1015)),
        modern_view.replace(
            "application/vnd.oci.image.manifest.v1+json",
            "application/vnd.oci.image.index.v1+json",
        ),
        modern_view.replace("arm64", "amd64"),
        modern_view.replace("linux", "windows"),
        modern_view.replace("\"os\":\"linux\"", "\"os\":\"linux\",\"os\":\"linux\""),
        modern_view.replace("\"os\":\"linux\"", "\"unexpected\":true,\"os\":\"linux\""),
        "x".repeat(16 * 1024 + 1),
    ] {
        assert!(modern.check(invalid.as_bytes()).is_err());
    }
    let descriptor: Descriptor = serde_json::from_str(&raw_descriptor).map_err(io::Error::other)?;
    assert!(modern.container_matches(&manifest, Some(&descriptor)));
    assert!(!modern.container_matches(&config, Some(&descriptor)));
    assert!(!modern.container_matches(&manifest, None));
    assert!(legacy.container_matches(&config, None));
    assert!(!legacy.container_matches(&manifest, None));
    assert!(!legacy.container_matches(&config, Some(&descriptor)));
    Ok(())
}

#[test]
fn observed_containerd_metadata_keeps_the_manifest_and_configuration_distinct() -> io::Result<()> {
    let expected = ExpectedImage {
        configuration: "sha256:329be8cfdf6da2df53999e53c0e37c542c4b80a5fea93398cf72a34284c658b2",
        manifest: "sha256:eb2b501054399158d4274c5073d1446a3bda8e10fbe1417abd94ce1ab5669678",
        manifest_size: 1014,
        target: ServerTarget::LinuxArm64,
    };
    let loaded = b"Loaded image ID: sha256:eb2b501054399158d4274c5073d1446a3bda8e10fbe1417abd94ce1ab5669678\n";
    let inspect = br#"{"id":"sha256:eb2b501054399158d4274c5073d1446a3bda8e10fbe1417abd94ce1ab5669678","os":"linux","architecture":"arm64","descriptor":{"digest":"sha256:eb2b501054399158d4274c5073d1446a3bda8e10fbe1417abd94ce1ab5669678","media_type":"application/vnd.oci.image.manifest.v1+json","size":1014}}"#;
    let image = EngineImage::loaded(loaded, expected)?;
    assert_eq!(image.id(), expected.manifest);
    image.check(inspect)?;
    let observed = br#"{"id":"5d7a3d75a218dc458d46716ab731440bae1d9ae294a21b72c637f3e766945dc6","image":"sha256:eb2b501054399158d4274c5073d1446a3bda8e10fbe1417abd94ce1ab5669678","owner":"rusterisk-server-oci-check-54043-1794836730958011000-serve","manifest":{"digest":"sha256:eb2b501054399158d4274c5073d1446a3bda8e10fbe1417abd94ce1ab5669678","media_type":"application/vnd.oci.image.manifest.v1+json","size":1014}}"#;
    let container: ContainerIdentity =
        serde_json::from_slice(observed).map_err(io::Error::other)?;
    assert!(image.container_matches(&container.image, Some(&container.manifest)));
    assert!(!image.container_matches(expected.configuration, Some(&container.manifest)));
    Ok(())
}
