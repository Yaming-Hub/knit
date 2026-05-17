//! Pratt parser for expressions.
//!
//! Uses precedence-climbing to handle operator precedence and associativity.
//! Produces an [`Expr`] AST from a list of [`SpannedToken`]s.

use super::ast::{BinOp, Expr, LiteralValue, UnOp};
use super::lexer::{SpannedToken, Token};
use std::fmt;

/// Parse error.
#[derive(Debug, Clone)]
pub struct ParseError {
    /// Error message.
    pub message: String,
    /// Byte position in the original source.
    pub pos: usize,
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "parse error at position {}: {}", self.pos, self.message)
    }
}

impl std::error::Error for ParseError {}

/// Parser state.
struct Parser<'a> {
    tokens: &'a [SpannedToken],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(tokens: &'a [SpannedToken]) -> Self {
        Self { tokens, pos: 0 }
    }

    fn peek(&self) -> &Token {
        &self.tokens[self.pos].token
    }

    fn peek_span_pos(&self) -> usize {
        self.tokens[self.pos].span.start
    }

    fn advance(&mut self) -> &SpannedToken {
        let t = &self.tokens[self.pos];
        self.pos += 1;
        t
    }

    fn expect(&mut self, expected: &Token) -> Result<(), ParseError> {
        if self.peek() == expected {
            self.advance();
            Ok(())
        } else {
            Err(ParseError {
                message: format!("expected {expected:?}, found {:?}", self.peek()),
                pos: self.peek_span_pos(),
            })
        }
    }

    /// Parse a complete expression.
    fn parse_expr(&mut self, min_bp: u8) -> Result<Expr, ParseError> {
        let mut lhs = self.parse_atom()?;

        loop {
            // Pipe operator `|>` — lowest precedence, desugars to function call
            if *self.peek() == Token::PipeGt {
                let pipe_bp: u8 = 1; // left binding power (lowest)
                if pipe_bp < min_bp {
                    break;
                }
                let pipe_pos = self.peek_span_pos();
                self.advance();
                let rhs = self.parse_expr(pipe_bp + 1)?; // right_bp = 2, left-associative
                match rhs {
                    Expr::FuncCall { name, mut args } => {
                        args.insert(0, lhs);
                        lhs = Expr::FuncCall { name, args };
                    }
                    _ => {
                        return Err(ParseError {
                            message: "right side of `|>` must be a function call".to_string(),
                            pos: pipe_pos,
                        });
                    }
                }
                continue;
            }

            let op = match self.peek() {
                Token::Plus => BinOp::Add,
                Token::Minus => BinOp::Sub,
                Token::Star => BinOp::Mul,
                Token::Slash => BinOp::Div,
                Token::Percent => BinOp::Mod,
                Token::EqEq => BinOp::Eq,
                Token::BangEq => BinOp::Ne,
                Token::Lt => BinOp::Lt,
                Token::Gt => BinOp::Gt,
                Token::LtEq => BinOp::Le,
                Token::GtEq => BinOp::Ge,
                Token::AmpAmp => BinOp::And,
                Token::PipePipe => BinOp::Or,
                _ => break,
            };

            let (l_bp, r_bp) = op.binding_power();
            if l_bp < min_bp {
                break;
            }

            self.advance();
            let rhs = self.parse_expr(r_bp)?;
            lhs = Expr::BinaryOp {
                left: Box::new(lhs),
                op,
                right: Box::new(rhs),
            };
        }

        Ok(lhs)
    }

    /// Parse an atomic expression (literal, ref, unary, paren, function call).
    fn parse_atom(&mut self) -> Result<Expr, ParseError> {
        match self.peek().clone() {
            // Literals
            Token::Int(v) => {
                self.advance();
                Ok(Expr::Literal(LiteralValue::Int(v)))
            }
            Token::Float(v) => {
                self.advance();
                Ok(Expr::Literal(LiteralValue::Float(v)))
            }
            Token::Str(ref v) => {
                let v = v.clone();
                self.advance();
                Ok(Expr::Literal(LiteralValue::Str(v)))
            }
            Token::Bool(v) => {
                self.advance();
                Ok(Expr::Literal(LiteralValue::Bool(v)))
            }
            Token::Null => {
                self.advance();
                Ok(Expr::Literal(LiteralValue::Null))
            }

            // Field/param references
            Token::FieldRef(ref name) => {
                let name = name.clone();
                self.advance();
                Ok(Expr::FieldRef(name))
            }
            Token::ParamRef(ref key) => {
                let key = key.clone();
                self.advance();
                Ok(Expr::ParamRef(key))
            }

            // Unary operators
            Token::Minus => {
                self.advance();
                let operand = self.parse_atom()?;
                Ok(Expr::UnaryOp {
                    op: UnOp::Neg,
                    operand: Box::new(operand),
                })
            }
            Token::Bang => {
                self.advance();
                let operand = self.parse_atom()?;
                Ok(Expr::UnaryOp {
                    op: UnOp::Not,
                    operand: Box::new(operand),
                })
            }

            // Parenthesized expression
            Token::LParen => {
                self.advance();
                let expr = self.parse_expr(0)?;
                self.expect(&Token::RParen)?;
                Ok(expr)
            }

            // Identifier: function call or `if` expression
            Token::Ident(ref name) => {
                let name = name.clone();
                self.advance();

                // `if(cond, then, else)` — parsed as a function call
                if self.peek() == &Token::LParen {
                    self.advance();
                    let mut args = Vec::new();
                    if self.peek() != &Token::RParen {
                        args.push(self.parse_expr(0)?);
                        while self.peek() == &Token::Comma {
                            self.advance();
                            args.push(self.parse_expr(0)?);
                        }
                    }
                    self.expect(&Token::RParen)?;
                    Ok(Expr::FuncCall {
                        name: name.to_lowercase(),
                        args,
                    })
                } else {
                    // Bare identifier — error (must be function call)
                    Err(ParseError {
                        message: format!(
                            "unexpected identifier `{name}` — did you mean `{name}(...)`?"
                        ),
                        pos: self.peek_span_pos(),
                    })
                }
            }

            _ => Err(ParseError {
                message: format!("unexpected token: {:?}", self.peek()),
                pos: self.peek_span_pos(),
            }),
        }
    }
}

/// Parse an expression string into an AST.
///
/// # Errors
///
/// Returns a [`ParseError`] if the input contains syntax errors.
pub fn parse(input: &str) -> Result<Expr, ParseError> {
    let tokens = super::lexer::tokenize(input).map_err(|e| ParseError {
        message: e.message,
        pos: e.pos,
    })?;
    let mut parser = Parser::new(&tokens);
    let expr = parser.parse_expr(0)?;

    if parser.peek() != &Token::Eof {
        return Err(ParseError {
            message: format!("unexpected trailing token: {:?}", parser.peek()),
            pos: parser.peek_span_pos(),
        });
    }

    Ok(expr)
}

/// Check if an expression string is a legacy string template
/// (contains `${field}` but is not a valid expression).
///
/// Returns `true` for strings like `"Hello ${name}, your id is ${id}"`
/// that should be handled as string templates, not parsed expressions.
pub fn is_legacy_template(input: &str) -> bool {
    // Must contain at least one ${...} reference
    if !input.contains("${") {
        return false;
    }
    // Expressions containing |> are expression syntax, not templates.
    // This prevents pipe expressions from silently degrading to template mode
    // on parse failure. The trade-off is that literal "|>" in templates would
    // be misclassified, but that is not a realistic scenario.
    if input.contains("|>") {
        return false;
    }
    // Try to parse as expression — if parsing fails, it's a template
    parse(input).is_err()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::r#gen::expr::ast::{BinOp, LiteralValue, UnOp};

    #[test]
    fn simple_add() {
        let expr = parse("${a} + ${b}").unwrap();
        assert_eq!(
            expr,
            Expr::BinaryOp {
                left: Box::new(Expr::FieldRef("a".into())),
                op: BinOp::Add,
                right: Box::new(Expr::FieldRef("b".into())),
            }
        );
    }

    #[test]
    fn precedence_mul_over_add() {
        // a + b * c should parse as a + (b * c)
        let expr = parse("${a} + ${b} * ${c}").unwrap();
        match expr {
            Expr::BinaryOp {
                op: BinOp::Add,
                right,
                ..
            } => match *right {
                Expr::BinaryOp { op: BinOp::Mul, .. } => {}
                other => panic!("expected Mul, got {other:?}"),
            },
            other => panic!("expected Add, got {other:?}"),
        }
    }

    #[test]
    fn parens_override_precedence() {
        // (a + b) * c
        let expr = parse("(${a} + ${b}) * ${c}").unwrap();
        match expr {
            Expr::BinaryOp {
                op: BinOp::Mul,
                left,
                ..
            } => match *left {
                Expr::BinaryOp { op: BinOp::Add, .. } => {}
                other => panic!("expected Add, got {other:?}"),
            },
            other => panic!("expected Mul, got {other:?}"),
        }
    }

    #[test]
    fn function_call() {
        let expr = parse("round(${price} * ${qty}, 2)").unwrap();
        match expr {
            Expr::FuncCall { name, args } => {
                assert_eq!(name, "round");
                assert_eq!(args.len(), 2);
            }
            other => panic!("expected FuncCall, got {other:?}"),
        }
    }

    #[test]
    fn nested_functions() {
        let expr = parse("max(abs(${a}), abs(${b}))").unwrap();
        match expr {
            Expr::FuncCall { name, args } => {
                assert_eq!(name, "max");
                assert_eq!(args.len(), 2);
                assert!(matches!(&args[0], Expr::FuncCall { name, .. } if name == "abs"));
            }
            other => panic!("expected FuncCall, got {other:?}"),
        }
    }

    #[test]
    fn unary_neg() {
        let expr = parse("-${x}").unwrap();
        assert_eq!(
            expr,
            Expr::UnaryOp {
                op: UnOp::Neg,
                operand: Box::new(Expr::FieldRef("x".into())),
            }
        );
    }

    #[test]
    fn unary_not() {
        let expr = parse("!${flag}").unwrap();
        assert_eq!(
            expr,
            Expr::UnaryOp {
                op: UnOp::Not,
                operand: Box::new(Expr::FieldRef("flag".into())),
            }
        );
    }

    #[test]
    fn comparison_and_logical() {
        let expr = parse("${x} > 0 && ${y} <= 100").unwrap();
        match expr {
            Expr::BinaryOp { op: BinOp::And, .. } => {}
            other => panic!("expected And, got {other:?}"),
        }
    }

    #[test]
    fn if_function() {
        let expr = parse("if(${age} >= 18, \"adult\", \"minor\")").unwrap();
        match expr {
            Expr::FuncCall { name, args } => {
                assert_eq!(name, "if");
                assert_eq!(args.len(), 3);
            }
            other => panic!("expected FuncCall(if), got {other:?}"),
        }
    }

    #[test]
    fn param_ref() {
        let expr = parse("${price} * ${param.tax_rate}").unwrap();
        match expr {
            Expr::BinaryOp { right, .. } => {
                assert_eq!(*right, Expr::ParamRef("tax_rate".into()));
            }
            other => panic!("expected BinaryOp, got {other:?}"),
        }
    }

    #[test]
    fn null_literal() {
        let expr = parse("coalesce(${x}, null)").unwrap();
        match expr {
            Expr::FuncCall { args, .. } => {
                assert_eq!(args[1], Expr::Literal(LiteralValue::Null));
            }
            other => panic!("expected FuncCall, got {other:?}"),
        }
    }

    #[test]
    fn complex_expression() {
        // if(${qty} > 0, round(${price} * ${qty} * (1.0 + ${param.tax}), 2), 0.0)
        let expr = parse("if(${qty} > 0, round(${price} * ${qty} * (1.0 + ${param.tax}), 2), 0.0)")
            .unwrap();
        match expr {
            Expr::FuncCall { name, args } => {
                assert_eq!(name, "if");
                assert_eq!(args.len(), 3);
            }
            other => panic!("expected FuncCall(if), got {other:?}"),
        }
    }

    #[test]
    fn error_unexpected_eof() {
        assert!(parse("${a} +").is_err());
    }

    #[test]
    fn error_mismatched_paren() {
        assert!(parse("(${a} + ${b}").is_err());
    }

    #[test]
    fn error_trailing_tokens() {
        assert!(parse("${a} ${b}").is_err());
    }

    #[test]
    fn legacy_template_detection() {
        assert!(is_legacy_template("Hello ${name}!"));
        assert!(is_legacy_template("${first}_${last}@example.com"));
        assert!(!is_legacy_template("${a} + ${b}"));
        assert!(!is_legacy_template("round(${x}, 2)"));
        assert!(!is_legacy_template("${a} * ${b} + ${c}"));
    }

    #[test]
    fn left_associativity() {
        // a - b - c should parse as (a - b) - c
        let expr = parse("${a} - ${b} - ${c}").unwrap();
        match expr {
            Expr::BinaryOp {
                op: BinOp::Sub,
                left,
                right,
            } => {
                assert!(matches!(*left, Expr::BinaryOp { op: BinOp::Sub, .. }));
                assert!(matches!(*right, Expr::FieldRef(ref n) if n == "c"));
            }
            other => panic!("expected Sub(Sub(a,b),c), got {other:?}"),
        }
    }

    #[test]
    fn pipe_simple() {
        // ${x} |> abs() desugars to abs(${x})
        let expr = parse("${x} |> abs()").unwrap();
        assert_eq!(
            expr,
            Expr::FuncCall {
                name: "abs".into(),
                args: vec![Expr::FieldRef("x".into())],
            }
        );
    }

    #[test]
    fn pipe_with_args() {
        // ${x} |> round(2) desugars to round(${x}, 2)
        let expr = parse("${x} |> round(2)").unwrap();
        assert_eq!(
            expr,
            Expr::FuncCall {
                name: "round".into(),
                args: vec![
                    Expr::FieldRef("x".into()),
                    Expr::Literal(LiteralValue::Int(2)),
                ],
            }
        );
    }

    #[test]
    fn pipe_chained() {
        // ${x} |> abs() |> round(2) desugars to round(abs(${x}), 2)
        let expr = parse("${x} |> abs() |> round(2)").unwrap();
        assert_eq!(
            expr,
            Expr::FuncCall {
                name: "round".into(),
                args: vec![
                    Expr::FuncCall {
                        name: "abs".into(),
                        args: vec![Expr::FieldRef("x".into())],
                    },
                    Expr::Literal(LiteralValue::Int(2)),
                ],
            }
        );
    }

    #[test]
    fn pipe_precedence_lower_than_arithmetic() {
        // ${x} + 1 |> round(0) desugars to round(${x} + 1, 0)
        let expr = parse("${x} + 1 |> round(0)").unwrap();
        match expr {
            Expr::FuncCall { name, args } => {
                assert_eq!(name, "round");
                assert_eq!(args.len(), 2);
                assert!(matches!(&args[0], Expr::BinaryOp { op: BinOp::Add, .. }));
            }
            other => panic!("expected FuncCall(round), got {other:?}"),
        }
    }

    #[test]
    fn pipe_precedence_lower_than_comparison() {
        // ${x} > 0 |> if(1, 0) desugars to if(${x} > 0, 1, 0)
        let expr = parse("${x} > 0 |> if(1, 0)").unwrap();
        match expr {
            Expr::FuncCall { name, args } => {
                assert_eq!(name, "if");
                assert_eq!(args.len(), 3);
                assert!(matches!(&args[0], Expr::BinaryOp { op: BinOp::Gt, .. }));
            }
            other => panic!("expected FuncCall(if), got {other:?}"),
        }
    }

    #[test]
    fn pipe_precedence_lower_than_logical() {
        // ${a} || ${b} |> f() desugars to f(${a} || ${b})
        let expr = parse("${a} || ${b} |> f()").unwrap();
        match expr {
            Expr::FuncCall { name, args } => {
                assert_eq!(name, "f");
                assert_eq!(args.len(), 1);
                assert!(matches!(&args[0], Expr::BinaryOp { op: BinOp::Or, .. }));
            }
            other => panic!("expected FuncCall(f), got {other:?}"),
        }
    }

    #[test]
    fn pipe_with_parens() {
        // (${x} |> abs()) + 1
        let expr = parse("(${x} |> abs()) + 1").unwrap();
        match expr {
            Expr::BinaryOp {
                op: BinOp::Add,
                left,
                ..
            } => {
                assert!(matches!(*left, Expr::FuncCall { ref name, .. } if name == "abs"));
            }
            other => panic!("expected Add, got {other:?}"),
        }
    }

    #[test]
    fn pipe_error_right_not_function() {
        assert!(parse("${x} |> 42").is_err());
        assert!(parse("${x} |> ${y}").is_err());
    }

    #[test]
    fn pipe_not_legacy_template() {
        // Expressions with |> should never be treated as legacy templates
        assert!(!is_legacy_template("${x} |> abs()"));
        assert!(!is_legacy_template("${x} |> round(2) |> abs()"));
    }
}
