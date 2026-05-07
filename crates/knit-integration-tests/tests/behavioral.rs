//! Behavioral modeling integration tests — verify persona-weighted FK
//! generation, temporal ordering, inter-event gaps, hour bias, and graph-aware
//! FK generation all produce correct behavioral properties.

use std::collections::{HashMap, HashSet};

use arrow::array::{Array, Int64Array, TimestampMillisecondArray};
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

/// Collect all timestamp values from a column across batches, normalized to milliseconds.
/// Handles all Arrow timestamp types: Second, Millisecond, Microsecond, Nanosecond.
fn collect_timestamp_column(batches: &[RecordBatch], column: &str) -> Vec<i64> {
    use arrow::array::{
        TimestampMicrosecondArray, TimestampNanosecondArray, TimestampSecondArray,
    };
    let mut values = Vec::new();
    for batch in batches {
        let idx = batch
            .schema()
            .index_of(column)
            .unwrap_or_else(|_| panic!("column '{column}' not found"));
        let arr = batch.column(idx);
        if let Some(ts_arr) = arr.as_any().downcast_ref::<TimestampMillisecondArray>() {
            for i in 0..ts_arr.len() {
                if !ts_arr.is_null(i) {
                    values.push(ts_arr.value(i));
                }
            }
        } else if let Some(ts_arr) = arr.as_any().downcast_ref::<TimestampMicrosecondArray>() {
            for i in 0..ts_arr.len() {
                if !ts_arr.is_null(i) {
                    values.push(ts_arr.value(i) / 1000);
                }
            }
        } else if let Some(ts_arr) = arr.as_any().downcast_ref::<TimestampNanosecondArray>() {
            for i in 0..ts_arr.len() {
                if !ts_arr.is_null(i) {
                    values.push(ts_arr.value(i) / 1_000_000);
                }
            }
        } else if let Some(ts_arr) = arr.as_any().downcast_ref::<TimestampSecondArray>() {
            for i in 0..ts_arr.len() {
                if !ts_arr.is_null(i) {
                    values.push(ts_arr.value(i) * 1000);
                }
            }
        } else if let Some(i64_arr) = arr.as_any().downcast_ref::<Int64Array>() {
            for i in 0..i64_arr.len() {
                if !i64_arr.is_null(i) {
                    values.push(i64_arr.value(i));
                }
            }
        } else {
            panic!("column '{column}' has unsupported type: {:?}", arr.data_type());
        }
    }
    values
}

/// Collect (fk, timestamp) pairs from batches for per-actor analysis.
fn collect_fk_timestamp_pairs(
    batches: &[RecordBatch],
    fk_col: &str,
    ts_col: &str,
) -> Vec<(i64, i64)> {
    use arrow::array::{
        TimestampMicrosecondArray, TimestampNanosecondArray, TimestampSecondArray,
    };
    let mut pairs = Vec::new();
    for batch in batches {
        let fk_idx = batch.schema().index_of(fk_col).unwrap();
        let ts_idx = batch.schema().index_of(ts_col).unwrap();
        let fk_arr = batch.column(fk_idx).as_any().downcast_ref::<Int64Array>().unwrap();
        let ts_col_arr = batch.column(ts_idx);
        for i in 0..batch.num_rows() {
            if fk_arr.is_null(i) {
                continue;
            }
            let ts_val = if let Some(ts_arr) =
                ts_col_arr.as_any().downcast_ref::<TimestampMillisecondArray>()
            {
                if ts_arr.is_null(i) { continue; }
                ts_arr.value(i)
            } else if let Some(ts_arr) =
                ts_col_arr.as_any().downcast_ref::<TimestampMicrosecondArray>()
            {
                if ts_arr.is_null(i) { continue; }
                ts_arr.value(i) / 1000
            } else if let Some(ts_arr) =
                ts_col_arr.as_any().downcast_ref::<TimestampNanosecondArray>()
            {
                if ts_arr.is_null(i) { continue; }
                ts_arr.value(i) / 1_000_000
            } else if let Some(ts_arr) =
                ts_col_arr.as_any().downcast_ref::<TimestampSecondArray>()
            {
                if ts_arr.is_null(i) { continue; }
                ts_arr.value(i) * 1000
            } else {
                continue;
            };
            pairs.push((fk_arr.value(i), ts_val));
        }
    }
    pairs
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
    // Posts, comments, and direct_messages use activity_count — their row
    // counts are computed dynamically from persona weights × actor count.
    // posts: 0.15×500×12 + 0.55×500×3 + 0.30×500×0.2 = 1755
    assert_eq!(total_rows(&batches["posts"]), 1755);
    // comments & direct_messages: 0.15×500×80 + 0.55×500×20 + 0.30×500×3 = 11950
    assert_eq!(total_rows(&batches["comments"]), 11950);
    assert_eq!(total_rows(&batches["direct_messages"]), 11950);
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
    let total_posts = author_ids.len() as f64;
    let top_100_posts: u32 = sorted_counts.iter().take(100).sum();
    let top_100_frac = top_100_posts as f64 / total_posts;

    assert!(
        top_100_frac > 0.50,
        "expected top-100 authors to produce >50% of posts; got {:.1}%",
        top_100_frac * 100.0
    );

    // Bottom 100 authors should produce much less
    let bottom_100_posts: u32 = sorted_counts.iter().rev().take(100).sum();
    let bottom_100_frac = bottom_100_posts as f64 / total_posts;

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

// ── Temporal Ordering Tests ─────────────────────────────────────────────

#[test]
fn social_platform_activity_after_signup() {
    let batches = generate_from_file(&social_platform_path());

    // Build user_id → signup_date map
    let user_ids = collect_i64_column(&batches["users"], "id");
    let signup_dates = collect_timestamp_column(&batches["users"], "signup_date");
    let signup_map: HashMap<i64, i64> = user_ids
        .into_iter()
        .zip(signup_dates.into_iter())
        .collect();

    // Posts: created_at >= author's signup_date
    let post_pairs = collect_fk_timestamp_pairs(&batches["posts"], "author_id", "created_at");
    let mut violations = 0;
    for (author_id, created_at) in &post_pairs {
        if let Some(&signup) = signup_map.get(author_id) {
            if *created_at < signup {
                violations += 1;
            }
        }
    }
    assert_eq!(
        violations, 0,
        "found {violations} posts with created_at before author's signup_date"
    );

    // Comments: created_at >= author's signup_date
    let comment_pairs =
        collect_fk_timestamp_pairs(&batches["comments"], "author_id", "created_at");
    let mut violations = 0;
    for (author_id, created_at) in &comment_pairs {
        if let Some(&signup) = signup_map.get(author_id) {
            if *created_at < signup {
                violations += 1;
            }
        }
    }
    assert_eq!(
        violations, 0,
        "found {violations} comments with created_at before author's signup_date"
    );

    // Direct messages: sent_at >= sender's signup_date
    let dm_pairs =
        collect_fk_timestamp_pairs(&batches["direct_messages"], "sender_id", "sent_at");
    let mut violations = 0;
    for (sender_id, sent_at) in &dm_pairs {
        if let Some(&signup) = signup_map.get(sender_id) {
            if *sent_at < signup {
                violations += 1;
            }
        }
    }
    assert_eq!(
        violations, 0,
        "found {violations} DMs with sent_at before sender's signup_date"
    );
}

#[test]
fn social_platform_inter_event_gaps() {
    let batches = generate_from_file(&social_platform_path());
    let min_gap_ms: i64 = 60_000; // default 1 minute

    // Check posts: per-author timestamps should have >= 60s gaps
    let post_pairs = collect_fk_timestamp_pairs(&batches["posts"], "author_id", "created_at");
    let mut per_author: HashMap<i64, Vec<i64>> = HashMap::new();
    for (author, ts) in &post_pairs {
        per_author.entry(*author).or_default().push(*ts);
    }

    let mut violations = 0;
    for timestamps in per_author.values_mut() {
        timestamps.sort_unstable();
        for window in timestamps.windows(2) {
            if window[1] - window[0] < min_gap_ms {
                violations += 1;
            }
        }
    }
    assert_eq!(
        violations, 0,
        "found {violations} post timestamp pairs with gap < {min_gap_ms}ms"
    );

    // Check direct_messages: per-sender gaps
    let dm_pairs =
        collect_fk_timestamp_pairs(&batches["direct_messages"], "sender_id", "sent_at");
    let mut per_sender: HashMap<i64, Vec<i64>> = HashMap::new();
    for (sender, ts) in &dm_pairs {
        per_sender.entry(*sender).or_default().push(*ts);
    }

    let mut violations = 0;
    for timestamps in per_sender.values_mut() {
        timestamps.sort_unstable();
        for window in timestamps.windows(2) {
            if window[1] - window[0] < min_gap_ms {
                violations += 1;
            }
        }
    }
    assert_eq!(
        violations, 0,
        "found {violations} DM timestamp pairs with gap < {min_gap_ms}ms"
    );
}

#[test]
fn social_platform_temporal_hour_bias() {
    let batches = generate_from_file(&social_platform_path());

    // Extract hours from post timestamps
    let timestamps = collect_timestamp_column(&batches["posts"], "created_at");
    let mut hour_counts = [0u32; 24];
    for ts in &timestamps {
        // Convert ms to hours within day
        let hour = (((*ts % (24 * 3_600_000)) + 24 * 3_600_000) % (24 * 3_600_000) / 3_600_000) as usize;
        if hour < 24 {
            hour_counts[hour] += 1;
        }
    }

    let total = timestamps.len() as f64;

    // With peak_hours of 14 (power), 20 (regular), 22 (lurker), activity
    // should be heavily biased toward evening hours (14-22) and low in early morning.
    // The 4am-8am window should be sparse (well below uniform 4/24 ≈ 16.7%).
    let early_morning: u32 = hour_counts[4..8].iter().sum();
    let early_frac = early_morning as f64 / total;
    assert!(
        early_frac < 0.12,
        "expected <12% activity during 4am-8am; got {:.1}% (persona temporal bias not working)",
        early_frac * 100.0
    );

    // Evening window 18-23 should be above-average (personas peak 20, 22)
    let evening: u32 = hour_counts[18..24].iter().sum();
    let evening_frac = evening as f64 / total;
    assert!(
        evening_frac > 0.30,
        "expected >30% activity during 18-24h; got {:.1}% (persona temporal bias not working)",
        evening_frac * 100.0
    );
}

#[test]
fn social_platform_graph_aware_receiver_ids() {
    let batches = generate_from_file(&social_platform_path());

    // Direct messages use relationship_ref on "messages" graph.
    // receiver_id should come from graph neighbors of sender_id.
    // We can't check exact graph edges, but we can verify that the
    // sender-receiver pairs are non-random: many repeated pairs should exist
    // (friends message each other multiple times).
    let sender_ids = collect_i64_column(&batches["direct_messages"], "sender_id");
    let receiver_ids = collect_i64_column(&batches["direct_messages"], "receiver_id");

    let mut pair_counts: HashMap<(i64, i64), u32> = HashMap::new();
    for (s, r) in sender_ids.iter().zip(receiver_ids.iter()) {
        *pair_counts.entry((*s, *r)).or_insert(0) += 1;
    }

    let total_messages = sender_ids.len();
    let unique_pairs = pair_counts.len();

    // With a small-world graph (avg_degree=4), 500 users have ~1000 edges.
    // 11950 messages across ~1000 edges means ~12 messages per edge on average.
    // Unique pairs should be much less than total messages (high repetition).
    let repetition_ratio = total_messages as f64 / unique_pairs as f64;
    assert!(
        repetition_ratio > 3.0,
        "expected >3x message repetition per pair (graph-aware FK); got {:.1}x \
         ({total_messages} messages across {unique_pairs} unique pairs)",
        repetition_ratio
    );

    // Also: sender should never equal receiver (no self-messages in graph)
    let self_messages: usize = sender_ids
        .iter()
        .zip(receiver_ids.iter())
        .filter(|(s, r)| s == r)
        .count();
    assert_eq!(
        self_messages, 0,
        "found {self_messages} self-messages (sender == receiver)"
    );
}