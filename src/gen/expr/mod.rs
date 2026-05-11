//! Expression engine for derived field generation.
//!
//! Provides a full expression language with:
//! - Arithmetic, comparison, and boolean operators
//! - ~20 built-in functions (math, string, type-cast, conditional)
//! - `${field}` and `${param.key}` references
//! - Vectorized evaluation over Arrow arrays
//!
//! # Pipeline
//!
//! ```text
//! Expression string → Lexer → Parser → AST → Type-check → Evaluator → ArrayRef
//! ```

pub mod ast;
pub mod eval;
pub mod functions;
pub mod lexer;
pub mod parser;