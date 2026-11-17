use syn::ext::IdentExt;
use syn::{buffer::Cursor, parse::ParseStream, parse::Parser, punctuated::Punctuated, Meta, Token};

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

pub(super) fn registration(exception: &crate::Exception) -> bool {
    exception.path == "crates/windows-job-accounting/src/lib.rs"
        && exception.item == "active_processes"
        && exception.lint == "unsafe_code"
        && exception.adr == "docs/adr/assign-windows-command-trees-to-jobs-during-creation.md"
        && exception.rationale == "Read one basic Job accounting record through a borrowed handle."
}

pub(super) fn accounting_query<'a>(
    file: &'a syn::File,
    exceptions: &[crate::Exception],
) -> Result<Option<&'a syn::Local>, syn::Error> {
    if exceptions.iter().filter(|item| registration(item)).count() != 1 {
        return Ok(None);
    }
    let expected: syn::File = syn::parse_str(ACCOUNTING_SOURCE)?;
    if *file != expected {
        return Ok(None);
    }
    let [syn::Item::Fn(function)] = file.items.as_slice() else {
        return Ok(None);
    };
    Ok(match function.block.stmts.get(2) {
        Some(syn::Stmt::Local(local)) => Some(local),
        _ => None,
    })
}

#[derive(Default)]
pub(super) struct Syntax {
    pub(super) unsafe_code: bool,
    pub(super) suppression: bool,
}

impl Syntax {
    fn merge(&mut self, other: Self) {
        self.unsafe_code |= other.unsafe_code;
        self.suppression |= other.suppression;
    }
}

fn cfg_attributes(input: ParseStream<'_>) -> Result<Punctuated<Meta, Token![,]>, syn::Error> {
    if input.peek(Token![$]) {
        input.parse::<Token![$]>()?;
        input.parse::<syn::Ident>()?;
    } else if input.peek(syn::LitBool) {
        input.parse::<syn::LitBool>()?;
    } else {
        input.parse::<Meta>()?;
    }
    input.parse::<Token![,]>()?;
    Punctuated::parse_terminated(input)
}

pub(super) fn attribute(meta: &Meta) -> Syntax {
    let path = meta.path();
    let mut found = Syntax {
        unsafe_code: ["unsafe", "no_mangle", "export_name", "link_section"]
            .iter()
            .any(|name| named(path, name)),
        suppression: ["allow", "expect", "allow_internal_unsafe"]
            .iter()
            .any(|name| named(path, name)),
    };
    if let Meta::List(list) = meta {
        if named(path, "cfg_attr") {
            match cfg_attributes.parse2(list.tokens.clone()) {
                Ok(arguments) => {
                    for nested in &arguments {
                        found.merge(attribute(nested));
                    }
                }
                Err(_) => found.suppression = true,
            }
        } else if named(path, "warn") {
            match Punctuated::<Meta, Token![,]>::parse_terminated.parse2(list.tokens.clone()) {
                Ok(arguments) => {
                    found.suppression |= arguments.iter().any(|argument| {
                        named(argument.path(), "unsafe_code")
                            || named(argument.path(), "unfulfilled_lint_expectations")
                    });
                }
                Err(_) => found.suppression = true,
            }
        }
    }
    found
}

fn named(path: &syn::Path, name: &str) -> bool {
    path.get_ident()
        .is_some_and(|identifier| identifier.unraw() == name)
}

fn assembly_name(identifier: &syn::Ident) -> bool {
    let identifier = identifier.unraw();
    ["asm", "global_asm", "naked_asm"]
        .iter()
        .any(|name| identifier == *name)
}

pub(super) fn assembly_macro(path: &syn::Path) -> bool {
    path.segments
        .last()
        .is_some_and(|segment| assembly_name(&segment.ident))
}

pub(super) fn assembly_import(tree: &syn::UseTree) -> bool {
    match tree {
        syn::UseTree::Path(path) => assembly_import(&path.tree),
        syn::UseTree::Name(name) => assembly_name(&name.ident),
        syn::UseTree::Rename(rename) => assembly_name(&rename.ident),
        syn::UseTree::Group(group) => group.items.iter().any(assembly_import),
        syn::UseTree::Glob(_) => false,
    }
}

fn end(mut cursor: Cursor<'_>) -> Cursor<'_> {
    while let Some((_, remaining)) = cursor.token_tree() {
        cursor = remaining;
    }
    cursor
}

fn leading_attributes(input: ParseStream<'_>) -> Result<Syntax, syn::Error> {
    let mut found = Syntax::default();
    let outer = input.peek(Token![#]) && input.peek2(syn::token::Bracket);
    let inner = input.peek(Token![#]) && input.peek2(Token![!]) && input.peek3(syn::token::Bracket);
    if outer || inner {
        let attributes = if inner {
            syn::Attribute::parse_inner(input)?
        } else {
            syn::Attribute::parse_outer(input)?
        };
        for item in attributes {
            found.merge(attribute(&item.meta));
        }
    }
    input.step(|cursor| Ok((found, end(*cursor))))
}

fn leading_foreign_block(input: ParseStream<'_>) -> Result<(), syn::Error> {
    input.parse::<syn::ItemForeignMod>()?;
    input.step(|cursor| Ok(((), end(*cursor))))
}

fn leading_assembly_import(input: ParseStream<'_>) -> Result<bool, syn::Error> {
    let import = input.parse::<syn::ItemUse>()?;
    input.step(|cursor| Ok((assembly_import(&import.tree), end(*cursor))))
}

fn tokens(input: ParseStream<'_>) -> Result<Syntax, syn::Error> {
    let mut found = Syntax::default();
    let mut pending: Vec<_> = [input.cursor()].into();
    while let Some(mut cursor) = pending.pop() {
        while let Some((_, remaining)) = cursor.token_tree() {
            if let Some((inside, _, _, _)) = cursor.any_group() {
                pending.push(inside);
            } else if let Some((identifier, _)) = cursor.ident() {
                let assembly_call = assembly_name(&identifier)
                    && remaining.punct().is_some_and(|(punctuation, after)| {
                        punctuation.as_char() == '!' && after.any_group().is_some()
                    });
                found.unsafe_code |= identifier == "unsafe"
                    || identifier == "extern"
                        && leading_foreign_block.parse2(cursor.token_stream()).is_ok()
                    || assembly_call
                    || identifier == "use"
                        && matches!(
                            leading_assembly_import.parse2(cursor.token_stream()),
                            Ok(true)
                        );
            } else if cursor
                .punct()
                .is_some_and(|(punctuation, _)| punctuation.as_char() == '#')
            {
                match leading_attributes.parse2(cursor.token_stream()) {
                    Ok(attributes) => found.merge(attributes),
                    Err(_) => {
                        found.unsafe_code = true;
                        found.suppression = true;
                    }
                }
            }
            cursor = remaining;
        }
    }
    input.step(|cursor| Ok((found, end(*cursor))))
}

pub(super) fn macro_tokens(node: &syn::Macro) -> Syntax {
    match tokens.parse2(node.tokens.clone()) {
        Ok(found) => found,
        Err(_) => Syntax {
            unsafe_code: true,
            suppression: true,
        },
    }
}
