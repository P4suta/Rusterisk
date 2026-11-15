use syn::{Expr, GenericArgument, Local, Path, PathArguments, Type};

fn plain_path(path: &Path, names: &[&str]) -> bool {
    path.leading_colon.is_some()
        && path.segments.len() == names.len()
        && path.segments.iter().zip(names).all(|(segment, name)| {
            segment.ident == *name && matches!(segment.arguments, PathArguments::None)
        })
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
    if call.args.len() != 1 {
        return false;
    }
    let Some(Expr::Path(argument)) = call.args.first() else {
        return false;
    };
    if !argument.attrs.is_empty() || argument.qself.is_some() || !argument.path.is_ident("config") {
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
    {
        return false;
    }
    let mut types = arguments.args.iter();
    let Some(GenericArgument::Type(Type::Path(config))) = types.next() else {
        return false;
    };
    if types.next().is_some() {
        return false;
    }
    config.qself.is_none() && plain_path(&config.path, &["tokio_rustls", "rustls", "ClientConfig"])
}
