use crate::Finding;

use std::collections::BTreeSet;
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

fn periodic(name: &syn::Ident) -> bool {
    name == "interval" || name == "interval_at"
}

fn test_only(attributes: &[syn::Attribute]) -> bool {
    attributes.iter().any(|attribute| {
        attribute.path().is_ident("cfg")
            && attribute
                .parse_args::<syn::Path>()
                .is_ok_and(|path| path.is_ident("test"))
    })
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

    // A macro body stays unparsed, so this scans its rendered tokens for a call by name.
    fn tokens(&mut self, rendered: &str) {
        let word = |character: char| character.is_alphanumeric() || character == '_';
        let mut rest = rendered;
        while let Some(found) = rest.find("interval") {
            let (before, name) = rest.split_at(found);
            let after = &name["interval".len()..];
            let after = after.strip_prefix("_at").unwrap_or(after);
            let starts_a_word = !before.chars().next_back().is_some_and(word);
            if starts_a_word && after.trim_start().starts_with('(') {
                self.timer();
            }
            rest = after;
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

    fn visit_expr_call(&mut self, node: &'ast syn::ExprCall) {
        if let syn::Expr::Path(function) = &*node.func {
            if function
                .path
                .segments
                .last()
                .is_some_and(|segment| periodic(&segment.ident))
            {
                self.timer();
            }
        }
        visit::visit_expr_call(self, node);
    }

    fn visit_use_rename(&mut self, node: &'ast syn::UseRename) {
        if periodic(&node.ident) {
            self.reject("an alias hides a periodic timer");
        }
        visit::visit_use_rename(self, node);
    }

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        self.tokens(&node.tokens.to_string());
        visit::visit_macro(self, node);
    }
}
