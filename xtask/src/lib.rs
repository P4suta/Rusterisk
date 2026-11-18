use syn::parse::Parser;
use syn::visit::{self, Visit};
pub mod browser;
pub mod core_assurance;
pub mod core_miri;
pub mod core_mutation;
pub mod core_proofs;
pub mod distribution;
pub mod input_fuzz;
pub mod legacy_audit;
pub mod native_sanitizers;
pub mod process;
mod recorded_path;
#[cfg(test)]
mod recorded_path_tests;
pub mod reference;
pub mod regular_archive;
pub mod release_check;
pub mod server_distribution;
pub const WORKSPACE_FORMAT_CHECK: &[&str] = &["fmt", "--", "--check"];
mod sip_domain_policy;
mod tls_client_policy;
mod unsafe_policy;
mod windows_job_accounting_policy;
pub use windows_job_accounting_policy::{
    inspect_windows_job_accounting_manifest, inspect_windows_job_accounting_source,
};

#[derive(Debug)]
pub enum SecurityConfigError {
    Parse(toml::de::Error),
    MissingAdvisories,
}

impl From<toml::de::Error> for SecurityConfigError {
    fn from(error: toml::de::Error) -> Self {
        Self::Parse(error)
    }
}

impl std::fmt::Display for SecurityConfigError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Parse(error) => write!(f, "{error}"),
            Self::MissingAdvisories => f.write_str("advisory policy table is required"),
        }
    }
}
impl std::error::Error for SecurityConfigError {}

pub fn advisory_snapshot_config(
    source: &str,
    db_path: &str,
) -> Result<toml::Value, SecurityConfigError> {
    let mut config: toml::Value = toml::from_str(source)?;
    let advisories = config
        .get_mut("advisories")
        .and_then(toml::Value::as_table_mut)
        .ok_or(SecurityConfigError::MissingAdvisories)?;
    advisories.insert("db-path".into(), toml::Value::String(db_path.into()));
    Ok(config)
}

#[derive(Debug, PartialEq, Eq)]
pub struct Finding {
    pub rule: &'static str,
    pub detail: String,
}

pub fn inspect(source: &str) -> Result<Vec<Finding>, syn::Error> {
    inspect_with_exceptions(source, &[])
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Exception {
    pub path: String,
    pub item: String,
    pub lint: String,
    pub adr: String,
    pub rationale: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Exceptions {
    pub exceptions: Vec<Exception>,
}

pub fn inspect_with_exceptions(
    source: &str,
    exceptions: &[Exception],
) -> Result<Vec<Finding>, syn::Error> {
    let file = syn::parse_file(source)?;
    let mut policy = Policy {
        findings: Vec::new(),
        exceptions,
        used: std::collections::BTreeSet::new(),
        function: None,
        scope_depth: 0,
        block_depth: 0,
        sip_domain_verifier: sip_domain_policy::verifier(&file),
        accounting_query: unsafe_policy::accounting_query(&file, exceptions)?,
    };
    policy.visit_file(&file);
    for (index, exception) in exceptions.iter().enumerate() {
        if !policy.used.contains(&index) {
            policy.reject(
                "unused-exception",
                format!("{}: {}", exception.item, exception.lint),
            );
        }
    }
    Ok(policy.findings)
}

struct Policy<'a> {
    findings: Vec<Finding>,
    exceptions: &'a [Exception],
    used: std::collections::BTreeSet<usize>,
    function: Option<String>,
    scope_depth: usize,
    block_depth: usize,
    sip_domain_verifier: bool,
    accounting_query: Option<&'a syn::Local>,
}

impl Policy<'_> {
    fn reject(&mut self, rule: &'static str, detail: impl Into<String>) {
        self.findings.push(Finding {
            rule,
            detail: detail.into(),
        });
    }

    fn name(&mut self, name: &str) {
        match name {
            "Arc" | "Rc" | "RefCell" | "Mutex" | "RwLock" => self.reject("shared-ownership", name),
            "Box" => self.reject("heap-erasure", name),
            "unbounded_channel" | "UnboundedSender" | "UnboundedReceiver" => {
                self.reject("unbounded-queue", name)
            }
            "async_trait" => self.reject("async-trait", name),
            _ => {}
        }
    }

    fn unsafe_syntax(&mut self, syntax: &unsafe_policy::Syntax) {
        if syntax.unsafe_code {
            self.reject(
                "unsafe-code",
                "only the registered fixed Job accounting query is permitted",
            );
        }
        if syntax.suppression {
            self.reject(
                "lint-suppression",
                "exceptions must be registered with an ADR and a precise source location",
            );
        }
    }

    fn approved(&mut self, attribute: &syn::Attribute, item: &str) -> bool {
        if !attribute.path().is_ident("expect") || !matches!(attribute.style, syn::AttrStyle::Outer)
        {
            return false;
        }
        let mut lint = String::new();
        let mut reason = String::new();
        let result = attribute.parse_nested_meta(|meta| {
            if meta.path.is_ident("reason") {
                reason = meta.value()?.parse::<syn::LitStr>()?.value();
            } else {
                if !lint.is_empty() {
                    return Err(meta.error("one lint per exception is required"));
                }
                lint = meta
                    .path
                    .segments
                    .iter()
                    .map(|segment| segment.ident.to_string())
                    .collect::<Vec<_>>()
                    .join("::");
            }
            Ok(())
        });
        if result.is_err() {
            return false;
        }
        if lint == "unsafe_code" || lint == "r#unsafe_code" {
            return false;
        }
        for (index, exception) in self.exceptions.iter().enumerate() {
            if exception.item == item
                && exception.lint == lint
                && reason == format!("{}: {}", exception.adr, exception.rationale)
                && !self.used.contains(&index)
            {
                self.used.insert(index);
                return true;
            }
        }
        false
    }
}

impl<'ast> Visit<'ast> for Policy<'_> {
    fn visit_item_extern_crate(&mut self, node: &'ast syn::ItemExternCrate) {
        if node.rename.is_some() || node.ident == "self" {
            self.reject(
                "crate-alias",
                "preserve dependency identities at checked API boundaries",
            );
        }
        visit::visit_item_extern_crate(self, node);
    }

    fn visit_item_fn(&mut self, node: &'ast syn::ItemFn) {
        let function = if self.scope_depth == 0 {
            Some(node.sig.ident.to_string())
        } else {
            None
        };
        let previous = std::mem::replace(&mut self.function, function);
        self.scope_depth += 1;
        visit::visit_item_fn(self, node);
        self.scope_depth -= 1;
        self.function = previous;
    }

    fn visit_item_mod(&mut self, node: &'ast syn::ItemMod) {
        self.scope_depth += 1;
        visit::visit_item_mod(self, node);
        self.scope_depth -= 1;
    }

    fn visit_item_impl(&mut self, node: &'ast syn::ItemImpl) {
        if node.unsafety.is_some() {
            self.reject("unsafe-code", "unsafe implementations are prohibited");
        }
        self.scope_depth += 1;
        visit::visit_item_impl(self, node);
        self.scope_depth -= 1;
    }

    fn visit_item_trait(&mut self, node: &'ast syn::ItemTrait) {
        if node.unsafety.is_some() {
            self.reject("unsafe-code", "unsafe traits are prohibited");
        }
        self.scope_depth += 1;
        visit::visit_item_trait(self, node);
        self.scope_depth -= 1;
    }

    fn visit_block(&mut self, node: &'ast syn::Block) {
        self.block_depth += 1;
        visit::visit_block(self, node);
        self.block_depth -= 1;
    }

    fn visit_local(&mut self, node: &'ast syn::Local) {
        if self
            .accounting_query
            .is_some_and(|query| std::ptr::eq(query, node))
        {
            if let Some(index) = self.exceptions.iter().position(unsafe_policy::registration) {
                self.used.insert(index);
                for attribute in &node.attrs {
                    visit::visit_attribute(self, attribute);
                }
                self.visit_pat(&node.pat);
                return;
            }
        }
        if self.block_depth == 1 {
            if let (Some(function), [attribute]) = (self.function.clone(), node.attrs.as_slice()) {
                let registered = self.exceptions.iter().any(|exception| {
                    exception.item == function
                        && (tls_client_policy::constructor(node)
                            && tls_client_policy::registration(exception)
                            || self.sip_domain_verifier
                                && sip_domain_policy::constructor(node)
                                && sip_domain_policy::registration(exception))
                });
                if registered && self.approved(attribute, &function) {
                    visit::visit_attribute(self, attribute);
                    self.visit_pat(&node.pat);
                    if let Some(initializer) = &node.init {
                        if let syn::Expr::Call(call) = initializer.expr.as_ref() {
                            for attribute in &call.attrs {
                                self.visit_attribute(attribute);
                            }
                            for argument in &call.args {
                                self.visit_expr(argument);
                            }
                        }
                    }
                    return;
                }
            }
        }
        visit::visit_local(self, node);
    }

    fn visit_item_enum(&mut self, node: &'ast syn::ItemEnum) {
        for attribute in &node.attrs {
            if self.approved(attribute, &node.ident.to_string()) {
                visit::visit_attribute(self, attribute);
            } else {
                self.visit_attribute(attribute);
            }
        }
        self.visit_visibility(&node.vis);
        self.visit_generics(&node.generics);
        for variant in &node.variants {
            self.visit_variant(variant);
        }
    }
    fn visit_type_trait_object(&mut self, node: &'ast syn::TypeTraitObject) {
        self.reject(
            "dynamic-dispatch",
            "use a concrete type, generic parameter, or closed enum",
        );
        visit::visit_type_trait_object(self, node);
    }

    fn visit_path(&mut self, node: &'ast syn::Path) {
        for segment in &node.segments {
            self.name(&segment.ident.to_string());
        }
        visit::visit_path(self, node);
    }

    fn visit_use_name(&mut self, node: &'ast syn::UseName) {
        self.name(&node.ident.to_string());
        visit::visit_use_name(self, node);
    }

    fn visit_item_use(&mut self, node: &'ast syn::ItemUse) {
        if unsafe_policy::assembly_import(&node.tree) {
            self.reject("unsafe-code", "assembly imports are prohibited");
        }
        visit::visit_item_use(self, node);
    }

    fn visit_use_rename(&mut self, node: &'ast syn::UseRename) {
        self.name(&node.ident.to_string());
        visit::visit_use_rename(self, node);
    }

    fn visit_attribute(&mut self, node: &'ast syn::Attribute) {
        self.unsafe_syntax(&unsafe_policy::attribute(&node.meta));
        visit::visit_attribute(self, node);
    }

    fn visit_signature(&mut self, node: &'ast syn::Signature) {
        if node.unsafety.is_some() {
            self.reject("unsafe-code", "unsafe functions are prohibited");
        }
        visit::visit_signature(self, node);
    }

    fn visit_expr_unsafe(&mut self, node: &'ast syn::ExprUnsafe) {
        self.reject(
            "unsafe-code",
            "only the registered fixed Job accounting query is permitted",
        );
        visit::visit_expr_unsafe(self, node);
    }

    fn visit_item_foreign_mod(&mut self, node: &'ast syn::ItemForeignMod) {
        self.reject("unsafe-code", "foreign blocks are prohibited");
        visit::visit_item_foreign_mod(self, node);
    }

    fn visit_macro(&mut self, node: &'ast syn::Macro) {
        if unsafe_policy::assembly_macro(&node.path) {
            self.reject("unsafe-code", "assembly macros are prohibited");
        }
        self.unsafe_syntax(&unsafe_policy::macro_tokens(node));
        if node.path.is_ident("vec") {
            let parser = syn::punctuated::Punctuated::<syn::Expr, syn::Token![,]>::parse_terminated;
            if let Ok(elements) = parser.parse2(node.tokens.clone()) {
                if elements.len() == 1 {
                    self.reject(
                        "single-element-vector",
                        "use the concrete owned value or a bounded collection",
                    );
                }
            }
        }
        visit::visit_macro(self, node);
    }
}
pub mod vendor;
