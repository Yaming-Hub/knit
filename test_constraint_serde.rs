// Test to verify TableConstraint serialization with flattened tagged enum
use serde::{Serialize, Deserialize};
use serde_json;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Constraint {
    Unique { fields: Vec<String> },
    NotNull { fields: Vec<String> },
    Check { expr: String },
    Range {
        field: String,
        min: Option<i64>,
        max: Option<i64>,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableConstraint {
    pub table: String,
    #[serde(flatten)]
    pub constraint: Constraint,
}

fn main() {
    // Test 1: Serialize TableConstraint with Unique constraint
    let tc = TableConstraint {
        table: "users".into(),
        constraint: Constraint::Unique {
            fields: vec!["email".into()],
        },
    };
    
    let json = serde_json::to_string_pretty(&tc).expect("serialize failed");
    println!("Serialized TableConstraint:");
    println!("{}\n", json);
    
    // Test 2: Deserialize it back
    let deserialized: TableConstraint = serde_json::from_str(&json).expect("deserialize failed");
    println!("Deserialized: {:?}\n", deserialized);
    
    // Test 3: Verify round-trip
    assert_eq!(tc, deserialized, "Round-trip failed!");
    println!("✓ Round-trip successful");
    
    // Test 4: Try Range constraint
    let tc2 = TableConstraint {
        table: "orders".into(),
        constraint: Constraint::Range {
            field: "total".into(),
            min: Some(0),
            max: Some(10000),
        },
    };
    
    let json2 = serde_json::to_string_pretty(&tc2).expect("serialize failed");
    println!("\nSerialized Range constraint:");
    println!("{}\n", json2);
    
    let deserialized2: TableConstraint = serde_json::from_str(&json2).expect("deserialize failed");
    assert_eq!(tc2, deserialized2, "Round-trip failed for Range!");
    println!("✓ Round-trip successful for Range");
    
    // Test 5: Deserialize JSON with table and flattened fields
    let json_input = r#"{"table": "products", "type": "not_null", "fields": ["name", "price"]}"#;
    let parsed: TableConstraint = serde_json::from_str(json_input).expect("parse failed");
    println!("\nParsed from JSON: {:?}", parsed);
    
    println!("\n✅ All tests passed!");
}
