const ROOT_SOURCE: &str = r#"
#![cfg(windows)]

/// Returns the current number of processes assigned to a borrowed Windows Job handle.
///
/// The count can change immediately after the query.
///
/// # Errors
///
/// Returns the operating-system error when the accounting query fails.
pub fn active_processes(
    handle: ::std::os::windows::io::BorrowedHandle<'_>,
) -> ::std::io::Result<::core::primitive::u32> {
    let mut information =
        ::windows_sys::Win32::System::JobObjects::JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
    let length = ::core::primitive::u32::try_from(::core::mem::size_of_val(&information))
        .map_err(::std::io::Error::other)?;
    #[expect(unsafe_code, reason = "docs/adr/assign-windows-command-trees-to-jobs-during-creation.md: Read one basic Job accounting record through a borrowed handle.")]
    let queried = unsafe {
        ::windows_sys::Win32::System::JobObjects::QueryInformationJobObject(
            ::std::os::windows::io::AsRawHandle::as_raw_handle(&handle),
            ::windows_sys::Win32::System::JobObjects::JobObjectBasicAccountingInformation,
            ::core::ptr::addr_of_mut!(information).cast(),
            length,
            ::core::ptr::null_mut(),
        )
    };
    if queried == 0 {
        ::core::result::Result::Err(::std::io::Error::last_os_error())
    } else {
        ::core::result::Result::Ok(information.ActiveProcesses)
    }
}
"#;

fn registration() -> xtask::Exception {
    xtask::Exception {
        path: "crates/windows-job-accounting/src/lib.rs".into(),
        item: "active_processes".into(),
        lint: "unsafe_code".into(),
        adr: "docs/adr/assign-windows-command-trees-to-jobs-during-creation.md".into(),
        rationale: "Read one basic Job accounting record through a borrowed handle.".into(),
    }
}

#[test]
fn permits_only_the_registered_public_query_in_the_windows_crate_root() -> Result<(), syn::Error> {
    assert!(xtask::inspect_with_exceptions(ROOT_SOURCE, &[registration()])?.is_empty());
    assert!(!xtask::inspect(ROOT_SOURCE)?.is_empty());
    Ok(())
}

#[test]
fn rejects_legacy_query_registration_and_simultaneous_exceptions() -> Result<(), syn::Error> {
    let legacy_source = ROOT_SOURCE
        .lines()
        .filter(|line| {
            !line.trim_start().starts_with("///")
                && !line.trim_start().starts_with("#![cfg(windows)]")
        })
        .collect::<Vec<_>>()
        .join("\n")
        .replace("pub fn active_processes", "pub(super) fn active_processes");
    let mut legacy = registration();
    legacy.path = "xtask/src/process/supervisor/windows/accounting.rs".into();
    assert!(
        !xtask::inspect_with_exceptions(&legacy_source, std::slice::from_ref(&legacy))?.is_empty()
    );
    assert!(!xtask::inspect_with_exceptions(ROOT_SOURCE, &[registration(), legacy])?.is_empty());
    Ok(())
}

#[test]
fn rejects_every_additional_root_item_attribute_and_query_operation() -> Result<(), syn::Error> {
    for source in [
        ROOT_SOURCE.replace("#![cfg(windows)]", "#![cfg(any())]"),
        ROOT_SOURCE.replace("#![cfg(windows)]", ""),
        ROOT_SOURCE.replace("pub fn active_processes", "pub(super) fn active_processes"),
        ROOT_SOURCE.replace("fn active_processes", "fn other_processes"),
        ROOT_SOURCE.replace("::windows_sys::", "windows_sys::"),
        ROOT_SOURCE.replace("BorrowedHandle<'_>", "OwnedHandle"),
        ROOT_SOURCE.replace("            length,", "            0,"),
        ROOT_SOURCE.replace(
            "JobObjectBasicAccountingInformation,",
            "JobObjectBasicProcessIdList,",
        ),
        ROOT_SOURCE.replace("let queried = unsafe {", "let queried = unsafe { other();"),
        ROOT_SOURCE.replace("#[expect(unsafe_code,", "#[allow(unsafe_code,"),
        format!("{ROOT_SOURCE}\nfn additional() {{}}"),
        format!("{ROOT_SOURCE}\n#[cfg(any())] mod additional;"),
        format!("{ROOT_SOURCE}\ninclude!(concat!(env!(\"OUT_DIR\"), \"/extra.rs\"));"),
        format!("{ROOT_SOURCE}\n#[derive(Serialize)] struct Additional;"),
        format!("{ROOT_SOURCE}\nextern crate foreign as windows_sys;"),
    ] {
        let findings = xtask::inspect_with_exceptions(&source, &[registration()])?;
        assert!(!findings.is_empty(), "{source}");
    }
    Ok(())
}
