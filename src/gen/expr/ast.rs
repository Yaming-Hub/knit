//! Abstract syntax tree for the expression language.

use std::fmt;

/// A parsed expression node.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    /// Literal constant value.
    Literal(LiteralValue),
    /// Reference to a sibling field: `${field_name}`.
    FieldRef(String),
    /// Reference to a user parameter: `${param.key}`.
    ParamRef(String),
    /// Binary operation: `left op right`.
    BinaryOp {
        /// Left operand.
        left: Box<Expr>,
        /// Operator.
        op: BinOp,
        /// Right operand.
        right: Box<Expr>,
    },
    /// Unary operation: `op operand`.
    UnaryOp {
        /// Operator.
        op: UnOp,
        /// Operand.
        operand: Box<Expr>,
    },
    /// Function call: `name(arg1, arg2, ...)`.
    FuncCall {
        /// Function name (lowercase).
        name: String,
        /// Arguments.
        args: Vec<Expr>,
    },
}

/// A literal value in an expression.
#[derive(Debug, Clone, PartialEq)]
pub enum LiteralValue {
    /// Integer literal.
    Int(i64),
    /// Floating-point literal.
    Float(f64),
    /// String literal (double-quoted).
    Str(String),
    /// Boolean literal.
    Bool(bool),
    /// Null literal.
    Null,
}

/// Binary operator kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    // Arithmetic
    /// `+`
    Add,
    /// `-`
    Sub,
    /// `*`
    Mul,
    /// `/`
    Div,
    /// `%`
    Mod,

    // Comparison
    /// `==`
    Eq,
    /// `!=`
    Ne,
    /// `<`
    Lt,
    /// `>`
    Gt,
    /// `<=`
    Le,
    /// `>=`
    Ge,

    // Logical
    /// `&&`
    And,
    /// `||`
    Or,
}

/// Unary operator kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    /// Arithmetic negation `-`.
    Neg,
    /// Logical negation `!`.
    Not,
}

/// Inferred expression result type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExprType {
    /// 64-bit integer.
    Int,
    /// 64-bit floating point.
    Float,
    /// UTF-8 string.
    Str,
    /// Boolean.
    Bool,
    /// Unknown / depends on runtime input.
    Unknown,
}

impl BinOp {
    /// Binding power (precedence) for Pratt parsing.
    /// Returns `(left_bp, right_bp)` — left-associative when `right_bp > left_bp`.
    /// Note: pipe operator `|>` has binding power (1, 2) and is handled separately in the parser.
    pub fn binding_power(self) -> (u8, u8) {
        match self {
            BinOp::Or => (3, 4),
            BinOp::And => (5, 6),
            BinOp::Eq | BinOp::Ne | BinOp::Lt | BinOp::Gt | BinOp::Le | BinOp::Ge => (7, 8),
            BinOp::Add | BinOp::Sub => (9, 10),
            BinOp::Mul | BinOp::Div | BinOp::Mod => (11, 12),
        }
    }
}

impl fmt::Display for BinOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BinOp::Add => write!(f, "+"),
            BinOp::Sub => write!(f, "-"),
            BinOp::Mul => write!(f, "*"),
            BinOp::Div => write!(f, "/"),
            BinOp::Mod => write!(f, "%"),
            BinOp::Eq => write!(f, "=="),
            BinOp::Ne => write!(f, "!="),
            BinOp::Lt => write!(f, "<"),
            BinOp::Gt => write!(f, ">"),
            BinOp::Le => write!(f, "<="),
            BinOp::Ge => write!(f, ">="),
            BinOp::And => write!(f, "&&"),
            BinOp::Or => write!(f, "||"),
        }
    }
}

impl fmt::Display for UnOp {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UnOp::Neg => write!(f, "-"),
            UnOp::Not => write!(f, "!"),
        }
    }
}

impl fmt::Display for LiteralValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LiteralValue::Int(v) => write!(f, "{v}"),
            LiteralValue::Float(v) => write!(f, "{v}"),
            LiteralValue::Str(v) => write!(f, "\"{v}\""),
            LiteralValue::Bool(v) => write!(f, "{v}"),
            LiteralValue::Null => write!(f, "null"),
        }
    }
}

impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Expr::Literal(v) => write!(f, "{v}"),
            Expr::FieldRef(name) => write!(f, "${{{name}}}"),
            Expr::ParamRef(key) => write!(f, "${{param.{key}}}"),
            Expr::BinaryOp { left, op, right } => write!(f, "({left} {op} {right})"),
            Expr::UnaryOp { op, operand } => write!(f, "({op}{operand})"),
            Expr::FuncCall { name, args } => {
                write!(f, "{name}(")?;
                for (i, arg) in args.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{arg}")?;
                }
                write!(f, ")")
            }
        }
    }
}

/// Extract all field references from an expression tree.
pub fn extract_field_refs(expr: &Expr) -> Vec<String> {
    let mut refs = Vec::new();
    collect_field_refs(expr, &mut refs);
    refs.sort();
    refs.dedup();
    refs
}

fn collect_field_refs(expr: &Expr, refs: &mut Vec<String>) {
    match expr {
        Expr::FieldRef(name) => refs.push(name.clone()),
        Expr::BinaryOp { left, right, .. } => {
            collect_field_refs(left, refs);
            collect_field_refs(right, refs);
        }
        Expr::UnaryOp { operand, .. } => collect_field_refs(operand, refs),
        Expr::FuncCall { args, .. } => {
            for arg in args {
                collect_field_refs(arg, refs);
            }
        }
        Expr::Literal(_) | Expr::ParamRef(_) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_roundtrip() {
        let expr = Expr::BinaryOp {
            left: Box::new(Expr::FieldRef("price".into())),
            op: BinOp::Mul,
            right: Box::new(Expr::FieldRef("quantity".into())),
        };
        assert_eq!(format!("{expr}"), "(${price} * ${quantity})");
    }

    #[test]
    fn extract_refs() {
        let expr = Expr::FuncCall {
            name: "round".into(),
            args: vec![
                Expr::BinaryOp {
                    left: Box::new(Expr::FieldRef("price".into())),
                    op: BinOp::Mul,
                    right: Box::new(Expr::FieldRef("qty".into())),
                },
                Expr::Literal(LiteralValue::Int(2)),
            ],
        };
        assert_eq!(extract_field_refs(&expr), vec!["price", "qty"]);
    }

    #[test]
    fn binding_power_ordering() {
        assert!(BinOp::Mul.binding_power().0 > BinOp::Add.binding_power().0);
        assert!(BinOp::Add.binding_power().0 > BinOp::Eq.binding_power().0);
        assert!(BinOp::Eq.binding_power().0 > BinOp::And.binding_power().0);
        assert!(BinOp::And.binding_power().0 > BinOp::Or.binding_power().0);
    }

    #[test]
    fn extract_field_refs_deduplicates() {
        let expr = Expr::BinaryOp {
            left: Box::new(Expr::FieldRef("price".into())),
            op: BinOp::Add,
            right: Box::new(Expr::FuncCall {
                name: "coalesce".into(),
                args: vec![
                    Expr::FieldRef("price".into()),
                    Expr::FieldRef("price".into()),
                ],
            }),
        };

        assert_eq!(extract_field_refs(&expr), vec!["price"]);
    }

    #[test]
    fn binding_power_mul_gt_add() {
        let (mul_left, mul_right) = BinOp::Mul.binding_power();
        let (add_left, add_right) = BinOp::Add.binding_power();

        assert!(mul_left > add_left);
        assert!(mul_right > add_right);
    }

    #[test]
    fn literal_null_display() {
        assert_eq!(LiteralValue::Null.to_string(), "null");
    }
}
