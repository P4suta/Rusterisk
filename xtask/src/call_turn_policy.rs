use crate::Finding;
use std::path::Path;
use syn::visit::{self, Visit};
use syn::{Expr, GenericArgument, Member, PathArguments, Type};

pub fn inspect_call_turn(path: &Path, source: &str) -> Result<Vec<Finding>, syn::Error> {
    let Some(file) = path.strip_prefix("crates/server/src").ok() else {
        return Ok(Vec::new());
    };
    let worker = ["runner", "signaling", "control"]
        .iter()
        .any(|name| file.starts_with(name) || file == Path::new(name).with_extension("rs"));
    let test_directory = file.parent().is_some_and(|directory| {
        directory.components().any(|part| {
            part.as_os_str()
                .to_str()
                .is_some_and(|name| name.ends_with("_tests"))
        })
    });
    if !worker || test_directory || !crate::timer_policy::production(path) {
        return Ok(Vec::new());
    }
    let mut policy = Policy(Vec::new());
    policy.visit_file(&syn::parse_file(source)?);
    Ok(policy.0)
}

struct Policy(Vec<Finding>);

fn collection_key(field: &syn::Field) -> Option<&syn::Ident> {
    let Type::Path(ty) = &field.ty else {
        return None;
    };
    let collection = ty.path.segments.last()?;
    if !matches!(
        collection.ident.to_string().as_str(),
        "BTreeMap" | "HashMap" | "BTreeSet" | "HashSet"
    ) {
        return None;
    }
    let PathArguments::AngleBracketed(args) = &collection.arguments else {
        return None;
    };
    let GenericArgument::Type(Type::Path(key)) = args.args.first()? else {
        return None;
    };
    Some(&key.path.segments.last()?.ident)
}

impl<'ast> Visit<'ast> for Policy {
    fn visit_item_mod(&mut self, module: &'ast syn::ItemMod) {
        if !module.attrs.iter().any(|attr| {
            attr.path().is_ident("cfg")
                && attr
                    .parse_args::<syn::Ident>()
                    .is_ok_and(|name| name == "test")
        }) {
            visit::visit_item_mod(self, module);
        }
    }

    fn visit_field(&mut self, field: &'ast syn::Field) {
        if let (Some(name), Some(key)) = (&field.ident, collection_key(field)) {
            let call_state = matches!(
                name.to_string().as_str(),
                "calls"
                    | "channels"
                    | "pending"
                    | "answers"
                    | "media_leases"
                    | "call_lifetimes"
                    | "external_admissions"
                    | "external_media"
                    | "native_pending"
                    | "stream_admissions"
                    | "websocket_admissions"
            );
            if key == "CallKey" || call_state && (key == "String" || key == "ChannelId") {
                self.0.push(Finding {
                    rule: "call-slot-key",
                    detail: name.to_string(),
                });
            }
        }
        visit::visit_field(self, field);
    }

    fn visit_expr_method_call(&mut self, call: &'ast syn::ExprMethodCall) {
        if matches!(
            call.method.to_string().as_str(),
            "get" | "get_mut" | "remove" | "contains_key"
        ) {
            let channels = matches!(&*call.receiver, Expr::Field(receiver) if matches!(&receiver.member, Member::Named(field) if field == "channels"));
            let event_name = matches!(call.args.first(), Some(Expr::Reference(argument)) if matches!(&*argument.expr, Expr::Field(field) if matches!(&field.member, Member::Named(field) if field == "id")));
            if channels && event_name {
                self.0.push(Finding {
                    rule: "call-slot-lookup",
                    detail: call.method.to_string(),
                });
            }
        }
        visit::visit_expr_method_call(self, call);
    }
}
