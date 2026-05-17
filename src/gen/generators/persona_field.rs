//! Persona-field generator — outputs actor persona trait values.
//!
//! For each row, reads the actor FK column from batch columns, maps the PK
//! to an actor index via the reverse map, then returns the actor's trait value
//! from the actor pool.

use std::collections::HashMap;
use std::sync::Arc;

use arrow::array::{
    Array, ArrayRef, BooleanArray, Float64Array, Int32Array, Int64Array, StringArray,
};
use arrow::datatypes::DataType;
use rand::RngCore;

use crate::gen::actor_pool::ActorPool;
use crate::gen::context::GenContext;
use crate::gen::traits::FieldGenerator;

/// Generates field values from per-actor persona traits.
///
/// Each row looks up the actor PK from the FK column, resolves the actor index
/// via the PK reverse map, and returns the corresponding trait value.
pub struct PersonaFieldGenerator {
    /// Actor pool to look up traits.
    actor_pool: Arc<ActorPool>,
    /// PK → actor index reverse map for the actor entity.
    pk_reverse_map: Arc<HashMap<i64, usize>>,
    /// Trait name to look up (e.g. `"activity_rate"`).
    trait_name: String,
    /// Actor entity name.
    actor_entity: String,
    /// FK field name in the current entity that references the actor.
    actor_field: String,
    /// Output data type (matches the field's declared type).
    output_data_type: DataType,
}

impl PersonaFieldGenerator {
    /// Create a new persona field generator.
    pub fn new(
        actor_pool: Arc<ActorPool>,
        pk_reverse_map: Arc<HashMap<i64, usize>>,
        trait_name: String,
        actor_entity: String,
        actor_field: String,
        output_data_type: DataType,
    ) -> Self {
        Self {
            actor_pool,
            pk_reverse_map,
            trait_name,
            actor_entity,
            actor_field,
            output_data_type,
        }
    }
}

impl FieldGenerator for PersonaFieldGenerator {
    fn generate(&self, _rng: &mut dyn RngCore, count: usize, ctx: &GenContext) -> ArrayRef {
        // Read actor FK column from batch
        let actor_col = ctx.batch_columns.get(&self.actor_field);

        let actor_pks: Vec<Option<i64>> = if let Some(col) = actor_col {
            if let Some(i64_arr) = col.as_any().downcast_ref::<Int64Array>() {
                (0..i64_arr.len())
                    .map(|i| {
                        if i64_arr.is_null(i) {
                            None
                        } else {
                            Some(i64_arr.value(i))
                        }
                    })
                    .collect()
            } else {
                vec![None; count]
            }
        } else {
            vec![None; count]
        };

        // Look up trait values per row
        let trait_values: Vec<Option<&crate::core::Value>> = actor_pks
            .iter()
            .map(|pk_opt| {
                pk_opt
                    .and_then(|pk| self.pk_reverse_map.get(&pk))
                    .and_then(|&idx| {
                        self.actor_pool
                            .get_trait(&self.actor_entity, idx, &self.trait_name)
                    })
            })
            .collect();

        // Convert to Arrow array matching the output type
        match &self.output_data_type {
            DataType::Float64 => {
                let values: Vec<Option<f64>> = trait_values
                    .iter()
                    .map(|v| match v {
                        Some(crate::core::Value::Float(f)) => Some(*f),
                        Some(crate::core::Value::Int(i)) => Some(*i as f64),
                        _ => None,
                    })
                    .collect();
                Arc::new(Float64Array::from(values)) as ArrayRef
            }
            DataType::Int64 => {
                let values: Vec<Option<i64>> = trait_values
                    .iter()
                    .map(|v| match v {
                        Some(crate::core::Value::Int(i)) => Some(*i),
                        Some(crate::core::Value::Float(f)) => Some(*f as i64),
                        _ => None,
                    })
                    .collect();
                Arc::new(Int64Array::from(values)) as ArrayRef
            }
            DataType::Int32 => {
                let values: Vec<Option<i32>> = trait_values
                    .iter()
                    .map(|v| match v {
                        Some(crate::core::Value::Int(i)) => Some(*i as i32),
                        Some(crate::core::Value::Float(f)) => Some(*f as i32),
                        _ => None,
                    })
                    .collect();
                Arc::new(Int32Array::from(values)) as ArrayRef
            }
            DataType::Boolean => {
                let values: Vec<Option<bool>> = trait_values
                    .iter()
                    .map(|v| match v {
                        Some(crate::core::Value::Bool(b)) => Some(*b),
                        Some(crate::core::Value::Int(i)) => Some(*i != 0),
                        Some(crate::core::Value::Float(f)) => Some(*f != 0.0),
                        _ => None,
                    })
                    .collect();
                Arc::new(BooleanArray::from(values)) as ArrayRef
            }
            _ => {
                // Default: stringify all values as Utf8
                let values: Vec<Option<String>> = trait_values
                    .iter()
                    .map(|v| match v {
                        Some(crate::core::Value::String(s)) => Some(s.clone()),
                        Some(crate::core::Value::Float(f)) => Some(f.to_string()),
                        Some(crate::core::Value::Int(i)) => Some(i.to_string()),
                        Some(crate::core::Value::Bool(b)) => Some(b.to_string()),
                        _ => None,
                    })
                    .collect();
                Arc::new(StringArray::from(
                    values
                        .iter()
                        .map(|v| v.as_deref())
                        .collect::<Vec<Option<&str>>>(),
                )) as ArrayRef
            }
        }
    }

    fn output_type(&self) -> DataType {
        self.output_data_type.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gen::actor_pool::ActorPool;
    use crate::plan::{ActorEntityPool, ActorPoolPlan, PersonaWeight};
    use std::collections::BTreeMap;

    fn make_pool_and_reverse_map() -> (Arc<ActorPool>, Arc<HashMap<i64, usize>>) {
        use crate::core::Value;
        let plan = ActorPoolPlan {
            pools: vec![ActorEntityPool {
                entity_name: "users".into(),
                actor_count: 3,
                persona_weights: vec![PersonaWeight {
                    name: "power".into(),
                    weight: 1.0,
                    traits: {
                        let mut m = BTreeMap::new();
                        m.insert("activity_rate".into(), Value::Float(5.0));
                        m.insert("label".into(), Value::Float(1.0));
                        m
                    },
                }],
            }],
            graph_plans: vec![],
        };
        let pool = Arc::new(ActorPool::from_plan(&plan, 42));
        let mut reverse = HashMap::new();
        // actor 0 → PK 100, actor 1 → PK 200, actor 2 → PK 300
        reverse.insert(100, 0);
        reverse.insert(200, 1);
        reverse.insert(300, 2);
        (pool, Arc::new(reverse))
    }

    #[test]
    fn float_trait_to_float64() {
        let (pool, rev) = make_pool_and_reverse_map();
        let gen = PersonaFieldGenerator::new(
            pool,
            rev,
            "activity_rate".into(),
            "users".into(),
            "user_id".into(),
            DataType::Float64,
        );

        let mut batch_columns = HashMap::new();
        let user_ids = Arc::new(Int64Array::from(vec![100, 200, 300])) as ArrayRef;
        batch_columns.insert("user_id".to_string(), user_ids);

        let ctx = GenContext::new(&batch_columns, 0, 0, 1, "posts");
        let mut rng = rand::rng();
        let result = gen.generate(&mut rng, 3, &ctx);

        let f64_arr = result.as_any().downcast_ref::<Float64Array>().unwrap();
        assert_eq!(f64_arr.len(), 3);
        // All actors have the same "power" persona with activity_rate = 5.0
        for i in 0..3 {
            assert!(!f64_arr.is_null(i));
            assert_eq!(f64_arr.value(i), 5.0);
        }
    }

    #[test]
    fn null_actor_fk_produces_null() {
        let (pool, rev) = make_pool_and_reverse_map();
        let gen = PersonaFieldGenerator::new(
            pool,
            rev,
            "activity_rate".into(),
            "users".into(),
            "user_id".into(),
            DataType::Float64,
        );

        let mut batch_columns = HashMap::new();
        let user_ids = Arc::new(Int64Array::from(vec![Some(100), None, Some(300)])) as ArrayRef;
        batch_columns.insert("user_id".to_string(), user_ids);

        let ctx = GenContext::new(&batch_columns, 0, 0, 1, "posts");
        let mut rng = rand::rng();
        let result = gen.generate(&mut rng, 3, &ctx);

        let f64_arr = result.as_any().downcast_ref::<Float64Array>().unwrap();
        assert!(!f64_arr.is_null(0));
        assert!(f64_arr.is_null(1)); // null FK → null output
        assert!(!f64_arr.is_null(2));
    }

    #[test]
    fn missing_trait_produces_null() {
        let (pool, rev) = make_pool_and_reverse_map();
        let gen = PersonaFieldGenerator::new(
            pool,
            rev,
            "nonexistent_trait".into(),
            "users".into(),
            "user_id".into(),
            DataType::Float64,
        );

        let mut batch_columns = HashMap::new();
        let user_ids = Arc::new(Int64Array::from(vec![100, 200])) as ArrayRef;
        batch_columns.insert("user_id".to_string(), user_ids);

        let ctx = GenContext::new(&batch_columns, 0, 0, 1, "posts");
        let mut rng = rand::rng();
        let result = gen.generate(&mut rng, 2, &ctx);

        let f64_arr = result.as_any().downcast_ref::<Float64Array>().unwrap();
        // Missing trait → null
        assert!(f64_arr.is_null(0));
        assert!(f64_arr.is_null(1));
    }
}
