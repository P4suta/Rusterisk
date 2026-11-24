use crate::Finding;

use proc_macro2::{Delimiter, TokenStream, TokenTree};
use std::collections::BTreeSet;
use syn::punctuated::Punctuated;
use syn::visit::{self, Visit};

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Timer {
    pub path: String,
    pub item: String,
    pub adr: String,
    pub rationale: String,
}

/// Whether the rule covers a source file: tests, examples, and benchmarks pace themselves freely.
pub fn production(path: &std::path::Path) -> bool {
    let outside = path.components().any(|component| {
        matches!(
            component.as_os_str().to_str(),
            Some("tests" | "examples" | "benches")
        )
    });
    let test_module = path
        .file_stem()
        .and_then(std::ffi::OsStr::to_str)
        .is_some_and(|stem| stem == "tests" || stem.ends_with("_tests"));
    !outside && !test_module
}

/// Finds each periodic timer outside the registered functions of one production source file.
pub fn inspect(source: &str, timers: &[Timer]) -> Result<Vec<Finding>, syn::Error> {
    let file = syn::parse_file(source)?;
    let mut policy = Policy {
        findings: Vec::new(),
        timers,
        used: BTreeSet::new(),
        function: None,
    };
    policy.visit_file(&file);
    for (index, timer) in timers.iter().enumerate() {
        if !policy.used.contains(&index) {
            policy.findings.push(Finding {
                rule: "unused-timer",
                detail: timer.item.clone(),
            });
        }
    }
    Ok(policy.findings)
}

struct Policy<'a> {
    findings: Vec<Finding>,
    timers: &'a [Timer],
    used: BTreeSet<usize>,
    function: Option<String>,
}

const HIDDEN: &str = "a function value hides a periodic timer";

fn periodic(name: &syn::Ident) -> bool {
    name == "interval" || name == "interval_at"
}

// A constructor is named through a path, because the policy rejects every import of its bare name.
fn constructor(path: &syn::Path) -> bool {
    path.segments.len() > 1
        && path
            .segments
            .last()
            .is_some_and(|segment| periodic(&segment.ident))
}

// Whether a condition holds only in a test build.
fn requires_test(condition: &syn::Meta) -> bool {
    let conditions = |list: &syn::MetaList| {
        list.parse_args_with(Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated)
    };
    match condition {
        syn::Meta::Path(path) => path.is_ident("test"),
        syn::Meta::List(list) if list.path.is_ident("all") => {
            conditions(list).is_ok_and(|all| all.iter().any(requires_test))
        }
        syn::Meta::List(list) if list.path.is_ident("any") => {
            conditions(list).is_ok_and(|any| !any.is_empty() && any.iter().all(requires_test))
        }
        syn::Meta::List(_) | syn::Meta::NameValue(_) => false,
    }
}

fn test_only(attributes: &[syn::Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        attribute.path().is_ident("cfg")
            && attribute
                .parse_args::<syn::Meta>()
                .is_ok_and(|condition| requires_test(&condition))
    })
}

fn globbed(tree: &syn::UseTree) -> bool {
    match tree {
        syn::UseTree::Glob(_) => true,
        syn::UseTree::Group(group) => group.items.iter().any(globbed),
        syn::UseTree::Path(_) | syn::UseTree::Name(_) | syn::UseTree::Rename(_) => false,
    }
}

impl Policy<'_> {
    fn reject(&mut self, detail: &str) {
        self.findings.push(Finding {
            rule: "periodic-timer",
            detail: detail.into(),
        });
    }

    fn timer(&mut self) {
        let function = self.function.clone().unwrap_or_default();
        let registered = self
            .timers
            .iter()
            .enumerate()
            .find(|(index, timer)| timer.item == function && !self.used.contains(index))
            .map(|(index, _)| index);
        match registered {
            Some(index) => {
                self.used.insert(index);
            }
            None => self.reject(&format!(
                "{function}: wake for an event or the earliest deadline, or register the timer"
            )),
        }
    }

    // A macro body stays unparsed, so this walks its tokens for the same references.
    fn tokens(&mut self, stream: TokenStream) {
        let mut colons = 0_u8;
        let mut trees = stream.into_iter().peekable();
        while let Some(tree) = trees.next() {
            match tree {
                TokenTree::Ident(name) => {
                    let called = matches!(
                        trees.peek(),
                        Some(TokenTree::Group(group)) if group.delimiter() == Delimiter::Parenthesis
                    );
                    if periodic(&name) && colons == 2 {
                        if called {
                            self.timer();
                        } else {
                            self.reject(HIDDEN);
                        }
                    }
                    colons = 0;
                }
                TokenTree::Punct(punct) => {
                    colons = if punct.as_char() == ':' {
                        colons.saturating_add(1).min(2)
                    } else {
                        0
                    };
                }
                TokenTree::Group(group) => {
                    self.tokens(group.stream());
                    colons = 0;
                }
                TokenTree::Literal(_) => colons = 0,
            }
        }
    }

    fn within(&mut self, name: &syn::Ident, visit: impl FnOnce(&mut Self)) {
        let previous = self.function.replace(name.to_string());
        visit(self);
        self.function = previous;
    }
}

impl<'ast> Visit<'ast> for Policy<'_> {
    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        if !test_only(&node.attrs) {
            visit::visit_item_mod(self, node);
        }
    }

    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        if !test_only(&node.attrs) {
            self.within(&node.sig.ident, |policy| visit::visit_item_fn(policy, node));
        }
    }

    fn visit_impl_item_fn(&mut self, node: &'ast syn::ImplItemFn) {
        if !test_only(&node.attrs) {
            self.within(&node.sig.ident, |policy| {
                visit::visit_impl_item_fn(policy, node);
            });
        }
    }

    // A constructor outside call position is a function value, whose calls no registration counts.
    fn visit_expr_path(&mut self, node: &'ast syn::ExprPath) {
        if constructor(&node.path) {
            self.reject(HIDDEN);
        }
        visit::visit_expr_path(self, node);
    }

    // A constructor in call position is one timer.
    fn visit_expr_call(&mut self, node: &'ast syn::ExprCall) {
        let constructor =
            matches!(&*node.func, syn::Expr::Path(function) if constructor(&function.path));
        if constructor {
            self.timer();
            for argument in &node.args {
                self.visit_expr(argument);
            }
        } else {
            visit::visit_expr_call(self, node);
        }
    }

    fn visit_item_use(&mut self, node: &'ast syn::ItemUse) {
        if !test_only(&node.attrs) {
            visit::visit_item_use(self, node);
        }
    }

    fn visit_use_name(&mut self, node: &'ast syn::UseName) {
        if periodic(&node.ident) {
            self.reject("an import hides a periodic timer");
        }
    }

    fn visit_use_rename(&mut self, node: &'ast syn::UseRename) {
        if periodic(&node.ident) {
            self.reject("an alias hides a periodic timer");
        }
        visit::visit_use_rename(self, node);
    }

    fn visit_use_path(&mut self, node: &'ast syn::UsePath) {
        if node.ident == "time" && globbed(&node.tree) {
            self.reject("a glob import hides a periodic timer");
        }
        visit::visit_use_path(self, node);
    }

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        self.tokens(node.tokens.clone());
        visit::visit_macro(self, node);
    }
}
