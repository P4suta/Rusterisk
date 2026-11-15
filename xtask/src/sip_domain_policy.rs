use crate::Exception;
use syn::{
    Expr, Fields, FnArg, GenericArgument, Item, Local, Member, Pat, Path, PathArguments, Stmt, Type,
};

fn exact_path(path: &Path, names: &[&str], absolute: bool) -> bool {
    path.leading_colon.is_some() == absolute
        && path.segments.len() == names.len()
        && path.segments.iter().zip(names).all(|(segment, name)| {
            segment.ident == *name && matches!(segment.arguments, PathArguments::None)
        })
}

pub(super) fn registration(exception: &Exception) -> bool {
    exception.path == "crates/server/src/encryption/sip_domain.rs"
        && exception.item == "sip_client_configuration"
        && exception.lint == "clippy::disallowed_types"
        && exception.adr == "docs/adr/verify-sip-domains-with-an-immutable-library-callback.md"
        && exception.rationale
            == "Share only immutable SIP certificate trust through the fixed verifier API."
}

pub(super) fn verifier(file: &syn::File) -> bool {
    if file.items.iter().any(|item| {
        if let Item::Type(item) = item {
            item.ident == "SipDomainVerifier"
        } else {
            false
        }
    }) {
        return false;
    }
    let mut declarations = file
        .items
        .iter()
        .filter_map(|item| {
            if let Item::Struct(item) = item {
                Some(item)
            } else {
                None
            }
        })
        .filter(|item| item.ident == "SipDomainVerifier");
    let Some(item) = declarations.next() else {
        return false;
    };
    if declarations.next().is_some()
        || !item.generics.params.is_empty()
        || item.generics.where_clause.is_some()
        || item.attrs.iter().any(|attribute| {
            !attribute.path().is_ident("derive")
                || !attribute
                    .parse_args::<Path>()
                    .is_ok_and(|path| path.is_ident("Debug"))
        })
    {
        return false;
    }
    let Fields::Named(fields) = &item.fields else {
        return false;
    };
    if fields.named.len() != 1 {
        return false;
    }
    let Some(field) = fields.named.first() else {
        return false;
    };
    let Type::Path(kind) = &field.ty else {
        return false;
    };
    field.ident.as_ref().is_some_and(|name| name == "roots")
        && field.attrs.is_empty()
        && kind.qself.is_none()
        && exact_path(
            &kind.path,
            &["tokio_rustls", "rustls", "RootCertStore"],
            true,
        )
        && factory(file)
}

fn binding(pattern: &Pat, name: &str) -> bool {
    let Pat::Ident(pattern) = pattern else {
        return false;
    };
    pattern.ident == name
        && pattern.attrs.is_empty()
        && pattern.by_ref.is_none()
        && pattern.mutability.is_none()
        && pattern.subpat.is_none()
}

fn factory(file: &syn::File) -> bool {
    let mut functions = file
        .items
        .iter()
        .filter_map(|item| {
            if let Item::Fn(item) = item {
                Some(item)
            } else {
                None
            }
        })
        .filter(|item| item.sig.ident == "sip_client_configuration");
    let Some(function) = functions.next() else {
        return false;
    };
    if functions.next().is_some()
        || !function.attrs.is_empty()
        || !function.sig.generics.params.is_empty()
        || function.sig.generics.where_clause.is_some()
        || function.sig.inputs.len() != 1
    {
        return false;
    }
    let Some(FnArg::Typed(roots)) = function.sig.inputs.first() else {
        return false;
    };
    let Type::Path(kind) = roots.ty.as_ref() else {
        return false;
    };
    if !roots.attrs.is_empty()
        || !binding(&roots.pat, "roots")
        || kind.qself.is_some()
        || !exact_path(
            &kind.path,
            &["tokio_rustls", "rustls", "RootCertStore"],
            true,
        )
    {
        return false;
    }
    let [Stmt::Local(value), Stmt::Local(shared), Stmt::Expr(_, None)] =
        function.block.stmts.as_slice()
    else {
        return false;
    };
    if !value.attrs.is_empty()
        || !binding(&value.pat, "verifier")
        || !binding(&shared.pat, "verifier")
        || !constructor(shared)
    {
        return false;
    }
    let Some(initializer) = &value.init else {
        return false;
    };
    if initializer.diverge.is_some() {
        return false;
    }
    let Expr::Struct(value) = initializer.expr.as_ref() else {
        return false;
    };
    if !value.attrs.is_empty()
        || value.qself.is_some()
        || !value.path.is_ident("SipDomainVerifier")
        || value.rest.is_some()
        || value.fields.len() != 1
    {
        return false;
    }
    let Some(field) = value.fields.first() else {
        return false;
    };
    let Expr::Path(argument) = &field.expr else {
        return false;
    };
    matches!(&field.member, Member::Named(name) if name == "roots")
        && field.attrs.is_empty()
        && field.colon_token.is_none()
        && argument.attrs.is_empty()
        && argument.qself.is_none()
        && argument.path.is_ident("roots")
}

pub(super) fn constructor(local: &Local) -> bool {
    let Some(initializer) = &local.init else {
        return false;
    };
    if initializer.diverge.is_some() {
        return false;
    }
    let Expr::Call(call) = initializer.expr.as_ref() else {
        return false;
    };
    if call.args.len() != 1 || !call.attrs.is_empty() {
        return false;
    }
    let Some(Expr::Path(argument)) = call.args.first() else {
        return false;
    };
    if !argument.attrs.is_empty() || argument.qself.is_some() || !argument.path.is_ident("verifier")
    {
        return false;
    }
    let Expr::Path(function) = call.func.as_ref() else {
        return false;
    };
    if !function.attrs.is_empty()
        || function.qself.is_some()
        || function.path.leading_colon.is_none()
        || function.path.segments.len() != 4
    {
        return false;
    }
    let mut segments = function.path.segments.iter();
    for name in ["std", "sync"] {
        let Some(segment) = segments.next() else {
            return false;
        };
        if segment.ident != name || !matches!(segment.arguments, PathArguments::None) {
            return false;
        }
    }
    let Some(arc) = segments.next() else {
        return false;
    };
    if arc.ident != "Arc" {
        return false;
    }
    let PathArguments::AngleBracketed(arguments) = &arc.arguments else {
        return false;
    };
    let Some(new) = segments.next() else {
        return false;
    };
    if new.ident != "new"
        || !matches!(new.arguments, PathArguments::None)
        || arguments.colon2_token.is_none()
        || arguments.args.len() != 1
    {
        return false;
    }
    let Some(GenericArgument::Type(Type::Path(verifier))) = arguments.args.first() else {
        return false;
    };
    verifier.qself.is_none()
        && exact_path(
            &verifier.path,
            &["crate", "encryption", "sip_domain", "SipDomainVerifier"],
            false,
        )
}
