//! RNG tree construction for deterministic seed derivation.
//!
//! The RNG tree provides hierarchical deterministic seeding so that:
//! - Adding/removing entities or fields does not affect existing seeds
//! - Partition seeds are independent of thread count or execution order
//! - The same schema always produces identical seeds on any platform
//!
//! Derivation chain: `global_seed → entity_seed → field_seed → partition_seed`
//! using SipHash for fast, high-quality mixing.

use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};

use siphasher::sip::SipHasher;

use crate::types::{EntitySeedNode, FieldSeedNode, RngTree};

/// Derive a child seed from a parent seed and a key using SipHash.
///
/// This is the fundamental building block of the seed hierarchy. It produces
/// a deterministic, well-distributed 64-bit seed from any parent seed and
/// arbitrary key bytes (entity name, field name, or partition ID).
pub fn derive_seed(parent_seed: u64, key: &[u8]) -> u64 {
    let mut hasher = SipHasher::new_with_keys(parent_seed, parent_seed.wrapping_mul(0x517cc1b727220a95));
    key.hash(&mut hasher);
    hasher.finish()
}

/// Build the full RNG tree from a global seed and entity/field/partition info.
///
/// Each entity gets a seed derived from `global_seed + entity_name`. Each field
/// gets a seed derived from its entity seed + field name. Each partition gets a
/// seed derived from its field seed + partition index.
///
/// # Arguments
/// - `global_seed` — The schema's top-level seed (from `model.seed`)
/// - `entities` — Tuples of `(entity_name, field_names, num_partitions)`
pub fn build_rng_tree(
    global_seed: u64,
    entities: &[(String, Vec<String>, u32)], // (entity_name, field_names, num_partitions)
) -> RngTree {
    let mut entity_nodes = BTreeMap::new();

    for (entity_name, field_names, num_partitions) in entities {
        let entity_seed = derive_seed(global_seed, entity_name.as_bytes());

        let mut field_seeds = BTreeMap::new();
        for field_name in field_names {
            let field_seed = derive_seed(entity_seed, field_name.as_bytes());

            let partition_seeds: Vec<u64> = (0..*num_partitions)
                .map(|pid| derive_seed(field_seed, &pid.to_le_bytes()))
                .collect();

            field_seeds.insert(
                field_name.clone(),
                FieldSeedNode {
                    field_seed,
                    partition_seeds,
                },
            );
        }

        entity_nodes.insert(
            entity_name.clone(),
            EntitySeedNode {
                entity_seed,
                field_seeds,
            },
        );
    }

    RngTree {
        global_seed,
        entity_nodes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_derive_seed_deterministic() {
        let s1 = derive_seed(42, b"users");
        let s2 = derive_seed(42, b"users");
        assert_eq!(s1, s2);
    }

    #[test]
    fn test_derive_seed_unique() {
        let s1 = derive_seed(42, b"users");
        let s2 = derive_seed(42, b"orders");
        assert_ne!(s1, s2);
    }

    #[test]
    fn test_rng_tree_seeds_unique() {
        let entities = vec![
            ("users".to_string(), vec!["id".to_string(), "name".to_string()], 2),
            ("orders".to_string(), vec!["id".to_string(), "total".to_string()], 3),
        ];
        let tree = build_rng_tree(42, &entities);

        // Collect all seeds.
        let mut all_seeds = Vec::new();
        for node in tree.entity_nodes.values() {
            all_seeds.push(node.entity_seed);
            for fnode in node.field_seeds.values() {
                all_seeds.push(fnode.field_seed);
                all_seeds.extend(&fnode.partition_seeds);
            }
        }

        // All seeds should be unique.
        let count = all_seeds.len();
        all_seeds.sort();
        all_seeds.dedup();
        assert_eq!(all_seeds.len(), count, "all seeds should be unique");
    }
}
