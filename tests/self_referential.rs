//! Self-referential hierarchy tests that verify generated trees are valid.

use std::collections::{HashMap, HashSet};

use arrow::array::{Array, Int64Array};

mod common;
use common::{examples_dir, generate_from_file};

#[test]
fn hierarchy_example_generates_acyclic_valid_manager_tree() {
    let path = examples_dir().join("hierarchy.knit.toml");
    let data = generate_from_file(&path);
    let employee_batches = data.get("employee").expect("employee entity should exist");

    let mut ids = HashSet::new();
    let mut manager_by_employee = HashMap::new();
    let mut total_rows = 0usize;

    for batch in employee_batches {
        let id = batch
            .column(batch.schema().index_of("id").unwrap())
            .as_any()
            .downcast_ref::<Int64Array>()
            .expect("id should be Int64");
        let manager_id = batch
            .column(batch.schema().index_of("manager_id").unwrap())
            .as_any()
            .downcast_ref::<Int64Array>()
            .expect("manager_id should be Int64");

        for row in 0..batch.num_rows() {
            total_rows += 1;
            let employee_id = id.value(row);
            assert!(
                ids.insert(employee_id),
                "duplicate employee id {employee_id} at row {total_rows}"
            );
            let manager = if manager_id.is_null(row) {
                None
            } else {
                Some(manager_id.value(row))
            };
            manager_by_employee.insert(employee_id, manager);
        }
    }

    assert_eq!(
        total_rows, ids.len(),
        "total rows ({total_rows}) should equal unique ids ({})",
        ids.len()
    );
    assert_eq!(ids.len(), manager_by_employee.len());
    assert!(
        !ids.is_empty(),
        "hierarchy example should generate employees"
    );

    let roots: Vec<i64> = manager_by_employee
        .iter()
        .filter(|(_, manager_id)| manager_id.is_none())
        .map(|(employee_id, _)| *employee_id)
        .collect();
    assert!(!roots.is_empty(), "hierarchy should contain root employees");

    for (employee_id, manager_id) in &manager_by_employee {
        if let Some(parent_id) = manager_id {
            assert!(
                ids.contains(parent_id),
                "employee {employee_id} references missing manager {parent_id}"
            );
            assert_ne!(
                employee_id, parent_id,
                "employee {employee_id} cannot manage itself"
            );
        }
    }

    let mut max_depth = 0usize;
    for employee_id in &ids {
        let mut seen = HashSet::new();
        let mut current = Some(*employee_id);
        let mut depth = 0usize;

        while let Some(node) = current {
            assert!(
                seen.insert(node),
                "cycle detected starting from employee {employee_id} via manager {node}"
            );
            current = manager_by_employee.get(&node).copied().flatten();
            if current.is_some() {
                depth += 1;
            }
            assert!(
                depth <= 6,
                "employee {employee_id} exceeded max depth 6 with depth {depth}"
            );
        }

        max_depth = max_depth.max(depth);
    }

    assert!(
        max_depth > 0,
        "hierarchy should have at least one non-root employee"
    );
}
