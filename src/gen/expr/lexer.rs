//! Lexer (tokenizer) for the expression language.
//!
//! Converts an expression string into a stream of [`Token`]s.
//! Handles `${field}` references, numbers, strings, operators, and identifiers.

use std::fmt;

/// A token produced by the lexer.
#[derive(Debug, Clone, PartialEq)]
pub enum Token {
    /// Integer literal: `42`, `-7`.
    Int(i64),
    /// Floating-point literal: `3.14`, `1e10`.
    Float(f64),
    /// String literal: `"hello"`.
    Str(String),
    /// Boolean literal: `true` or `false`.
    Bool(bool),
    /// Null literal: `null`.
    Null,
    /// Field reference: `${field_name}`.
    FieldRef(String),
    /// Parameter reference: `${param.key}`.
    ParamRef(String),
    /// Identifier (function name or keyword): `abs`, `if`, etc.
    Ident(String),

    // Operators
    /// `+`
    Plus,
    /// `-`
    Minus,
    /// `*`
    Star,
    /// `/`
    Slash,
    /// `%`
    Percent,
    /// `==`
    EqEq,
    /// `!=`
    BangEq,
    /// `<`
    Lt,
    /// `>`
    Gt,
    /// `<=`
    LtEq,
    /// `>=`
    GtEq,
    /// `&&`
    AmpAmp,
    /// `||`
    PipePipe,
    /// `|>`
    PipeGt,
    /// `!`
    Bang,

    // Delimiters
    /// `(`
    LParen,
    /// `)`
    RParen,
    /// `,`
    Comma,

    /// End of input.
    Eof,
}

/// Position in the source string for error reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Span {
    /// Byte offset of the start of the token.
    pub start: usize,
    /// Byte offset past the end of the token.
    pub end: usize,
}

/// A token with its source position.
#[derive(Debug, Clone, PartialEq)]
pub struct SpannedToken {
    /// The token.
    pub token: Token,
    /// Source position.
    pub span: Span,
}

/// Lexer error.
#[derive(Debug, Clone)]
pub struct LexError {
    /// Error message.
    pub message: String,
    /// Position in source where the error occurred.
    pub pos: usize,
}

impl fmt::Display for LexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "lex error at position {}: {}", self.pos, self.message)
    }
}

impl std::error::Error for LexError {}

/// Tokenize an expression string into a list of spanned tokens.
pub fn tokenize(input: &str) -> Result<Vec<SpannedToken>, LexError> {
    let mut tokens = Vec::new();
    let bytes = input.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        // Skip whitespace
        if bytes[i].is_ascii_whitespace() {
            i += 1;
            continue;
        }

        let start = i;

        // Field/param reference: ${...}
        if i + 1 < bytes.len() && bytes[i] == b'$' && bytes[i + 1] == b'{' {
            i += 2;
            let name_start = i;
            while i < bytes.len() && bytes[i] != b'}' {
                i += 1;
            }
            if i >= bytes.len() {
                return Err(LexError {
                    message: "unterminated field reference `${...}`".into(),
                    pos: start,
                });
            }
            let name = &input[name_start..i];
            i += 1; // skip '}'

            if let Some(key) = name.strip_prefix("param.") {
                tokens.push(SpannedToken {
                    token: Token::ParamRef(key.to_string()),
                    span: Span { start, end: i },
                });
            } else {
                tokens.push(SpannedToken {
                    token: Token::FieldRef(name.to_string()),
                    span: Span { start, end: i },
                });
            }
            continue;
        }

        // String literal: "..."
        if bytes[i] == b'"' {
            i += 1;
            let mut s = String::new();
            while i < bytes.len() && bytes[i] != b'"' {
                if bytes[i] == b'\\' && i + 1 < bytes.len() {
                    i += 1;
                    match bytes[i] {
                        b'n' => s.push('\n'),
                        b't' => s.push('\t'),
                        b'\\' => s.push('\\'),
                        b'"' => s.push('"'),
                        _ => {
                            s.push('\\');
                            s.push(bytes[i] as char);
                        }
                    }
                    i += 1;
                } else {
                    // Decode a full UTF-8 character from the byte stream
                    let remaining = &input[i..];
                    let ch = remaining.chars().next().unwrap();
                    s.push(ch);
                    i += ch.len_utf8();
                }
            }
            if i >= bytes.len() {
                return Err(LexError {
                    message: "unterminated string literal".into(),
                    pos: start,
                });
            }
            i += 1; // skip closing '"'
            tokens.push(SpannedToken {
                token: Token::Str(s),
                span: Span { start, end: i },
            });
            continue;
        }

        // Number: integer or float
        if bytes[i].is_ascii_digit() || (bytes[i] == b'.' && i + 1 < bytes.len() && bytes[i + 1].is_ascii_digit()) {
            let num_start = i;
            let mut has_dot = false;
            let mut has_exp = false;

            while i < bytes.len() {
                if bytes[i].is_ascii_digit() {
                    i += 1;
                } else if bytes[i] == b'.' && !has_dot && !has_exp {
                    has_dot = true;
                    i += 1;
                } else if (bytes[i] == b'e' || bytes[i] == b'E') && !has_exp {
                    has_exp = true;
                    i += 1;
                    if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
                        i += 1;
                    }
                } else {
                    break;
                }
            }

            let text = &input[num_start..i];
            if has_dot || has_exp {
                let val: f64 = text.parse().map_err(|_| LexError {
                    message: format!("invalid float literal: {text}"),
                    pos: num_start,
                })?;
                tokens.push(SpannedToken {
                    token: Token::Float(val),
                    span: Span { start, end: i },
                });
            } else {
                let val: i64 = text.parse().map_err(|_| LexError {
                    message: format!("invalid integer literal: {text}"),
                    pos: num_start,
                })?;
                tokens.push(SpannedToken {
                    token: Token::Int(val),
                    span: Span { start, end: i },
                });
            }
            continue;
        }

        // Identifier or keyword
        if bytes[i].is_ascii_alphabetic() || bytes[i] == b'_' {
            let id_start = i;
            while i < bytes.len() && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'_') {
                i += 1;
            }
            let word = &input[id_start..i];
            let token = match word {
                "true" => Token::Bool(true),
                "false" => Token::Bool(false),
                "null" => Token::Null,
                _ => Token::Ident(word.to_string()),
            };
            tokens.push(SpannedToken {
                token,
                span: Span { start, end: i },
            });
            continue;
        }

        // Two-character operators
        if i + 1 < bytes.len() {
            let two = &input[i..i + 2];
            let tok = match two {
                "==" => Some(Token::EqEq),
                "!=" => Some(Token::BangEq),
                "<=" => Some(Token::LtEq),
                ">=" => Some(Token::GtEq),
                "&&" => Some(Token::AmpAmp),
                "||" => Some(Token::PipePipe),
                "|>" => Some(Token::PipeGt),
                _ => None,
            };
            if let Some(t) = tok {
                tokens.push(SpannedToken {
                    token: t,
                    span: Span { start, end: i + 2 },
                });
                i += 2;
                continue;
            }
        }

        // Single-character operators/delimiters
        let tok = match bytes[i] {
            b'+' => Some(Token::Plus),
            b'-' => Some(Token::Minus),
            b'*' => Some(Token::Star),
            b'/' => Some(Token::Slash),
            b'%' => Some(Token::Percent),
            b'<' => Some(Token::Lt),
            b'>' => Some(Token::Gt),
            b'!' => Some(Token::Bang),
            b'(' => Some(Token::LParen),
            b')' => Some(Token::RParen),
            b',' => Some(Token::Comma),
            _ => None,
        };

        if let Some(t) = tok {
            tokens.push(SpannedToken {
                token: t,
                span: Span { start, end: i + 1 },
            });
            i += 1;
            continue;
        }

        return Err(LexError {
            message: format!("unexpected character: {:?}", bytes[i] as char),
            pos: i,
        });
    }

    tokens.push(SpannedToken {
        token: Token::Eof,
        span: Span {
            start: input.len(),
            end: input.len(),
        },
    });

    Ok(tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tok_types(input: &str) -> Vec<Token> {
        tokenize(input)
            .unwrap()
            .into_iter()
            .map(|st| st.token)
            .collect()
    }

    #[test]
    fn simple_arithmetic() {
        assert_eq!(
            tok_types("${a} + ${b} * 2"),
            vec![
                Token::FieldRef("a".into()),
                Token::Plus,
                Token::FieldRef("b".into()),
                Token::Star,
                Token::Int(2),
                Token::Eof,
            ]
        );
    }

    #[test]
    fn float_and_scientific() {
        assert_eq!(
            tok_types("3.14 + 1e5"),
            vec![Token::Float(3.14), Token::Plus, Token::Float(1e5), Token::Eof]
        );
    }

    #[test]
    fn string_with_escapes() {
        assert_eq!(
            tok_types(r#""hello\nworld""#),
            vec![Token::Str("hello\nworld".into()), Token::Eof]
        );
    }

    #[test]
    fn comparison_and_logical() {
        assert_eq!(
            tok_types("${x} >= 5 && ${y} != 0"),
            vec![
                Token::FieldRef("x".into()),
                Token::GtEq,
                Token::Int(5),
                Token::AmpAmp,
                Token::FieldRef("y".into()),
                Token::BangEq,
                Token::Int(0),
                Token::Eof,
            ]
        );
    }

    #[test]
    fn param_ref() {
        assert_eq!(
            tok_types("${param.tax_rate}"),
            vec![Token::ParamRef("tax_rate".into()), Token::Eof]
        );
    }

    #[test]
    fn function_call() {
        assert_eq!(
            tok_types("round(${price}, 2)"),
            vec![
                Token::Ident("round".into()),
                Token::LParen,
                Token::FieldRef("price".into()),
                Token::Comma,
                Token::Int(2),
                Token::RParen,
                Token::Eof,
            ]
        );
    }

    #[test]
    fn keywords() {
        assert_eq!(
            tok_types("true false null"),
            vec![Token::Bool(true), Token::Bool(false), Token::Null, Token::Eof]
        );
    }

    #[test]
    fn unterminated_field_ref() {
        assert!(tokenize("${abc").is_err());
    }

    #[test]
    fn unterminated_string() {
        assert!(tokenize(r#""hello"#).is_err());
    }

    #[test]
    fn unary_operators() {
        assert_eq!(
            tok_types("-${x} + !${flag}"),
            vec![
                Token::Minus,
                Token::FieldRef("x".into()),
                Token::Plus,
                Token::Bang,
                Token::FieldRef("flag".into()),
                Token::Eof,
            ]
        );
    }

    #[test]
    fn modulo() {
        assert_eq!(
            tok_types("${a} % 3"),
            vec![
                Token::FieldRef("a".into()),
                Token::Percent,
                Token::Int(3),
                Token::Eof,
            ]
        );
    }

    #[test]
    fn pipe_operator() {
        assert_eq!(
            tok_types("${x} |> abs()"),
            vec![
                Token::FieldRef("x".into()),
                Token::PipeGt,
                Token::Ident("abs".into()),
                Token::LParen,
                Token::RParen,
                Token::Eof,
            ]
        );
    }

    #[test]
    fn pipe_vs_or() {
        // |> and || are distinct
        assert_eq!(
            tok_types("${a} || ${b} |> f()"),
            vec![
                Token::FieldRef("a".into()),
                Token::PipePipe,
                Token::FieldRef("b".into()),
                Token::PipeGt,
                Token::Ident("f".into()),
                Token::LParen,
                Token::RParen,
                Token::Eof,
            ]
        );
    }
}