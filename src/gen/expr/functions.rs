//! Built-in function registry with type signatures.
//!
//! Defines which functions are available in expressions and their expected
//! argument types and return types.

use super::ast::ExprType;

/// A function signature describing expected argument types and return type.
#[derive(Debug, Clone)]
pub struct FuncSig {
    /// Function name (lowercase).
    pub name: &'static str,
    /// Minimum number of arguments.
    pub min_args: usize,
    /// Maximum number of arguments.
    pub max_args: usize,
    /// Expected argument types (if fixed). Empty means any.
    pub arg_types: &'static [ExprType],
    /// Return type. `Unknown` means depends on input types.
    pub return_type: ExprType,
    /// Whether this function has special null handling (does NOT propagate nulls).
    pub null_safe: bool,
}

/// Get the signature for a built-in function by name.
///
/// Returns `None` if the function is not recognized.
pub fn lookup(name: &str) -> Option<&'static FuncSig> {
    BUILTINS.iter().find(|sig| sig.name == name)
}

/// All available function names.
pub fn available_names() -> Vec<&'static str> {
    BUILTINS.iter().map(|s| s.name).collect()
}

// Function signatures registry.
static BUILTINS: &[FuncSig] = &[
    // Math functions
    FuncSig {
        name: "abs",
        min_args: 1,
        max_args: 1,
        arg_types: &[],
        return_type: ExprType::Unknown, // same as input
        null_safe: false,
    },
    FuncSig {
        name: "ceil",
        min_args: 1,
        max_args: 1,
        arg_types: &[ExprType::Float],
        return_type: ExprType::Int,
        null_safe: false,
    },
    FuncSig {
        name: "floor",
        min_args: 1,
        max_args: 1,
        arg_types: &[ExprType::Float],
        return_type: ExprType::Int,
        null_safe: false,
    },
    FuncSig {
        name: "round",
        min_args: 1,
        max_args: 2,
        arg_types: &[],
        return_type: ExprType::Float,
        null_safe: false,
    },
    FuncSig {
        name: "min",
        min_args: 2,
        max_args: 2,
        arg_types: &[],
        return_type: ExprType::Unknown,
        null_safe: false,
    },
    FuncSig {
        name: "max",
        min_args: 2,
        max_args: 2,
        arg_types: &[],
        return_type: ExprType::Unknown,
        null_safe: false,
    },
    FuncSig {
        name: "clamp",
        min_args: 3,
        max_args: 3,
        arg_types: &[],
        return_type: ExprType::Unknown,
        null_safe: false,
    },
    // String functions
    FuncSig {
        name: "upper",
        min_args: 1,
        max_args: 1,
        arg_types: &[ExprType::Str],
        return_type: ExprType::Str,
        null_safe: false,
    },
    FuncSig {
        name: "lower",
        min_args: 1,
        max_args: 1,
        arg_types: &[ExprType::Str],
        return_type: ExprType::Str,
        null_safe: false,
    },
    FuncSig {
        name: "trim",
        min_args: 1,
        max_args: 1,
        arg_types: &[ExprType::Str],
        return_type: ExprType::Str,
        null_safe: false,
    },
    FuncSig {
        name: "len",
        min_args: 1,
        max_args: 1,
        arg_types: &[ExprType::Str],
        return_type: ExprType::Int,
        null_safe: false,
    },
    FuncSig {
        name: "concat",
        min_args: 2,
        max_args: 16,
        arg_types: &[],
        return_type: ExprType::Str,
        null_safe: false,
    },
    FuncSig {
        name: "substr",
        min_args: 2,
        max_args: 3,
        arg_types: &[],
        return_type: ExprType::Str,
        null_safe: false,
    },
    FuncSig {
        name: "replace",
        min_args: 3,
        max_args: 3,
        arg_types: &[ExprType::Str, ExprType::Str, ExprType::Str],
        return_type: ExprType::Str,
        null_safe: false,
    },
    // Type cast functions
    FuncSig {
        name: "cast_int",
        min_args: 1,
        max_args: 1,
        arg_types: &[],
        return_type: ExprType::Int,
        null_safe: false,
    },
    FuncSig {
        name: "cast_float",
        min_args: 1,
        max_args: 1,
        arg_types: &[],
        return_type: ExprType::Float,
        null_safe: false,
    },
    FuncSig {
        name: "cast_string",
        min_args: 1,
        max_args: 1,
        arg_types: &[],
        return_type: ExprType::Str,
        null_safe: false,
    },
    // Conditional functions (null-safe)
    FuncSig {
        name: "if",
        min_args: 3,
        max_args: 3,
        arg_types: &[],
        return_type: ExprType::Unknown,
        null_safe: true,
    },
    FuncSig {
        name: "coalesce",
        min_args: 2,
        max_args: 16,
        arg_types: &[],
        return_type: ExprType::Unknown,
        null_safe: true,
    },
    FuncSig {
        name: "nullif",
        min_args: 2,
        max_args: 2,
        arg_types: &[],
        return_type: ExprType::Unknown,
        null_safe: true,
    },
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_known_functions() {
        assert!(lookup("abs").is_some());
        assert!(lookup("round").is_some());
        assert!(lookup("if").is_some());
        assert!(lookup("coalesce").is_some());
        assert!(lookup("upper").is_some());
    }

    #[test]
    fn lookup_unknown() {
        assert!(lookup("nonexistent").is_none());
    }

    #[test]
    fn null_safe_functions() {
        assert!(lookup("if").unwrap().null_safe);
        assert!(lookup("coalesce").unwrap().null_safe);
        assert!(lookup("nullif").unwrap().null_safe);
        assert!(!lookup("abs").unwrap().null_safe);
    }

    #[test]
    fn available_names_not_empty() {
        let names = available_names();
        assert!(names.len() >= 20);
        assert!(names.contains(&"abs"));
        assert!(names.contains(&"concat"));
    }
}
