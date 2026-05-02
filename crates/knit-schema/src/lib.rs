//! knit-schema: Parser and validator for the Weave schema language.

mod error;
mod extends;
mod parser;
mod validate;

pub use error::SchemaError;
pub use extends::{merge_models, resolve_extends};
pub use parser::{parse_json, parse_json_file, parse_toml, parse_toml_file};
pub use validate::validate;
