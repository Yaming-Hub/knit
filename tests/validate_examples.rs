//! Validate that every example schema in `examples/` parses and validates
//! without errors.

mod common;
use common::example_schemas;
use knit::schema::{parse_toml_file, validate};

#[test]
fn all_example_schemas_parse_successfully() {
    let schemas = example_schemas();
    assert!(
        !schemas.is_empty(),
        "no .weave.toml files found in examples/"
    );

    for path in &schemas {
        let stem = path.file_name().unwrap().to_string_lossy();
        let model = parse_toml_file(path).unwrap_or_else(|e| panic!("{stem}: parse error: {e}"));

        // Smoke-check: the model name should be non-empty.
        assert!(
            !model.name.is_empty(),
            "{stem}: model name must not be empty"
        );
    }
}

#[test]
fn all_example_schemas_validate_successfully() {
    let schemas = example_schemas();
    for path in &schemas {
        let stem = path.file_name().unwrap().to_string_lossy();
        let model = parse_toml_file(path).unwrap_or_else(|e| panic!("{stem}: parse error: {e}"));
        let errors = validate(&model);
        assert!(errors.is_empty(), "{stem}: validation errors: {errors:?}");
    }
}

#[test]
fn ecommerce_schema_has_expected_entities() {
    let schemas = example_schemas();
    let ecommerce = schemas
        .iter()
        .find(|p| {
            p.file_name()
                .and_then(|s| s.to_str())
                .is_some_and(|n| n.starts_with("ecommerce"))
        })
        .expect("ecommerce.weave.toml not found");

    let model = parse_toml_file(ecommerce).unwrap();
    let names: Vec<&str> = model.entities.iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains(&"users"), "missing entity: users");
    assert!(names.contains(&"products"), "missing entity: products");
    assert!(names.contains(&"orders"), "missing entity: orders");
    assert!(names.contains(&"reviews"), "missing entity: reviews");
}

#[test]
fn iot_schema_has_expected_entities() {
    let schemas = example_schemas();
    let iot = schemas
        .iter()
        .find(|p| {
            p.file_name()
                .and_then(|s| s.to_str())
                .is_some_and(|n| n.starts_with("iot"))
        })
        .expect("iot_sensors.weave.toml not found");

    let model = parse_toml_file(iot).unwrap();
    let names: Vec<&str> = model.entities.iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains(&"devices"), "missing entity: devices");
    assert!(names.contains(&"readings"), "missing entity: readings");
    assert!(names.contains(&"alerts"), "missing entity: alerts");
}

#[test]
fn server_logs_schema_has_expected_entities() {
    let schemas = example_schemas();
    let logs = schemas
        .iter()
        .find(|p| {
            p.file_name()
                .and_then(|s| s.to_str())
                .is_some_and(|n| n.starts_with("server_logs"))
        })
        .expect("server_logs.weave.toml not found");

    let model = parse_toml_file(logs).unwrap();
    let names: Vec<&str> = model.entities.iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains(&"servers"), "missing entity: servers");
    assert!(names.contains(&"requests"), "missing entity: requests");
    assert!(names.contains(&"errors"), "missing entity: errors");
}

#[test]
fn financial_schema_has_expected_entities() {
    let schemas = example_schemas();
    let fin = schemas
        .iter()
        .find(|p| {
            p.file_name()
                .and_then(|s| s.to_str())
                .is_some_and(|n| n.starts_with("financial"))
        })
        .expect("financial.weave.toml not found");

    let model = parse_toml_file(fin).unwrap();
    let names: Vec<&str> = model.entities.iter().map(|e| e.name.as_str()).collect();
    assert!(names.contains(&"accounts"), "missing entity: accounts");
    assert!(
        names.contains(&"transactions"),
        "missing entity: transactions"
    );
}

#[test]
fn hr_org_schema_has_expected_entities() {
    let schemas = example_schemas();
    let hr = schemas
        .iter()
        .find(|p| {
            p.file_name()
                .and_then(|s| s.to_str())
                .is_some_and(|n| n.starts_with("hr_org"))
        })
        .expect("hr_org.weave.toml not found");

    let model = parse_toml_file(hr).unwrap();
    let names: Vec<&str> = model.entities.iter().map(|e| e.name.as_str()).collect();
    assert!(
        names.contains(&"departments"),
        "missing entity: departments"
    );
    assert!(names.contains(&"employees"), "missing entity: employees");
}

#[test]
fn all_schemas_have_relationships() {
    let schemas = example_schemas();
    for path in &schemas {
        let stem = path.file_name().unwrap().to_string_lossy();
        // cli_test is a minimal utility schema without relationships
        // nested_objects demonstrates struct/object types without relationships
        if stem.contains("cli_test") || stem.contains("nested_objects") {
            continue;
        }
        let model = parse_toml_file(path).unwrap();
        assert!(
            !model.relationships.is_empty() || !model.actor_relationships.is_empty(),
            "{stem}: expected at least one relationship or actor_relationship"
        );
    }
}

#[test]
fn relationship_entity_references_are_valid() {
    let schemas = example_schemas();
    for path in &schemas {
        let stem = path.file_name().unwrap().to_string_lossy();
        let model = parse_toml_file(path).unwrap();
        let entity_names: Vec<&str> = model.entities.iter().map(|e| e.name.as_str()).collect();

        for rel in &model.relationships {
            assert!(
                entity_names.contains(&rel.from.as_str()),
                "{stem}: relationship '{}' references unknown from-entity '{}'",
                rel.name,
                rel.from
            );
            assert!(
                entity_names.contains(&rel.to.as_str()),
                "{stem}: relationship '{}' references unknown to-entity '{}'",
                rel.name,
                rel.to
            );
        }
    }
}
