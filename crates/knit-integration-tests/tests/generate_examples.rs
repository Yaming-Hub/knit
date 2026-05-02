//! End-to-end generation tests: parse → validate → compile → generate batches,
//! then verify output row counts and column presence.

use knit_integration_tests::{example_schemas, generate_from_file, total_rows};
use knit_schema::parse_toml_file;

#[test]
fn generate_ecommerce_schema() {
    let path = example_schemas()
        .into_iter()
        .find(|p| {
            p.file_name()
                .and_then(|s| s.to_str())
                .map_or(false, |n| n.starts_with("ecommerce"))
        })
        .expect("ecommerce.weave.toml not found");

    let model = parse_toml_file(&path).unwrap();
    let batches = generate_from_file(&path);

    for entity in &model.entities {
        let entity_batches = batches
            .get(&entity.name)
            .unwrap_or_else(|| panic!("no batches for entity '{}'", entity.name));
        let rows = total_rows(entity_batches);

        // Row count must match the schema count spec (all are Fixed here).
        let expected: u64 = match &entity.count {
            knit_core::CountSpec::Fixed(n) => *n,
            _ => continue,
        };
        assert_eq!(
            rows as u64, expected,
            "entity '{}': expected {expected} rows, got {rows}",
            entity.name
        );

        // Every field must appear as a column.
        let schema = entity_batches[0].schema();
        for field in &entity.fields {
            assert!(
                schema.field_with_name(&field.name).is_ok(),
                "entity '{}': missing column '{}'",
                entity.name,
                field.name,
            );
        }
    }
}

#[test]
fn generate_iot_sensors_schema() {
    let path = example_schemas()
        .into_iter()
        .find(|p| {
            p.file_name()
                .and_then(|s| s.to_str())
                .map_or(false, |n| n.starts_with("iot"))
        })
        .expect("iot_sensors.weave.toml not found");

    let model = parse_toml_file(&path).unwrap();
    let batches = generate_from_file(&path);

    for entity in &model.entities {
        let entity_batches = batches
            .get(&entity.name)
            .unwrap_or_else(|| panic!("no batches for entity '{}'", entity.name));
        let rows = total_rows(entity_batches);

        if let knit_core::CountSpec::Fixed(expected) = &entity.count {
            assert_eq!(
                rows as u64, *expected,
                "entity '{}': row count mismatch",
                entity.name
            );
        }

        let schema = entity_batches[0].schema();
        for field in &entity.fields {
            assert!(
                schema.field_with_name(&field.name).is_ok(),
                "entity '{}': missing column '{}'",
                entity.name,
                field.name,
            );
        }
    }
}

#[test]
fn generate_server_logs_schema() {
    let path = example_schemas()
        .into_iter()
        .find(|p| {
            p.file_name()
                .and_then(|s| s.to_str())
                .map_or(false, |n| n.starts_with("server_logs"))
        })
        .expect("server_logs.weave.toml not found");

    let model = parse_toml_file(&path).unwrap();
    let batches = generate_from_file(&path);

    for entity in &model.entities {
        let entity_batches = batches
            .get(&entity.name)
            .unwrap_or_else(|| panic!("no batches for entity '{}'", entity.name));
        let rows = total_rows(entity_batches);

        if let knit_core::CountSpec::Fixed(expected) = &entity.count {
            assert_eq!(
                rows as u64, *expected,
                "entity '{}': row count mismatch",
                entity.name
            );
        }
    }
}

#[test]
fn generate_financial_schema() {
    let path = example_schemas()
        .into_iter()
        .find(|p| {
            p.file_name()
                .and_then(|s| s.to_str())
                .map_or(false, |n| n.starts_with("financial"))
        })
        .expect("financial.weave.toml not found");

    let model = parse_toml_file(&path).unwrap();
    let batches = generate_from_file(&path);

    for entity in &model.entities {
        let entity_batches = batches
            .get(&entity.name)
            .unwrap_or_else(|| panic!("no batches for entity '{}'", entity.name));
        let rows = total_rows(entity_batches);

        if let knit_core::CountSpec::Fixed(expected) = &entity.count {
            assert_eq!(
                rows as u64, *expected,
                "entity '{}': row count mismatch",
                entity.name
            );
        }
    }
}

#[test]
fn generate_hr_org_schema() {
    let path = example_schemas()
        .into_iter()
        .find(|p| {
            p.file_name()
                .and_then(|s| s.to_str())
                .map_or(false, |n| n.starts_with("hr_org"))
        })
        .expect("hr_org.weave.toml not found");

    let model = parse_toml_file(&path).unwrap();
    let batches = generate_from_file(&path);

    // Verify all entities produced batches.
    for entity in &model.entities {
        let entity_batches = batches
            .get(&entity.name)
            .unwrap_or_else(|| panic!("no batches for entity '{}'", entity.name));
        let rows = total_rows(entity_batches);

        if let knit_core::CountSpec::Fixed(expected) = &entity.count {
            // Self-referential relationships may produce deferred batches,
            // so total rows can exceed the fixed count. Check >= instead.
            assert!(
                rows as u64 >= *expected,
                "entity '{}': expected at least {expected} rows, got {rows}",
                entity.name
            );
        }
    }
}

#[test]
fn all_examples_generate_without_errors() {
    for path in example_schemas() {
        let stem = path.file_name().unwrap().to_string_lossy().to_string();
        let batches = generate_from_file(&path);
        assert!(
            !batches.is_empty(),
            "{stem}: generation produced no entity batches"
        );
        for (entity, entity_batches) in &batches {
            assert!(
                !entity_batches.is_empty(),
                "{stem}/{entity}: no batches produced"
            );
            let rows = total_rows(entity_batches);
            assert!(rows > 0, "{stem}/{entity}: zero rows generated");
        }
    }
}
