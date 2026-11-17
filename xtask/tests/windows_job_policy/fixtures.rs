const QUERY_EXPECTATION: &str = r#"#[expect(unsafe_code, reason = "docs/adr/assign-windows-command-trees-to-jobs-during-creation.md: Read one basic Job accounting record through a borrowed handle.")]"#;

const ACCOUNTING_SOURCE: &str = r#"
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

fn query_exception() -> xtask::Exception {
    xtask::Exception {
        path: "crates/windows-job-accounting/src/lib.rs".into(),
        item: "active_processes".into(),
        lint: "unsafe_code".into(),
        adr: "docs/adr/assign-windows-command-trees-to-jobs-during-creation.md".into(),
        rationale: "Read one basic Job accounting record through a borrowed handle.".into(),
    }
}

fn assert_rejected(
    source: &str,
    exceptions: &[xtask::Exception],
    rule: &str,
) -> Result<(), syn::Error> {
    let findings = xtask::inspect_with_exceptions(source, exceptions)?;
    assert!(
        findings.iter().any(|finding| finding.rule == rule),
        "{rule}: {source}: {findings:?}"
    );
    Ok(())
}

#[test]
fn rejects_unsafe_syntax_before_cfg_expansion() -> Result<(), syn::Error> {
    for source in [
        "fn f() { unsafe {} }",
        "unsafe fn f() {}",
        "struct S; impl S { unsafe fn f() {} }",
        "unsafe trait T {}",
        "trait T {} struct S; unsafe impl T for S {}",
        "unsafe extern \"C\" { fn foreign(); }",
        "extern \"C\" { fn foreign(); }",
        "extern \"C\" { static FOREIGN: u8; }",
        "#[cfg(windows)] fn f() { unsafe {} }",
        "#[cfg(not(windows))] fn f() { unsafe {} }",
        "#[cfg(any())] fn f() { unsafe {} }",
        "#[cfg_attr(all(), cfg(any()))] fn f() { unsafe {} }",
        "mod nested { fn f() { unsafe {} } }",
        "macro_rules! hidden { () => { unsafe {} }; }",
        "macro_rules! hidden { () => { unsafe fn f() {} }; }",
        "macro_rules! hidden { () => { extern \"C\" { fn foreign(); } }; }",
        "tokens!(unsafe { foreign() });",
        "tokens!({ (unsafe { foreign() }) });",
        "#[cfg(any())] macro_rules! hidden { () => { unsafe {} }; }",
    ] {
        assert_rejected(source, &[], "unsafe-code")?;
    }
    Ok(())
}

#[test]
fn rejects_assembly_macros_in_all_cfg_branches() -> Result<(), syn::Error> {
    for source in [
        "core::arch::global_asm!(\"nop\");",
        "#[cfg(any())] core::arch::global_asm!(\"nop\");",
        "#[cfg(windows)] fn f() { core::arch::asm!(\"nop\"); }",
        "#[cfg(not(windows))] fn f() { core::arch::naked_asm!(\"nop\"); }",
        "macro_rules! hidden { () => { ::core::arch::global_asm!(\"nop\"); }; }",
    ] {
        assert_rejected(source, &[], "unsafe-code")?;
    }
    Ok(())
}

#[test]
fn rejects_assembly_import_aliases_in_all_cfg_branches() -> Result<(), syn::Error> {
    for source in [
        "use core::arch::global_asm as assembly; assembly!(\"nop\");",
        "#[cfg(any())] use core::arch::{asm as assembly, naked_asm as naked};",
        "use std::arch::r#global_asm as assembly; assembly!(\"nop\");",
        "macro_rules! hidden { () => { use core::arch::global_asm as assembly; assembly!(\"nop\"); }; }",
    ] {
        assert_rejected(source, &[], "unsafe-code")?;
    }
    Ok(())
}

#[test]
fn rejects_unsafe_attributes_in_legacy_and_wrapped_forms() -> Result<(), syn::Error> {
    for source in [
        "#[no_mangle] fn f() {}",
        "#[unsafe(no_mangle)] fn f() {}",
        "#[export_name = \"foreign\"] fn f() {}",
        "#[unsafe(export_name = \"foreign\")] fn f() {}",
        "#[link_section = \"foreign\"] static VALUE: u8 = 0;",
        "#[unsafe(link_section = \"foreign\")] static VALUE: u8 = 0;",
        "#[cfg_attr(any(), no_mangle)] fn f() {}",
        "#[cfg_attr(any(), unsafe(no_mangle))] fn f() {}",
        "#[cfg_attr(any(), cfg_attr(all(), unsafe(no_mangle)))] fn f() {}",
        "macro_rules! hidden { () => { #[no_mangle] fn f() {} }; }",
        "tokens!(#[unsafe(no_mangle)] fn f() {});",
        "macro_rules! forward { ($attribute:meta) => { #[$attribute] fn f() {} }; } forward!(no_mangle);",
    ] {
        assert_rejected(source, &[], "unsafe-code")?;
    }
    Ok(())
}

#[test]
fn rejects_suppression_in_attributes_cfg_attr_and_macro_tokens() -> Result<(), syn::Error> {
    for source in [
        "#![allow(unsafe_code)] fn f() {}",
        "#![warn(unsafe_code)] fn f() {}",
        "#[allow(unsafe_code)] fn f() {}",
        "#[expect(unsafe_code)] fn f() {}",
        "#[warn(unsafe_code)] fn f() {}",
        "#[allow(warnings)] fn f() {}",
        "#[expect(unfulfilled_lint_expectations)] fn f() {}",
        "#[warn(unfulfilled_lint_expectations)] fn f() {}",
        "#[cfg_attr(any(), allow(unsafe_code))] fn f() {}",
        "#[cfg_attr(any(), expect(unsafe_code))] fn f() {}",
        "#[cfg_attr(any(), warn(unsafe_code))] fn f() {}",
        "#[cfg_attr(any(), cfg_attr(all(), allow(unsafe_code)))] fn f() {}",
        "#[cfg_attr(any(), cfg_attr(all(), expect(unfulfilled_lint_expectations)))] fn f() {}",
        "#[cfg_attr(any(), warn(unfulfilled_lint_expectations))] fn f() {}",
        "#[allow_internal_unsafe] macro_rules! hidden { () => {} }",
        "#[cfg_attr(any(), allow_internal_unsafe)] macro_rules! hidden { () => {} }",
        "macro_rules! hidden { () => { #[allow(unsafe_code)] fn f() {} }; }",
        "tokens!(#[cfg_attr(any(), warn(unsafe_code))] fn f() {});",
        "macro_rules! forward { ($attribute:meta) => { #[$attribute] fn f() {} }; } forward!(allow(unsafe_code));",
        "macro_rules! forward { ($attribute:meta) => { #[$attribute] fn f() {} }; } forward!(cfg_attr(any(), warn(unsafe_code)));",
    ] {
        assert_rejected(source, &[], "lint-suppression")?;
    }
    Ok(())
}

#[test]
fn permits_only_the_registered_borrowed_accounting_query() -> Result<(), syn::Error> {
    assert!(xtask::inspect_with_exceptions(ACCOUNTING_SOURCE, &[query_exception()])?.is_empty());
    assert_rejected(ACCOUNTING_SOURCE, &[], "unsafe-code")?;
    assert!(xtask::inspect(
        r##"#[doc = "unsafe { ignored() }"] fn f() { let _ = r#"unsafe_code #[allow(unsafe_code)]"#; }"##
    )?
    .is_empty());
    assert!(
        xtask::inspect("fn f() { let allow = 1; let warn = 2; assert_eq!(allow, warn); }")?
            .is_empty()
    );
    Ok(())
}

#[test]
fn rejects_unsafe_expectations_registered_for_other_syntax() -> Result<(), syn::Error> {
    let source = format!("{QUERY_EXPECTATION}\nenum active_processes {{ End }}");
    assert!(!xtask::inspect_with_exceptions(&source, &[query_exception()])?.is_empty());
    let mut raw_lint = query_exception();
    raw_lint.lint = "r#unsafe_code".into();
    assert!(!xtask::inspect_with_exceptions(
        &source.replace("unsafe_code", "r#unsafe_code"),
        &[raw_lint]
    )?
    .is_empty());
    Ok(())
}

#[test]
fn permits_ordinary_macro_variables_and_function_calls() -> Result<(), syn::Error> {
    for source in [
        "fn f(no_mangle: u8, export_name: u8, link_section: u8, allow_internal_unsafe: u8) { assert_eq!(no_mangle, export_name); assert_eq!(link_section, allow_internal_unsafe); }",
        "fn allow(value: u8) -> u8 { value } fn warn(value: u8) -> u8 { value } fn f() { assert_eq!(allow(1), warn(1)); }",
        "fn cfg_attr(first: u8, second: u8) -> u8 { first + second } fn f() { assert_eq!(cfg_attr(1, 2), 3); }",
        "fn f(asm: u8, global_asm: u8, naked_asm: u8) { assert!(asm != global_asm); assert_eq!(naked_asm, 1); }",
    ] {
        let findings = xtask::inspect(source)?;
        assert!(findings.is_empty(), "{source}: {findings:?}");
    }
    Ok(())
}

#[test]
fn permits_fixed_safe_attributes_with_dynamic_cfg_predicates() -> Result<(), syn::Error> {
    for source in [
        "macro_rules! define { ($predicate:meta) => { #[cfg_attr($predicate, inline)] fn f() {} }; } define!(any());",
        "macro_rules! define { ($($predicate:tt)*) => { #[cfg_attr(all($($predicate)*), inline)] fn f() {} }; } define!(unix);",
        "macro_rules! define { ($predicate:meta) => { #[cfg_attr($predicate, cfg_attr($predicate, inline))] fn f() {} }; } define!(any());",
        "#[cfg_attr(true, inline)] fn f() {}",
        "#[cfg_attr(false, inline)] fn f() {}",
    ] {
        let findings = xtask::inspect(source)?;
        assert!(findings.is_empty(), "{source}: {findings:?}");
    }
    Ok(())
}

#[test]
fn dynamic_cfg_predicates_preserve_unsafe_attribute_and_name_rejection() -> Result<(), syn::Error> {
    for (source, rule) in [
        (
            "macro_rules! define { ($predicate:meta) => { #[cfg_attr($predicate, no_mangle)] fn f() {} }; }",
            "unsafe-code",
        ),
        (
            "macro_rules! define { ($predicate:meta) => { #[cfg_attr($predicate, allow(unsafe_code))] fn f() {} }; }",
            "lint-suppression",
        ),
        (
            "macro_rules! define { ($predicate:meta) => { #[cfg_attr($predicate, cfg_attr($predicate, expect(unsafe_code)))] fn f() {} }; }",
            "lint-suppression",
        ),
        (
            "macro_rules! define { ($predicate:meta, $attribute:meta) => { #[cfg_attr($predicate, $attribute)] fn f() {} }; }",
            "lint-suppression",
        ),
        (
            "macro_rules! define { ($($predicate:tt)*) => { #[cfg_attr($($predicate)*, inline)] fn f() {} }; } define!(all(), allow(unsafe_code), no_mangle);",
            "lint-suppression",
        ),
        (
            "macro_rules! define { ($comma:tt, $attribute:meta) => { #[cfg_attr(all() $comma $attribute, inline)] fn f() {} }; } define!(,,allow(unsafe_code));",
            "lint-suppression",
        ),
        ("#[cfg_attr(false, no_mangle)] fn f() {}", "unsafe-code"),
    ] {
        assert_rejected(source, &[], rule)?;
    }
    Ok(())
}

#[test]
fn rejects_query_changes_extra_operations_and_copied_exceptions() -> Result<(), syn::Error> {
    for source in [
        ACCOUNTING_SOURCE.replace("fn active_processes", "fn other_processes"),
        ACCOUNTING_SOURCE.replace("pub fn active_processes", "pub(super) fn active_processes"),
        ACCOUNTING_SOURCE.replace("::windows_sys::", "windows_sys::"),
        ACCOUNTING_SOURCE.replace("BorrowedHandle<'_>", "OwnedHandle"),
        ACCOUNTING_SOURCE.replace(
            "JobObjectBasicAccountingInformation,",
            "JobObjectBasicProcessIdList,",
        ),
        ACCOUNTING_SOURCE.replace(
            "::core::ptr::addr_of_mut!(information)",
            "::core::ptr::null_mut()",
        ),
        ACCOUNTING_SOURCE.replace("            length,", "            0,"),
        ACCOUNTING_SOURCE.replace(
            "::core::ptr::null_mut(),",
            "::core::ptr::addr_of_mut!(length),",
        ),
        ACCOUNTING_SOURCE.replace(
            "let queried = unsafe {",
            "let queried = unsafe { foreign();",
        ),
        ACCOUNTING_SOURCE.replace("if queried == 0", "let _extra = unsafe {}; if queried == 0"),
        ACCOUNTING_SOURCE.replace("Ok(information.ActiveProcesses)", "Ok(0)"),
        ACCOUNTING_SOURCE.replace("let length =", "let _extra = handle; let length ="),
        ACCOUNTING_SOURCE.replace("#[expect(unsafe_code,", "#[allow(unsafe_code,"),
        ACCOUNTING_SOURCE.replace(
            "#[expect(unsafe_code,",
            "#[expect(unsafe_code, unused_variables,",
        ),
        ACCOUNTING_SOURCE.replace(QUERY_EXPECTATION, ""),
        ACCOUNTING_SOURCE.replace("a borrowed handle.", "an unregistered handle."),
        ACCOUNTING_SOURCE.replace(
            "pub fn active_processes",
            "#[cfg(any())]\npub fn active_processes",
        ),
        format!("mod nested {{ {ACCOUNTING_SOURCE} }}"),
        format!(
            "{ACCOUNTING_SOURCE}\n{}",
            ACCOUNTING_SOURCE.replacen("#![cfg(windows)]", "", 1)
        ),
        format!("{ACCOUNTING_SOURCE}\nfn additional() {{ unsafe {{}} }}"),
        format!("{ACCOUNTING_SOURCE}\nfn additional() -> u8 {{ 1 }}"),
        format!("{ACCOUNTING_SOURCE}\nextern crate foreign as windows_sys;"),
    ] {
        let findings = xtask::inspect_with_exceptions(&source, &[query_exception()])?;
        assert!(!findings.is_empty(), "{source}");
    }
    let mut wrong_path = query_exception();
    wrong_path.path = "xtask/src/process/elsewhere.rs".into();
    let mut wrong_item = query_exception();
    wrong_item.item = "other_processes".into();
    let mut wrong_lint = query_exception();
    wrong_lint.lint = "warnings".into();
    let mut wrong_adr = query_exception();
    wrong_adr.adr = "docs/adr/another-decision.md".into();
    let mut wrong_rationale = query_exception();
    wrong_rationale.rationale = "Unregistered reason.".into();
    for exception in [
        wrong_path,
        wrong_item,
        wrong_lint,
        wrong_adr,
        wrong_rationale,
    ] {
        assert!(!xtask::inspect_with_exceptions(ACCOUNTING_SOURCE, &[exception])?.is_empty());
    }
    assert!(!xtask::inspect_with_exceptions(
        ACCOUNTING_SOURCE,
        &[query_exception(), query_exception()]
    )?
    .is_empty());
    Ok(())
}
