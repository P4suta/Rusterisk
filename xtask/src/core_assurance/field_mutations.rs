use super::AssuranceError;
use syn::visit::{self, Visit};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FieldMutation {
    TerminalState,
    ConnectionClaim,
}

impl FieldMutation {
    pub const ALL: [Self; 2] = [Self::TerminalState, Self::ConnectionClaim];

    pub const fn label(self) -> &'static str {
        match self {
            Self::TerminalState => "terminal-state",
            Self::ConnectionClaim => "connection-claim",
        }
    }

    pub const fn original(self) -> &'static str {
        match self {
            Self::TerminalState => "self.state = CallState::Ended;",
            Self::ConnectionClaim => "self.connection_claim = ConnectionClaim::Claimed;",
        }
    }

    pub fn apply(self, source: &str) -> Result<String, AssuranceError> {
        if source.len() > 1024 * 1024 || source.matches(self.original()).count() != 1 {
            return Err(AssuranceError::Inventory);
        }
        let file = syn::parse_file(source).map_err(AssuranceError::Syntax)?;
        let (method, field, variant, replacement) = match self {
            Self::TerminalState => (
                "apply",
                "state",
                ["CallState", "Ended"],
                "self.state = CallState::Up;",
            ),
            Self::ConnectionClaim => (
                "claim_connection",
                "connection_claim",
                ["ConnectionClaim", "Claimed"],
                "self.connection_claim = ConnectionClaim::Available;",
            ),
        };
        let mut methods = 0;
        let mut assignments = Assignments {
            field,
            variant,
            count: 0,
        };
        for item in &file.items {
            if let syn::Item::Impl(item) = item {
                if item.trait_.is_some()
                    || !matches!(item.self_ty.as_ref(), syn::Type::Path(path) if path.qself.is_none() && path.path.is_ident("Call"))
                {
                    continue;
                }
                for item in &item.items {
                    if let syn::ImplItem::Fn(function) = item {
                        if function.sig.ident == method {
                            methods += 1;
                            assignments.visit_block(&function.block);
                        }
                    }
                }
            }
        }
        if methods != 1 || assignments.count != 1 {
            return Err(AssuranceError::Inventory);
        }
        Ok(source.replacen(self.original(), replacement, 1))
    }
}

struct Assignments<'a> {
    field: &'a str,
    variant: [&'a str; 2],
    count: usize,
}

impl<'ast> Visit<'ast> for Assignments<'_> {
    fn visit_expr_assign(&mut self, assignment: &'ast syn::ExprAssign) {
        let field = matches!(assignment.left.as_ref(), syn::Expr::Field(field)
            if matches!(&field.member, syn::Member::Named(name) if name == self.field)
                && matches!(field.base.as_ref(), syn::Expr::Path(path) if path.qself.is_none() && path.path.is_ident("self")));
        let value = matches!(assignment.right.as_ref(), syn::Expr::Path(path)
            if path.qself.is_none() && path.path.leading_colon.is_none() && path.path.segments.len() == 2
                && path.path.segments.iter().zip(self.variant).all(|(segment, name)| segment.ident == name && matches!(segment.arguments, syn::PathArguments::None)));
        if field && value {
            self.count += 1;
        }
        visit::visit_expr_assign(self, assignment);
    }
}
