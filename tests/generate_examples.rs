//! End-to-end generation tests: parse → validate → compile → generate batches,
//! then verify output row counts and column presence.

mod common;
use common::{example_schemas, generate_from_file, total_rows};
use knit::blueprint::parse_toml_file;

#[test]
fn generate_ecommerce_schema() {
    let path = example_schemas()
        .into_iter()
        .find(|p| {
            p.file_name()
                .and_then(|s| s.to_str())
                .is_some_and(|n| n.starts_with("ecommerce"))
        })
        .expect("ecommerce.knit.toml not found");

    let model = parse_toml_file(&path).unwrap();
    let batches = generate_from_file(&path);

    for entity in &model.entities {
        let entity_batches = batches
            .get(&entity.name)
            .unwrap_or_else(|| panic!("no batches for entity '{}'", entity.name));
        let rows = total_rows(entity_batches);

        // Row count must match the schema count spec (all are Fixed here).
        let expected: u64 = match &entity.count {
            knit::core::CountSpec::Fixed(n) => *n,
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

        // At least the first field (typically the PK) should produce non-null values.
        let first_col = entity_batches[0].column(0);
        assert!(
            first_col.null_count() < first_col.len(),
            "entity '{}': first column is entirely null (generator may have degraded)",
            entity.name,
        );
    }
}

#[test]
fn generate_iot_sensors_schema() {
    let path = example_schemas()
        .into_iter()
        .find(|p| {
            p.file_name()
                .and_then(|s| s.to_str())
                .is_some_and(|n| n.starts_with("iot"))
        })
        .expect("iot_sensors.knit.toml not found");

    let model = parse_toml_file(&path).unwrap();
    let batches = generate_from_file(&path);

    for entity in &model.entities {
        let entity_batches = batches
            .get(&entity.name)
            .unwrap_or_else(|| panic!("no batches for entity '{}'", entity.name));
        let rows = total_rows(entity_batches);

        if let knit::core::CountSpec::Fixed(expected) = &entity.count {
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
                .is_some_and(|n| n.starts_with("server_logs"))
        })
        .expect("server_logs.knit.toml not found");

    let model = parse_toml_file(&path).unwrap();
    let batches = generate_from_file(&path);

    for entity in &model.entities {
        let entity_batches = batches
            .get(&entity.name)
            .unwrap_or_else(|| panic!("no batches for entity '{}'", entity.name));
        let rows = total_rows(entity_batches);

        if let knit::core::CountSpec::Fixed(expected) = &entity.count {
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
                .is_some_and(|n| n.starts_with("financial"))
        })
        .expect("financial.knit.toml not found");

    let model = parse_toml_file(&path).unwrap();
    let batches = generate_from_file(&path);

    for entity in &model.entities {
        let entity_batches = batches
            .get(&entity.name)
            .unwrap_or_else(|| panic!("no batches for entity '{}'", entity.name));
        let rows = total_rows(entity_batches);

        if let knit::core::CountSpec::Fixed(expected) = &entity.count {
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
                .is_some_and(|n| n.starts_with("hr_org"))
        })
        .expect("hr_org.knit.toml not found");

    let model = parse_toml_file(&path).unwrap();
    let batches = generate_from_file(&path);

    // Verify all entities produced batches.
    for entity in &model.entities {
        let entity_batches = batches
            .get(&entity.name)
            .unwrap_or_else(|| panic!("no batches for entity '{}'", entity.name));

        if let knit::core::CountSpec::Fixed(expected) = &entity.count {
            // Count only "full" batches (those with all entity fields),
            // excluding deferred FK patch batches which have fewer columns.
            let field_count = entity.fields.len();
            let full_rows: usize = entity_batches
                .iter()
                .filter(|b| b.num_columns() >= field_count)
                .map(|b| b.num_rows())
                .sum();
            assert!(
                full_rows as u64 >= *expected,
                "entity '{}': expected at least {expected} full rows, got {full_rows}",
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
