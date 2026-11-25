use crate::Finding;
use proc_macro2::{TokenStream, TokenTree};
use std::{collections::BTreeMap, path::Path};
use syn::visit::{self, Visit};

fn timer(name: &str) -> bool {
    matches!(name, "sleep" | "sleep_until" | "timeout" | "timeout_at")
}

fn paused(attributes: &[syn::Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        if !attribute
            .path()
            .segments
            .last()
            .is_some_and(|segment| segment.ident == "test")
        {
            return false;
        }
        attribute.parse_args_with(
            syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated
        ).is_ok_and(|settings| settings.iter().any(|setting| {
            matches!(setting, syn::Meta::NameValue(pair)
                if pair.path.is_ident("start_paused")
                    && matches!(&pair.value, syn::Expr::Lit(value) if matches!(&value.lit, syn::Lit::Bool(flag) if flag.value())))
        }))
    })
}

struct Policy {
    test: bool,
    paused: bool,
    common: bool,
    shared_bounds: bool,
    shared_virtual: bool,
    wait_impl: bool,
    imports: BTreeMap<String, String>,
    findings: Vec<Finding>,
}

impl Policy {
    fn path(&mut self, names: &[String], called: bool) {
        if !self.test || self.common {
            return;
        }
        let Some(last) = names.last() else { return };
        let name = if names.len() == 1 {
            let Some(name) = self.imports.get(last) else {
                return;
            };
            name.as_str()
        } else {
            last
        };
        let parent = names.iter().rev().nth(1).map(String::as_str);
        let namespace = parent
            .and_then(|value| self.imports.get(value).map(String::as_str))
            .or(parent);
        let thread = namespace == Some("thread") || name == "thread::sleep";
        if !thread && !matches!(namespace, None | Some("time")) {
            return;
        }
        if called && name == "pause" {
            self.paused = true;
        } else if called && name == "resume" {
            self.paused = false;
        } else if matches!(name, "timeout" | "timeout_at")
            || (thread || !self.paused) && (timer(name) || name == "thread::sleep")
        {
            self.findings.push(Finding {
                rule: "test-time",
                detail: format!(
                    "{name}: wait for readiness with the shared bound or use a paused clock"
                ),
            });
        }
    }

    fn imports(&mut self, prefix: &str, tree: &syn::UseTree) {
        match tree {
            syn::UseTree::Path(path) => self.imports(&path.ident.to_string(), &path.tree),
            syn::UseTree::Group(group) => {
                for item in &group.items {
                    self.imports(prefix, item);
                }
            }
            syn::UseTree::Name(item) => self.import(prefix, &item.ident, &item.ident),
            syn::UseTree::Rename(item) => self.import(prefix, &item.ident, &item.rename),
            syn::UseTree::Glob(_) => {
                if matches!(prefix, "time" | "thread") {
                    for name in [
                        "sleep",
                        "sleep_until",
                        "timeout",
                        "timeout_at",
                        "pause",
                        "resume",
                    ] {
                        self.imports.insert(
                            name.to_owned(),
                            if prefix == "thread" {
                                format!("thread::{name}")
                            } else {
                                name.to_owned()
                            },
                        );
                    }
                }
            }
        }
    }

    fn import(&mut self, prefix: &str, original: &syn::Ident, alias: &syn::Ident) {
        let original = original.to_string();
        if matches!(original.as_str(), "time" | "thread")
            || matches!(prefix, "time" | "thread")
                && (timer(&original) || matches!(original.as_str(), "pause" | "resume"))
        {
            let name = if prefix == "thread" {
                format!("thread::{original}")
            } else {
                original
            };
            self.imports.insert(alias.to_string(), name);
        }
    }

    fn tokens(&mut self, stream: TokenStream) {
        let tokens: Vec<_> = stream.into_iter().collect();
        let mut names = Vec::new();
        for (index, token) in tokens.iter().enumerate() {
            match token {
                TokenTree::Ident(name) => {
                    names.push(name.to_string());
                    let separator = tokens.get(index + 1..index + 3).is_some_and(|pair| {
                        pair.iter()
                            .all(|part| matches!(part, TokenTree::Punct(p) if p.as_char() == ':'))
                    });
                    if !separator {
                        if !index
                            .checked_sub(1)
                            .and_then(|i| tokens.get(i))
                            .is_some_and(
                                |part| matches!(part, TokenTree::Punct(p) if p.as_char() == '.'),
                            )
                        {
                            self.path(&names, false);
                        }
                        names.clear();
                    }
                }
                TokenTree::Group(group) => {
                    names.clear();
                    self.tokens(group.stream());
                }
                TokenTree::Punct(punct) if punct.as_char() == ':' => {}
                TokenTree::Punct(_) | TokenTree::Literal(_) => names.clear(),
            }
        }
    }
}

impl<'ast> Visit<'ast> for Policy {
    fn visit_file(&mut self, file: &'ast syn::File) {
        for item in &file.items {
            if let syn::Item::Use(import) = item {
                self.imports("", &import.tree);
            }
        }
        visit::visit_file(self, file);
    }

    fn visit_block(&mut self, block: &'ast syn::Block) {
        let clock = self.paused;
        let imports = self.imports.clone();
        for stmt in &block.stmts {
            if let syn::Stmt::Item(syn::Item::Use(import)) = stmt {
                self.imports("", &import.tree);
            }
        }
        visit::visit_block(self, block);
        self.paused &= clock;
        self.imports = imports;
    }

    fn visit_item_impl(&mut self, item: &'ast syn::ItemImpl) {
        let (test, prior) = (self.test, self.wait_impl);
        self.test |= crate::timer_policy::test_only(&item.attrs);
        self.wait_impl =
            matches!(item.self_ty.as_ref(), syn::Type::Path(ty) if ty.path.is_ident("Wait"));
        visit::visit_item_impl(self, item);
        (self.test, self.wait_impl) = (test, prior);
    }

    fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
        if let syn::Expr::Path(path) = call.func.as_ref() {
            self.path(
                &path
                    .path
                    .segments
                    .iter()
                    .map(|s| s.ident.to_string())
                    .collect::<Vec<_>>(),
                true,
            );
            for expr in &call.args {
                self.visit_expr(expr);
            }
        } else {
            visit::visit_expr_call(self, call);
        }
    }

    fn visit_item_use(&mut self, item: &'ast syn::ItemUse) {
        self.imports("", &item.tree);
    }

    fn visit_item_mod(&mut self, item: &'ast syn::ItemMod) {
        let prior = self.test;
        let imports = self.imports.clone();
        if let Some((_, items)) = &item.content {
            for item in items {
                if let syn::Item::Use(import) = item {
                    self.imports("", &import.tree);
                }
            }
        }
        self.test |= crate::timer_policy::test_only(&item.attrs);
        visit::visit_item_mod(self, item);
        self.test = prior;
        self.imports = imports;
    }

    fn visit_item_fn(&mut self, item: &'ast syn::ItemFn) {
        let (test, clock, common) = (self.test, self.paused, self.common);
        self.test |= item.attrs.iter().any(|attr| {
            attr.path()
                .segments
                .last()
                .is_some_and(|segment| segment.ident == "test")
        }) || crate::timer_policy::test_only(&item.attrs);
        self.paused = paused(&item.attrs);
        self.common = self.shared_virtual && item.sig.ident == "virtual_window";
        visit::visit_item_fn(self, item);
        (self.test, self.paused, self.common) = (test, clock, common);
    }

    fn visit_impl_item_fn(&mut self, item: &'ast syn::ImplItemFn) {
        let (test, clock, common) = (self.test, self.paused, self.common);
        self.test |= crate::timer_policy::test_only(&item.attrs);
        self.paused = paused(&item.attrs);
        self.common = self.shared_bounds && self.wait_impl && item.sig.ident == "bounded";
        visit::visit_impl_item_fn(self, item);
        (self.test, self.paused, self.common) = (test, clock, common);
    }

    fn visit_expr_path(&mut self, expr: &'ast syn::ExprPath) {
        self.path(
            &expr
                .path
                .segments
                .iter()
                .map(|s| s.ident.to_string())
                .collect::<Vec<_>>(),
            false,
        );
        visit::visit_expr_path(self, expr);
    }

    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        self.tokens(mac.tokens.clone());
    }
}

pub fn inspect(path: &Path, source: &str) -> Result<Vec<Finding>, syn::Error> {
    let stem = path
        .file_stem()
        .and_then(|part| part.to_str())
        .unwrap_or_default();
    let mut policy = Policy {
        test: path.components().any(|part| {
            part.as_os_str()
                .to_str()
                .is_some_and(|name| name == "tests" || name.ends_with("_tests"))
        }) || path.starts_with("crates/test-support")
            || stem == "tests"
            || stem.ends_with("_tests"),
        paused: false,
        common: false,
        shared_bounds: path == Path::new("crates/cli/tests/support/time.rs"),
        shared_virtual: path == Path::new("crates/test-support/src/lib.rs"),
        wait_impl: false,
        imports: BTreeMap::new(),
        findings: Vec::new(),
    };
    policy.visit_file(&syn::parse_file(source)?);
    Ok(policy.findings)
}
