//! `knit generators` — list available generator types with descriptions.

use colored::Colorize;

/// Generator type metadata for display.
struct GeneratorInfo {
    name: &'static str,
    description: &'static str,
    parameters: &'static str,
    example: &'static str,
}

const GENERATORS: &[GeneratorInfo] = &[
    GeneratorInfo {
        name: "distribution",
        description: "Sample from a statistical distribution (normal, uniform, exponential, etc.)",
        parameters: "kind, params (distribution-specific), round (optional)",
        example: r##"type = "distribution", kind = "normal", params = { mean = 50.0, std_dev = 10.0 }"##,
    },
    GeneratorInfo {
        name: "faker",
        description: "Generate structured fake data (names, emails, addresses) via locale-aware faker",
        parameters: "method, args (optional)",
        example: r##"type = "faker", method = "name""##,
    },
    GeneratorInfo {
        name: "sequence",
        description: "Auto-incrementing or stepped sequence, optionally with a string prefix",
        parameters: "start (default: 0), step (default: 1), prefix (optional)",
        example: r##"type = "sequence", start = 1000, step = 1, prefix = "ORD-""##,
    },
    GeneratorInfo {
        name: "one_of",
        description: "Weighted random choice from a fixed set of values",
        parameters: "choices (list of {value, weight} pairs)",
        example: r##"type = "one_of", choices = [{value = "active", weight = 80}, {value = "inactive", weight = 20}]"##,
    },
    GeneratorInfo {
        name: "pattern",
        description: "Regex-like pattern expansion (# = digit, ? = lowercase, A = uppercase)",
        parameters: "pattern",
        example: r####"type = "pattern", pattern = "###-???-AAA""####,
    },
    GeneratorInfo {
        name: "derived",
        description: "Expression that references other fields in the same entity",
        parameters: "expr",
        example: r##"type = "derived", expr = "quantity * price""##,
    },
    GeneratorInfo {
        name: "conditional",
        description: "Value depends on another field's value via branch conditions",
        parameters: "field, branches (list of {when, generator}), default (optional)",
        example: r##"type = "conditional", field = "status", branches = [{when = "active", generator = {type = "constant", value = 1}}]"##,
    },
    GeneratorInfo {
        name: "composite",
        description: "Produces JSON array values from an element generator and length",
        parameters: "template (unused currently), generators (named sub-generators; first is element)",
        example: r##"type = "composite", template = "", generators = { item = { type = "faker", method = "word" } }"##,
    },
    GeneratorInfo {
        name: "lookup",
        description: "Foreign key lookup — copies values from another entity's field",
        parameters: "entity, field",
        example: r##"type = "lookup", entity = "users", field = "email""##,
    },
    GeneratorInfo {
        name: "constant",
        description: "Every row receives the same fixed value",
        parameters: "value",
        example: r##"type = "constant", value = "pending""##,
    },
    GeneratorInfo {
        name: "uuid",
        description: "Generate a UUID (v4 by default)",
        parameters: "version (default: 4)",
        example: r##"type = "uuid""##,
    },
    GeneratorInfo {
        name: "unique",
        description: "Wrap an inner generator with uniqueness enforcement via retry",
        parameters: "inner (generator spec), max_retries (default: 1000)",
        example: r##"type = "unique", inner = { type = "faker", method = "email" }"##,
    },
    GeneratorInfo {
        name: "relative",
        description: "Value relative to another field (e.g. end_date = start_date + offset)",
        parameters: "field, offset",
        example: r##"type = "relative", field = "start_date", offset = 7"##,
    },
    GeneratorInfo {
        name: "business_hours",
        description: "Timestamps constrained to business hours (and optionally weekdays)",
        parameters: "start_hour (default: 9), end_hour (default: 17), exclude_weekends",
        example: r##"type = "business_hours", start_hour = 8, end_hour = 18, exclude_weekends = true"##,
    },
    GeneratorInfo {
        name: "dictionary",
        description: "Sample from an external dictionary file (one value per line)",
        parameters: "file, expansion (\"sample\"|\"combinatorial\"|\"suffix\", default: \"sample\")",
        example: r##"type = "dictionary", file = "dictionaries/cities.txt""##,
    },
    // Behavioral modeling generators
    GeneratorInfo {
        name: "actor_ref",
        description: "Reference to an actor entity for behavioral identity",
        parameters: "entity (name of the actor entity)",
        example: r##"type = "actor_ref", entity = "users""##,
    },
    GeneratorInfo {
        name: "actor_temporal",
        description: "Generate temporal values influenced by actor persona traits",
        parameters: "trait (persona trait name), temporal_after (optional causal ordering), burst (optional session config)",
        example: r##"type = "actor_temporal", trait = "activity_hours""##,
    },
    GeneratorInfo {
        name: "relationship_ref",
        description: "Generate values based on actor-to-actor relationships",
        parameters: "relationship (name of an actor_relationship)",
        example: r##"type = "relationship_ref", relationship = "reports_to""##,
    },
    GeneratorInfo {
        name: "persona_field",
        description: "Generate values derived from the actor's persona trait",
        parameters: "trait (persona trait name to read the value from)",
        example: r##"type = "persona_field", trait = "department""##,
    },
    GeneratorInfo {
        name: "thread_ref",
        description: "Self-referential thread/conversation structure (nullable FK to own PK)",
        parameters: "reply_probability (0.0–1.0, default 0.6), max_depth (default 10), reply_window (default 100)",
        example: r##"type = "thread_ref", reply_probability = 0.7, max_depth = 5"##,
    },
];

const DISTRIBUTIONS: &[(&str, &str)] = &[
    ("uniform", "Continuous uniform over [min, max]"),
    ("normal", "Gaussian / bell curve (mean, std_dev)"),
    ("log_normal", "Log-normal: exp(Normal(mean, std_dev))"),
    ("exponential", "Exponential with rate lambda"),
    ("poisson", "Poisson with rate lambda"),
    ("bernoulli", "Bernoulli trial with probability p"),
    ("binomial", "Binomial: n trials, each with probability p"),
    ("geometric", "Geometric: trials until first success (p)"),
    ("pareto", "Pareto with shape alpha and scale x_m"),
    ("weibull", "Weibull with shape k and scale lambda"),
    ("gamma", "Gamma with shape and scale parameters"),
    ("beta", "Beta on [0,1] with alpha and beta"),
    ("cauchy", "Cauchy with location x0 and scale gamma"),
    ("chi_squared", "Chi-squared with k degrees of freedom"),
    ("student_t", "Student's t with nu degrees of freedom"),
    ("triangular", "Triangular with min, mode, and max"),
    ("zipf", "Zipf (power-law) with exponent s and n elements"),
];

/// Run the generators command.
pub fn run(json: bool) -> anyhow::Result<()> {
    if json {
        let generators: Vec<serde_json::Value> = GENERATORS
            .iter()
            .map(|g| {
                serde_json::json!({
                    "name": g.name,
                    "description": g.description,
                    "parameters": g.parameters,
                    "example": g.example,
                })
            })
            .collect();
        let distributions: Vec<serde_json::Value> = DISTRIBUTIONS
            .iter()
            .map(|(name, desc)| {
                serde_json::json!({
                    "name": name,
                    "description": desc,
                })
            })
            .collect();
        let output = serde_json::json!({
            "generators": generators,
            "distributions": distributions,
        });
        println!("{}", serde_json::to_string_pretty(&output)?);
    } else {
        println!("{}", "═══ Available Generators ═══".green().bold());
        println!();
        for g in GENERATORS {
            println!("  {} {}", "●".green(), g.name.bold());
            println!("    {}", g.description);
            println!("    {} {}", "params:".dimmed(), g.parameters);
            println!("    {} {}", "example:".dimmed(), g.example);
            println!();
        }

        println!("{}", "═══ Distribution Kinds ═══".green().bold());
        println!("  (used with type = \"distribution\", kind = \"...\")\n");
        for (name, desc) in DISTRIBUTIONS {
            println!("  {} {:14} {}", "●".green(), name.bold(), desc);
        }
        println!();
        println!(
            "  {} {} generators, {} distribution kinds",
            "total:".dimmed(),
            GENERATORS.len(),
            DISTRIBUTIONS.len()
        );
    }

    Ok(())
}
