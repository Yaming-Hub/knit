//! knit-schema: Parser and validator for the Weave schema language.

mod error;
mod parser;

pub use error::SchemaError;
pub use parser::{parse_json, parse_json_file, parse_toml, parse_toml_file};
