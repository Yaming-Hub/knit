//! # blueprint — Parser and Validator for the Knit Blueprint Language
//!
//! This module converts TOML or JSON blueprint files into a validated
//! [`DataModel`](crate::core::DataModel), the canonical in-memory representation
//! consumed by the rest of the knit pipeline.
//!
//! ## Pipeline Position
//!
//! ```text
//! Knit Blueprint (TOML/JSON) → blueprint → DataModel → plan → gen
//! ```
//!
//! ## Key Entry Points
//!
//! - [`parse_toml()`] / [`parse_json()`] — parse from a string
//! - [`parse_toml_file()`] / [`parse_json_file()`] — parse from a file (with `extends` support)
//! - [`validate()`] — semantic validation (references, distributions, counts)
//! - [`merge_models()`] — merge a child model on top of a parent (for `extends` chains)
//! - [`resolve_extends()`] — resolve an `extends` directive from a file path
//!
//! ## Error Handling
//!
//! All functions return [`BlueprintError`], which wraps TOML/JSON parse errors,
//! I/O errors, and semantic validation issues.

mod error;
mod extends;
pub(crate) mod includes;
mod parser;
mod validate;

pub use error::BlueprintError;
pub use extends::{merge_models, resolve_extends};
pub use includes::{merge_main_over_includes, resolve_includes};
pub use parser::{parse_json, parse_json_file, parse_toml, parse_toml_file};
pub use validate::validate;
