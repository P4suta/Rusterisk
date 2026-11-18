use serde_json::{json, Value};
use std::io;

#[cfg(windows)]
mod native;

const EXECUTABLE: &str = r"C:\native candidate\server\bin\rusterisk.exe";
const SYSTEM_DIRECTORY: &str = r"C:\Windows\System32";

fn array<T: Into<Value>, const N: usize>(values: [T; N]) -> Value {
    Value::Array(values.into_iter().map(Into::into).collect())
}

fn observation() -> Value {
    let modules = array([
        EXECUTABLE,
        r"C:\Windows\System32\ntdll.dll",
        r"C:\Windows\System32\KERNEL32.DLL",
    ]);
    json!({
        "format": 1,
        "observer_bits": 64,
        "observer_version": "5.1.22621.2506",
        "executable": EXECUTABLE,
        "system_directory": SYSTEM_DIRECTORY,
        "modules": modules,
    })
}

fn rejected(value: &Value, executable: &str) -> io::Result<()> {
    let stdout = serde_json::to_vec(value).map_err(io::Error::other)?;
    assert!(
        super::check_output(&stdout, b"", executable).is_err(),
        "Invalid Windows module observation was accepted: {value}"
    );
    Ok(())
}

#[test]
fn native_modules_accept_case_differences_and_verbatim_drive_prefixes() -> io::Result<()> {
    let mut value = observation();
    value["executable"] = json!(r"c:\NATIVE candidate\SERVER\bin\RUSTERISK.EXE");
    value["system_directory"] = json!(r"c:\WINDOWS\system32");
    value["modules"] = array([
        r"C:\native candidate\server\bin\rusterisk.exe",
        r"c:\WINDOWS\System32\ntdll.dll",
        r"\\?\C:\Windows\System32\KERNEL32.DLL",
    ]);
    super::check_output(
        &serde_json::to_vec(&value).map_err(io::Error::other)?,
        b"",
        r"\\?\C:\native candidate\server\bin\rusterisk.exe",
    )
}

#[test]
fn observer_identity_and_fields_must_match_the_native_profile() -> io::Result<()> {
    for (field, replacement) in [
        ("format", json!(0)),
        ("format", json!(2)),
        ("format", json!("1")),
        ("observer_bits", json!(32)),
        ("observer_bits", json!("64")),
        ("observer_version", json!("")),
        ("observer_version", json!("5.1.invalid.0")),
        ("observer_version", json!("5.1.22621.2506\n")),
        ("executable", Value::Null),
        ("system_directory", Value::Null),
        ("modules", json!("KERNEL32.DLL")),
        ("modules", array([Value::from(EXECUTABLE), Value::Null])),
    ] {
        let mut value = observation();
        value[field] = replacement;
        rejected(&value, EXECUTABLE)?;
    }
    for field in [
        "format",
        "observer_bits",
        "observer_version",
        "executable",
        "system_directory",
        "modules",
    ] {
        let mut value = observation();
        let object = value
            .as_object_mut()
            .ok_or_else(|| io::Error::other("Windows module fixture is not an object"))?;
        assert!(object.remove(field).is_some());
        rejected(&value, EXECUTABLE)?;
    }
    let mut value = observation();
    value["verified"] = json!(true);
    rejected(&value, EXECUTABLE)
}

#[test]
fn observations_require_the_requested_executable_and_one_loaded_main_module() -> io::Result<()> {
    let mut foreign = observation();
    foreign["executable"] = json!(r"C:\foreign\bin\rusterisk.exe");
    rejected(&foreign, EXECUTABLE)?;
    for modules in [
        array::<Value, 0>([]),
        array([EXECUTABLE]),
        array([r"C:\Windows\System32\ntdll.dll"]),
        array([
            r"C:\foreign\bin\rusterisk.exe",
            r"C:\Windows\System32\ntdll.dll",
        ]),
        array([
            EXECUTABLE,
            r"\\?\c:\NATIVE candidate\server\bin\RUSTERISK.EXE",
            r"C:\Windows\System32\ntdll.dll",
        ]),
        array([
            EXECUTABLE,
            r"C:\Windows\System32\ntdll.dll",
            r"\\?\c:\WINDOWS\system32\NTDLL.DLL",
        ]),
    ] {
        let mut value = observation();
        value["modules"] = modules;
        rejected(&value, EXECUTABLE)?;
    }
    rejected(
        &observation(),
        r"D:\native candidate\server\bin\rusterisk.exe",
    )
}

#[test]
fn loaded_dlls_cannot_escape_the_observed_system_directory() -> io::Result<()> {
    for path in [
        r"C:\native candidate\server\bin\VCRUNTIME140.dll",
        r"C:\tools\KERNEL32.DLL",
        r"C:\Windows\System32-shadow\KERNEL32.DLL",
        r"C:\Windows\SysWOW64\KERNEL32.DLL",
        r"D:\Windows\System32\KERNEL32.DLL",
        r"C:\Windows\System32\..\Temp\KERNEL32.DLL",
        r"C:\Windows\System32\.\KERNEL32.DLL",
        r"\\?\C:\Windows\System32\..\Temp\KERNEL32.DLL",
    ] {
        let mut value = observation();
        value["modules"][2] = json!(path);
        rejected(&value, EXECUTABLE)?;
    }
    Ok(())
}

#[test]
fn module_paths_reject_device_namespaces_and_ambiguous_components() -> io::Result<()> {
    for path in [
        r"KERNEL32.DLL",
        r"C:Windows\System32\KERNEL32.DLL",
        r"\Windows\System32\KERNEL32.DLL",
        r"\\server\share\Windows\System32\KERNEL32.DLL",
        r"\\?\UNC\server\share\KERNEL32.DLL",
        r"\\?\GLOBALROOT\Device\HarddiskVolume1\Windows\System32\KERNEL32.DLL",
        r"\\.\C:\Windows\System32\KERNEL32.DLL",
        r"\??\C:\Windows\System32\KERNEL32.DLL",
        r"C:/Windows/System32/KERNEL32.DLL",
        r"C:\Windows\System32\\KERNEL32.DLL",
        r"C:\Windows\System32\KERNEL32.DLL:alternate",
        r"C:\Windows\System32\KERNEL32.DLL.",
        r"C:\Windows\System32\KERNEL32.DLL ",
        r"C:\Windows\System32\NUL.dll",
        "C:\\Windows\\System32\\KERNEL32.DLL\n",
    ] {
        let mut value = observation();
        value["modules"][2] = json!(path);
        rejected(&value, EXECUTABLE)?;
    }
    Ok(())
}

#[test]
fn system_and_executable_paths_cannot_hide_the_same_escapes() -> io::Result<()> {
    for directory in [
        r"C:\Windows\System32\..\Temp",
        r"C:\Windows\System32.",
        r"C:\Windows\System32 ",
        r"\\?\GLOBALROOT\Device\HarddiskVolume1\Windows\System32",
    ] {
        let mut value = observation();
        value["system_directory"] = json!(directory);
        value["modules"] = array([
            EXECUTABLE.to_owned(),
            format!("{directory}\\ntdll.dll"),
            format!("{directory}\\KERNEL32.DLL"),
        ]);
        rejected(&value, EXECUTABLE)?;
    }
    for executable in [
        r"C:\native candidate\server\..\bin\rusterisk.exe",
        r"C:\native candidate\server\bin\rusterisk.exe:alternate",
        r"C:\native candidate\server\bin\rusterisk.exe.",
        r"\\.\C:\native candidate\server\bin\rusterisk.exe",
    ] {
        let mut value = observation();
        value["executable"] = json!(executable);
        value["modules"][0] = json!(executable);
        rejected(&value, executable)?;
    }
    Ok(())
}

#[test]
fn malformed_diagnostic_and_unbounded_observations_are_rejected() -> io::Result<()> {
    let valid = serde_json::to_vec(&observation()).map_err(io::Error::other)?;
    for stdout in [b"".as_slice(), b"{}", b"[]", b"null", b"{", b"\xff"] {
        assert!(super::check_output(stdout, b"", EXECUTABLE).is_err());
    }
    assert!(super::check_output(&valid, b"inspection failed\n", EXECUTABLE).is_err());
    let duplicate = String::from_utf8(valid.clone())
        .map_err(io::Error::other)?
        .replacen('{', "{\"format\":1,", 1);
    assert!(super::check_output(duplicate.as_bytes(), b"", EXECUTABLE).is_err());
    let mut oversized = valid;
    oversized.resize(65_537, b' ');
    assert!(super::check_output(&oversized, b"", EXECUTABLE).is_err());
    let mut value = observation();
    value["modules"] = Value::Array(
        std::iter::once(json!(EXECUTABLE))
            .chain((0..64).map(|index| json!(format!("{SYSTEM_DIRECTORY}\\module-{index}.dll"))))
            .collect(),
    );
    rejected(&value, EXECUTABLE)
}
