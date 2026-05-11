//! Determinism test: generating with the same seed must produce identical output.

mod common;
use common::generate_from_toml;

/// A minimal schema used by determinism tests.
const DETERMINISM_SCHEMA: &str = r#"
blueprint_version = "1.0"

[model]
name = "determinism_test"
seed = 99999

[[entities]]
name = "items"
count = 500

[[entities.fields]]
name = "id"
data_type = "int"
primary_key = true
[entities.fields.generator]
type = "sequence"
start = 1
step = 1

[[entities.fields]]
name = "value"
data_type = "float"
[entities.fields.generator]
type = "distribution"
kind = "normal"
[entities.fields.generator.params]
mean = 50.0
std_dev = 10.0

[[entities.fields]]
name = "label"
data_type = "string"
[entities.fields.generator]
type = "one_of"
choices = [
    { value = "alpha", weight = 0.5 },
    { value = "beta", weight = 0.3 },
    { value = "gamma", weight = 0.2 },
]

[[entities.fields]]
name = "flag"
data_type = "int"
[entities.fields.generator]
type = "distribution"
kind = "bernoulli"
[entities.fields.generator.params]
p = 0.7
"#;

#[test]
fn same_seed_produces_identical_batches() {
    let run1 = generate_from_toml(DETERMINISM_SCHEMA);
    let run2 = generate_from_toml(DETERMINISM_SCHEMA);

    assert_eq!(
        run1.keys().collect::<std::collections::BTreeSet<_>>(),
        run2.keys().collect::<std::collections::BTreeSet<_>>(),
        "entity sets differ between runs"
    );

    for (entity, batches1) in &run1 {
        let batches2 = run2.get(entity).expect("entity missing in second run");
        assert_eq!(
            batches1.len(),
            batches2.len(),
            "entity '{entity}': batch count differs"
        );

        for (i, (b1, b2)) in batches1.iter().zip(batches2.iter()).enumerate() {
            assert_eq!(
                b1.num_rows(),
                b2.num_rows(),
                "entity '{entity}' batch {i}: row count differs"
            );
            assert_eq!(
                b1.num_columns(),
                b2.num_columns(),
                "entity '{entity}' batch {i}: column count differs"
            );
            // Column-by-column equality.
            for col in 0..b1.num_columns() {
                assert_eq!(
                    b1.column(col).as_ref(),
                    b2.column(col).as_ref(),
                    "entity '{entity}' batch {i} column {col}: data mismatch"
                );
            }
        }
    }
}

#[test]
fn different_seeds_produce_different_output() {
    let schema_a = r#"
blueprint_version = "1.0"

[model]
name = "seed_a"
seed = 1111

[[entities]]
name = "nums"
count = 100

[[entities.fields]]
name = "v"
data_type = "float"
[entities.fields.generator]
type = "distribution"
kind = "uniform"
[entities.fields.generator.params]
min = 0.0
max = 1000.0
"#;

    let schema_b = r#"
blueprint_version = "1.0"

[model]
name = "seed_b"
seed = 2222

[[entities]]
name = "nums"
count = 100

[[entities.fields]]
name = "v"
data_type = "float"
[entities.fields.generator]
type = "distribution"
kind = "uniform"
[entities.fields.generator.params]
min = 0.0
max = 1000.0
"#;

    let run_a = generate_from_toml(schema_a);
    let run_b = generate_from_toml(schema_b);

    let batches_a = &run_a["nums"];
    let batches_b = &run_b["nums"];

    // At least one column value should differ with overwhelming probability.
    let mut all_equal = true;
    for (ba, bb) in batches_a.iter().zip(batches_b.iter()) {
        for col in 0..ba.num_columns() {
            if ba.column(col).as_ref() != bb.column(col).as_ref() {
                all_equal = false;
                break;
            }
        }
        if !all_equal {
            break;
        }
    }
    assert!(!all_equal, "different seeds should produce different data");
}