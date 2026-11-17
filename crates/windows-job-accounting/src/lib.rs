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
    #[expect(
        unsafe_code,
        reason = "docs/adr/assign-windows-command-trees-to-jobs-during-creation.md: Read one basic Job accounting record through a borrowed handle."
    )]
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
