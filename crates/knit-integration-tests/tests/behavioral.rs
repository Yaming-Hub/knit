//! Behavioral modeling integration tests — verify persona-weighted FK
//! generation produces non-uniform distributions consistent with actor traits.

use std::collections::{HashMap, HashSet};

use arrow::array::{Array, Int64Array};
use arrow::record_batch::RecordBatch;
use knit_integration_tests::{example_schemas, generate_from_file, total_rows};

/// Collect all values of an Int64 column across batches.
fn collect_i64_column(batches: &[RecordBatch], column: &str) -> Vec<i64> {
    let mut values = Vec::new();
    for batch in batches {
        let idx = batch
            .schema()
            .index_of(column)
            .unwrap_or_else(|_| panic!("column '{column}' not found"));
        let arr = batch.column(idx);
        let i64_arr = arr
            .as_any()
            .downcast_ref::<Int64Array>()
            .unwrap_or_else(|| panic!("column '{column}' is not Int64"));
        for i in 0..i64_arr.len() {
            if !i64_arr.is_null(i) {
                values.push(i64_arr.value(i));
            }
        }
    }
    values
}

/// Find the social_platform example schema.
fn social_platform_path() -> std::path::PathBuf {
    example_schemas()
        .into_iter()
        .find(|p| {
            p.file_name()
                .and_then(|s| s.to_str())
                .is_some_and(|n| n.contains("social_platform"))
        })
        .expect("social_platform.weave.toml not found in examples/")
}

#[test]
fn social_platform_generates_all_entities() {
    let batches = generate_from_file(&social_platform_path());

    assert!(batches.contains_key("users"), "missing 'users' entity");
    assert!(batches.contains_key("posts"), "missing 'posts' entity");
    assert!(batches.contains_key("comments"), "missing 'comments' entity");
    assert!(
        batches.contains_key("direct_messages"),
        "missing 'direct_messages' entity"
    );

    assert_eq!(total_rows(&batches["users"]), 500);
    assert_eq!(total_rows(&batches["posts"]), 5000);
    assert_eq!(total_rows(&batches["comments"]), 15000);
    assert_eq!(total_rows(&batches["direct_messages"]), 8000);
}

#[test]
fn social_platform_fk_referential_integrity() {
    let batches = generate_from_file(&social_platform_path());

    let user_ids: HashSet<i64> = collect_i64_column(&batches["users"], "id")
        .into_iter()
        .collect();
    let post_ids: HashSet<i64> = collect_i64_column(&batches["posts"], "id")
        .into_iter()
        .collect();

    // posts.author_id → users.id
    for author_id in collect_i64_column(&batches["posts"], "author_id") {
        assert!(
            user_ids.contains(&author_id),
            "posts.author_id {author_id} not in users.id"
        );
    }

    // comments.post_id → posts.id
    for post_id in collect_i64_column(&batches["comments"], "post_id") {
        assert!(
            post_ids.contains(&post_id),
            "comments.post_id {post_id} not in posts.id"
        );
    }

    // comments.author_id → users.id
    for author_id in collect_i64_column(&batches["comments"], "author_id") {
        assert!(
            user_ids.contains(&author_id),
            "comments.author_id {author_id} not in users.id"
        );
    }

    // direct_messages.sender_id → users.id
    for sender_id in collect_i64_column(&batches["direct_messages"], "sender_id") {
        assert!(
            user_ids.contains(&sender_id),
            "direct_messages.sender_id {sender_id} not in users.id"
        );
    }

    // direct_messages.receiver_id → users.id
    for receiver_id in collect_i64_column(&batches["direct_messages"], "receiver_id") {
        assert!(
            user_ids.contains(&receiver_id),
            "direct_messages.receiver_id {receiver_id} not in users.id"
        );
    }
}

#[test]
fn social_platform_persona_weighted_distribution() {
    let batches = generate_from_file(&social_platform_path());

    // Count posts per author
    let author_ids = collect_i64_column(&batches["posts"], "author_id");
    let mut author_counts: HashMap<i64, u32> = HashMap::new();
    for id in &author_ids {
        *author_counts.entry(*id).or_insert(0) += 1;
    }

    let mut sorted_counts: Vec<u32> = author_counts.values().copied().collect();
    sorted_counts.sort_unstable_by(|a, b| b.cmp(a));

    // With persona weighting (power_user activity=80 vs lurker activity=3),
    // the distribution should be heavily skewed.
    // Top 20% of authors (100 users) should produce much more than 20% of posts.
    let top_100_posts: u32 = sorted_counts.iter().take(100).sum();
    let top_100_frac = top_100_posts as f64 / 5000.0;

    assert!(
        top_100_frac > 0.50,
        "expected top-100 authors to produce >50% of posts; got {:.1}%",
        top_100_frac * 100.0
    );

    // Bottom 100 authors should produce much less
    let bottom_100_posts: u32 = sorted_counts.iter().rev().take(100).sum();
    let bottom_100_frac = bottom_100_posts as f64 / 5000.0;

    assert!(
        bottom_100_frac < 0.15,
        "expected bottom-100 authors to produce <15% of posts; got {:.1}%",
        bottom_100_frac * 100.0
    );

    // Diversity check: ensure a reasonable spread of distinct authors
    let distinct_authors = author_counts.len();
    assert!(
        distinct_authors >= 300,
        "expected at least 300 distinct post authors; got {distinct_authors}"
    );
    assert!(
        distinct_authors <= 500,
        "expected at most 500 distinct post authors; got {distinct_authors}"
    );
}

#[test]
fn social_platform_deterministic_output() {
    // Generate twice with same seed — should produce identical results
    let batches1 = generate_from_file(&social_platform_path());
    let batches2 = generate_from_file(&social_platform_path());

    // Compare all entities
    for entity in ["users", "posts", "comments", "direct_messages"] {
        let b1 = &batches1[entity];
        let b2 = &batches2[entity];
        assert_eq!(
            total_rows(b1),
            total_rows(b2),
            "row count mismatch for {entity}"
        );
        // Compare first batch column-by-column
        let batch1 = &b1[0];
        let batch2 = &b2[0];
        for col_idx in 0..batch1.num_columns() {
            let col1 = batch1.column(col_idx);
            let col2 = batch2.column(col_idx);
            assert_eq!(
                col1.as_ref(),
                col2.as_ref(),
                "column {} mismatch in {entity}",
                batch1.schema().field(col_idx).name()
            );
        }
    }
}
