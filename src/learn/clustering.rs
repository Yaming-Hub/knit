//! Persona clustering — discover behavioral personas from actor profiles.
//!
//! Uses K-means clustering with automatic K selection (silhouette score)
//! to group actors into personas. Each persona captures the centroid of
//! its cluster as behavioral traits.
//!
//! ## Algorithm
//!
//! 1. Convert [`ActorProfile`]s to normalized feature vectors
//! 2. Run K-means for K = 2..√N (capped at 10)
//! 3. Select best K using silhouette score
//! 4. Extract cluster centroids as persona trait values
//! 5. Auto-generate persona names from dominant traits

use std::collections::BTreeMap;

use crate::core::Value;
use rand::rngs::StdRng;
use rand::{RngExt, SeedableRng};

use crate::learn::behavioral::ActorProfile;

/// Maximum K to try during auto-selection.
const MAX_K: usize = 10;

/// Minimum cluster size to consider valid.
const MIN_CLUSTER_SIZE: usize = 2;

/// Result of persona clustering.
#[derive(Debug, Clone)]
pub struct ClusteringResult {
    /// Discovered personas with names, weights, and traits.
    pub personas: Vec<PersonaSpec>,
    /// Assignment of each actor to a persona (index into `personas`).
    pub assignments: Vec<(String, usize)>,
    /// The K that was selected.
    pub k: usize,
    /// Silhouette score for the selected K.
    pub silhouette_score: f64,
}

/// A discovered persona specification ready for emission.
#[derive(Debug, Clone)]
pub struct PersonaSpec {
    /// Auto-generated persona name.
    pub name: String,
    /// Fraction of actors in this persona.
    pub weight: f64,
    /// Behavioral traits as key-value pairs.
    pub traits: BTreeMap<String, Value>,
}

/// Configuration for the clustering algorithm.
#[derive(Debug, Clone)]
pub struct ClusteringConfig {
    /// Random seed for reproducibility.
    pub seed: u64,
    /// Maximum K-means iterations per run.
    pub max_iterations: usize,
    /// Convergence threshold (centroid movement).
    pub convergence_threshold: f64,
    /// Minimum number of actors required for clustering.
    pub min_actors: usize,
}

impl Default for ClusteringConfig {
    fn default() -> Self {
        Self {
            seed: 42,
            max_iterations: 100,
            convergence_threshold: 1e-6,
            min_actors: 4,
        }
    }
}

/// Convert actor profiles into normalized feature vectors.
///
/// Features extracted per profile:
/// - 24 hourly activity values (already normalized)
/// - 7 daily activity values (already normalized)
/// - activity_count (log-scaled, then normalized)
/// - active_span_days (normalized)
///
/// Returns (feature_matrix, feature_names) where each row is one actor.
pub fn profiles_to_features(profiles: &[ActorProfile]) -> (Vec<Vec<f64>>, Vec<String>) {
    if profiles.is_empty() {
        return (Vec::new(), Vec::new());
    }

    let mut feature_names: Vec<String> = Vec::new();
    for h in 0..24 {
        feature_names.push(format!("hour_{h:02}"));
    }
    for d in 0..7 {
        feature_names.push(format!("day_{d}"));
    }
    feature_names.push("log_activity_count".into());
    feature_names.push("active_span_days".into());

    let mut raw_matrix: Vec<Vec<f64>> = Vec::with_capacity(profiles.len());
    for profile in profiles {
        let mut row = Vec::with_capacity(33);
        row.extend_from_slice(&profile.active_hours);
        row.extend_from_slice(&profile.active_days);
        row.push((profile.activity_count as f64 + 1.0).ln());
        row.push(profile.active_span_days);
        raw_matrix.push(row);
    }

    // Z-score normalize each feature column
    let num_features = raw_matrix[0].len();
    let n = raw_matrix.len() as f64;

    for col in 0..num_features {
        let mean: f64 = raw_matrix.iter().map(|r| r[col]).sum::<f64>() / n;
        let variance: f64 = raw_matrix
            .iter()
            .map(|r| (r[col] - mean).powi(2))
            .sum::<f64>()
            / n;
        let std_dev = variance.sqrt();

        if std_dev > 1e-10 {
            for row in &mut raw_matrix {
                row[col] = (row[col] - mean) / std_dev;
            }
        } else {
            // Constant feature — zero out
            for row in &mut raw_matrix {
                row[col] = 0.0;
            }
        }
    }

    (raw_matrix, feature_names)
}

/// Run K-means clustering on feature vectors.
///
/// Returns (assignments, centroids) where `assignments[i]` is the cluster
/// index for the i-th data point.
pub fn kmeans(
    data: &[Vec<f64>],
    k: usize,
    config: &ClusteringConfig,
) -> (Vec<usize>, Vec<Vec<f64>>) {
    let n = data.len();
    let d = data[0].len();
    let mut rng = StdRng::seed_from_u64(config.seed.wrapping_add(k as u64));

    // K-means++ initialization
    let mut centroids = kmeans_plus_plus_init(data, k, &mut rng);
    let mut assignments = vec![0usize; n];

    for _iter in 0..config.max_iterations {
        // Assignment step
        for (i, point) in data.iter().enumerate() {
            let mut best_cluster = 0;
            let mut best_dist = f64::INFINITY;
            for (c, centroid) in centroids.iter().enumerate() {
                let dist = euclidean_dist_sq(point, centroid);
                if dist < best_dist {
                    best_dist = dist;
                    best_cluster = c;
                }
            }
            assignments[i] = best_cluster;
        }

        // Update step
        let mut new_centroids = vec![vec![0.0; d]; k];
        let mut counts = vec![0usize; k];

        for (i, point) in data.iter().enumerate() {
            let c = assignments[i];
            counts[c] += 1;
            for (j, &val) in point.iter().enumerate() {
                new_centroids[c][j] += val;
            }
        }

        for c in 0..k {
            if counts[c] > 0 {
                for val in &mut new_centroids[c][..d] {
                    *val /= counts[c] as f64;
                }
            } else {
                // Empty cluster — reinitialize randomly
                let random_idx = rng.random_range(0..n);
                new_centroids[c] = data[random_idx].clone();
            }
        }

        // Check convergence
        let max_movement: f64 = centroids
            .iter()
            .zip(new_centroids.iter())
            .map(|(old, new)| euclidean_dist_sq(old, new))
            .fold(0.0, f64::max);

        centroids = new_centroids;

        if max_movement < config.convergence_threshold {
            break;
        }
    }

    (assignments, centroids)
}

/// Compute the silhouette score for a clustering.
///
/// Returns a value in [-1, 1] where higher is better.
/// For single-cluster (k=1), returns 0.0.
///
/// For datasets larger than `SILHOUETTE_SAMPLE_SIZE`, uses random sampling
/// to keep computation tractable (O(sample² × K) instead of O(N² × K)).
pub fn silhouette_score(data: &[Vec<f64>], assignments: &[usize], k: usize) -> f64 {
    silhouette_score_seeded(data, assignments, k, 42)
}

/// Maximum number of points to use for silhouette scoring.
/// Beyond this, we sample to avoid O(N²) blowup.
const SILHOUETTE_SAMPLE_SIZE: usize = 2000;

/// Silhouette score with configurable seed (for deterministic sampling).
fn silhouette_score_seeded(data: &[Vec<f64>], assignments: &[usize], k: usize, seed: u64) -> f64 {
    if k <= 1 || data.len() <= k {
        return 0.0;
    }

    let n = data.len();

    // For large datasets, sample to keep O(N²) manageable
    let (sample_data, sample_assignments): (Vec<&Vec<f64>>, Vec<usize>) =
        if n > SILHOUETTE_SAMPLE_SIZE {
            let mut rng = StdRng::seed_from_u64(seed);
            let mut indices: Vec<usize> = (0..n).collect();
            // Fisher-Yates shuffle, take first SILHOUETTE_SAMPLE_SIZE
            for i in 0..SILHOUETTE_SAMPLE_SIZE {
                let j = rng.random_range(i..n);
                indices.swap(i, j);
            }
            indices.truncate(SILHOUETTE_SAMPLE_SIZE);
            let data_refs: Vec<&Vec<f64>> = indices.iter().map(|&i| &data[i]).collect();
            let assign_refs: Vec<usize> = indices.iter().map(|&i| assignments[i]).collect();
            (data_refs, assign_refs)
        } else {
            let data_refs: Vec<&Vec<f64>> = data.iter().collect();
            (data_refs, assignments.to_vec())
        };

    let sample_n = sample_data.len();
    let mut total_score = 0.0;

    for i in 0..sample_n {
        let cluster_i = sample_assignments[i];

        // a(i) = mean intra-cluster distance
        let mut intra_sum = 0.0;
        let mut intra_count = 0;
        for j in 0..sample_n {
            if j != i && sample_assignments[j] == cluster_i {
                intra_sum += euclidean_dist_sq(sample_data[i], sample_data[j]).sqrt();
                intra_count += 1;
            }
        }
        let a_i = if intra_count > 0 {
            intra_sum / intra_count as f64
        } else {
            0.0
        };

        // b(i) = min mean inter-cluster distance
        let mut b_i = f64::INFINITY;
        for c in 0..k {
            if c == cluster_i {
                continue;
            }
            let mut inter_sum = 0.0;
            let mut inter_count = 0;
            for j in 0..sample_n {
                if sample_assignments[j] == c {
                    inter_sum += euclidean_dist_sq(sample_data[i], sample_data[j]).sqrt();
                    inter_count += 1;
                }
            }
            if inter_count > 0 {
                let mean_dist = inter_sum / inter_count as f64;
                if mean_dist < b_i {
                    b_i = mean_dist;
                }
            }
        }

        if b_i == f64::INFINITY {
            b_i = 0.0;
        }

        let s_i = if a_i.max(b_i) > 0.0 {
            (b_i - a_i) / a_i.max(b_i)
        } else {
            0.0
        };
        total_score += s_i;
    }

    total_score / sample_n as f64
}

/// Discover personas from actor profiles using K-means clustering.
///
/// Automatically selects the best K using silhouette score. Returns
/// `None` if there are too few actors for meaningful clustering.
pub fn discover_personas(
    profiles: &[ActorProfile],
    config: &ClusteringConfig,
) -> Option<ClusteringResult> {
    if profiles.len() < config.min_actors {
        return None;
    }

    let (features, _feature_names) = profiles_to_features(profiles);
    if features.is_empty() {
        return None;
    }

    // Determine K range: 2..min(√N, MAX_K)
    let max_k = ((profiles.len() as f64).sqrt().ceil() as usize)
        .min(MAX_K)
        .min(profiles.len() / MIN_CLUSTER_SIZE);
    if max_k < 2 {
        return None;
    }

    // Try each K and pick the one with best silhouette score
    let mut best_k = 2;
    let mut best_score = f64::NEG_INFINITY;
    let mut best_assignments = Vec::new();
    let mut best_centroids = Vec::new();

    for k in 2..=max_k {
        let (assignments, centroids) = kmeans(&features, k, config);

        // Check that all clusters have minimum size
        let mut cluster_sizes = vec![0usize; k];
        for &a in &assignments {
            cluster_sizes[a] += 1;
        }
        let has_tiny_cluster = cluster_sizes.iter().any(|&s| s < MIN_CLUSTER_SIZE);
        if has_tiny_cluster {
            continue;
        }

        let score = silhouette_score(&features, &assignments, k);
        if score > best_score {
            best_score = score;
            best_k = k;
            best_assignments = assignments;
            best_centroids = centroids;
        }
    }

    if best_assignments.is_empty() {
        return None;
    }

    // Build personas from centroids
    let personas = build_personas(&best_centroids, &best_assignments, profiles, best_k);

    // Build actor→persona assignments
    let assignments: Vec<(String, usize)> = profiles
        .iter()
        .zip(best_assignments.iter())
        .map(|(p, &c)| (p.actor_id.clone(), c))
        .collect();

    Some(ClusteringResult {
        personas,
        assignments,
        k: best_k,
        silhouette_score: best_score,
    })
}

/// Build persona specs from cluster assignments and original profiles.
fn build_personas(
    _centroids: &[Vec<f64>],
    assignments: &[usize],
    profiles: &[ActorProfile],
    k: usize,
) -> Vec<PersonaSpec> {
    let n = profiles.len() as f64;
    let mut personas = Vec::with_capacity(k);

    for cluster_idx in 0..k {
        let cluster_members: Vec<&ActorProfile> = profiles
            .iter()
            .zip(assignments.iter())
            .filter(|&(_, &a)| a == cluster_idx)
            .map(|(p, _)| p)
            .collect();

        let weight = cluster_members.len() as f64 / n;

        // Extract traits from cluster members (not Z-score centroids)
        let mut traits = BTreeMap::new();

        // Peak hours: average hourly distribution across cluster members
        let mut avg_hours = [0.0f64; 24];
        for member in &cluster_members {
            for (h, &v) in member.active_hours.iter().enumerate() {
                avg_hours[h] += v;
            }
        }
        let member_count = cluster_members.len() as f64;
        for h in &mut avg_hours {
            *h /= member_count;
        }
        let mut hour_scores: Vec<(usize, f64)> =
            avg_hours.iter().enumerate().map(|(h, &v)| (h, v)).collect();
        hour_scores.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let peak_hours: Vec<Value> = hour_scores
            .iter()
            .take(3)
            .map(|&(h, _)| Value::Int(h as i64))
            .collect();
        traits.insert("peak_hours".into(), Value::Array(peak_hours));

        // Active days pattern: average daily distribution across cluster members
        // Day indices: Mon=0..Sun=6 (per ActorAccumulator convention)
        let mut avg_days = [0.0f64; 7];
        for member in &cluster_members {
            for (d, &v) in member.active_days.iter().enumerate() {
                avg_days[d] += v;
            }
        }
        for d in &mut avg_days {
            *d /= member_count;
        }
        let weekday_sum: f64 = avg_days[0..5].iter().sum();
        let weekend_sum: f64 = avg_days[5..7].iter().sum();
        let pattern = if weekday_sum > weekend_sum * 2.0 {
            "weekday_heavy"
        } else if weekend_sum > weekday_sum {
            "weekend_heavy"
        } else {
            "uniform"
        };
        traits.insert("active_days_pattern".into(), Value::String(pattern.into()));

        // Activity rate (mean activity count of cluster members)
        let mean_activity: f64 = cluster_members
            .iter()
            .map(|p| p.activity_count as f64)
            .sum::<f64>()
            / cluster_members.len() as f64;
        traits.insert("activity_rate".into(), Value::Float(mean_activity));

        // Active span (mean days)
        let mean_span: f64 = cluster_members
            .iter()
            .map(|p| p.active_span_days)
            .sum::<f64>()
            / cluster_members.len() as f64;
        traits.insert("active_span_days".into(), Value::Float(mean_span));

        // Auto-generate name from dominant traits
        let name = generate_persona_name(cluster_idx, &avg_hours, pattern, mean_activity);

        personas.push(PersonaSpec {
            name,
            weight,
            traits,
        });
    }

    personas
}

/// Auto-generate a persona name from behavioral traits.
fn generate_persona_name(
    idx: usize,
    avg_hours: &[f64],
    days_pattern: &str,
    activity_rate: f64,
) -> String {
    // Determine activity level
    let activity_label = if activity_rate > 100.0 {
        "power"
    } else if activity_rate > 20.0 {
        "regular"
    } else {
        "casual"
    };

    // Determine time preference from peak hours (original space)
    let peak_hour = avg_hours
        .iter()
        .enumerate()
        .max_by(|a, b| a.1.partial_cmp(b.1).unwrap_or(std::cmp::Ordering::Equal))
        .map(|(h, _)| h)
        .unwrap_or(12);

    let time_label = if peak_hour < 6 {
        "night_owl"
    } else if peak_hour < 12 {
        "early_bird"
    } else if peak_hour < 18 {
        "afternoon"
    } else {
        "evening"
    };

    // Combine labels
    let day_suffix = match days_pattern {
        "weekend_heavy" => "_weekender",
        "weekday_heavy" => "",
        _ => "",
    };

    format!("{activity_label}_{time_label}{day_suffix}_{idx}")
}

// ── Math helpers ────────────────────────────────────────────────────────

fn euclidean_dist_sq(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b.iter()).map(|(x, y)| (x - y).powi(2)).sum()
}

/// K-means++ initialization: select initial centroids with probability
/// proportional to squared distance from nearest existing centroid.
fn kmeans_plus_plus_init(data: &[Vec<f64>], k: usize, rng: &mut StdRng) -> Vec<Vec<f64>> {
    let n = data.len();
    let mut centroids = Vec::with_capacity(k);

    // First centroid: random point
    let first_idx = rng.random_range(0..n);
    centroids.push(data[first_idx].clone());

    for _ in 1..k {
        // Compute distances to nearest centroid
        let mut distances: Vec<f64> = data
            .iter()
            .map(|point| {
                centroids
                    .iter()
                    .map(|c| euclidean_dist_sq(point, c))
                    .fold(f64::INFINITY, f64::min)
            })
            .collect();

        // Normalize to probabilities
        let total: f64 = distances.iter().sum();
        if total <= 0.0 {
            // All points are at centroids — pick random
            let idx = rng.random_range(0..n);
            centroids.push(data[idx].clone());
            continue;
        }
        for d in &mut distances {
            *d /= total;
        }

        // Weighted random selection
        let r: f64 = rng.random::<f64>();
        let mut cumulative = 0.0;
        let mut selected = n - 1;
        for (i, &prob) in distances.iter().enumerate() {
            cumulative += prob;
            if cumulative >= r {
                selected = i;
                break;
            }
        }
        centroids.push(data[selected].clone());
    }

    centroids
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::learn::behavioral::ActorProfile;

    fn make_profile(
        id: &str,
        peak_hour: usize,
        peak_day: usize,
        activity_count: u64,
        span_days: f64,
    ) -> ActorProfile {
        let mut active_hours = [0.0; 24];
        active_hours[peak_hour] = 0.6;
        active_hours[(peak_hour + 1) % 24] = 0.3;
        active_hours[(peak_hour + 2) % 24] = 0.1;

        let mut active_days = [0.0; 7];
        active_days[peak_day] = 0.5;
        active_days[(peak_day + 1) % 7] = 0.3;
        active_days[(peak_day + 2) % 7] = 0.2;

        ActorProfile {
            actor_id: id.into(),
            activity_count,
            active_span_days: span_days,
            active_hours,
            active_days,
            field_preferences: BTreeMap::new(),
        }
    }

    // ─── profiles_to_features ───────────────────────────────────────────

    #[test]
    fn profiles_to_features_correct_dimensions() {
        let profiles = vec![
            make_profile("a", 9, 0, 50, 30.0),
            make_profile("b", 14, 3, 100, 60.0),
        ];
        let (features, names) = profiles_to_features(&profiles);
        assert_eq!(features.len(), 2);
        assert_eq!(features[0].len(), 33); // 24 + 7 + 2
        assert_eq!(names.len(), 33);
    }

    #[test]
    fn profiles_to_features_z_normalized() {
        let profiles = vec![
            make_profile("a", 9, 0, 50, 30.0),
            make_profile("b", 9, 0, 50, 30.0),
        ];
        let (features, _) = profiles_to_features(&profiles);
        // Identical profiles should normalize to zero vectors
        for val in &features[0] {
            assert!(val.abs() < 1e-10);
        }
    }

    #[test]
    fn profiles_to_features_empty() {
        let (features, names) = profiles_to_features(&[]);
        assert!(features.is_empty());
        assert!(names.is_empty());
    }

    #[test]
    fn profiles_to_features_single_profile() {
        let profiles = vec![make_profile("a", 12, 3, 100, 45.0)];
        let (features, names) = profiles_to_features(&profiles);
        assert_eq!(features.len(), 1);
        assert_eq!(names.len(), 33);
        // Single profile: all features should be zero (no variance possible)
        for val in &features[0] {
            assert!(val.abs() < 1e-10, "single profile should be all zeros");
        }
    }

    #[test]
    fn profiles_to_features_names_correct() {
        let profiles = vec![make_profile("a", 0, 0, 1, 1.0)];
        let (_, names) = profiles_to_features(&profiles);
        assert_eq!(names[0], "hour_00");
        assert_eq!(names[23], "hour_23");
        assert_eq!(names[24], "day_0");
        assert_eq!(names[30], "day_6");
        assert_eq!(names[31], "log_activity_count");
        assert_eq!(names[32], "active_span_days");
    }

    // ─── kmeans ─────────────────────────────────────────────────────────

    #[test]
    fn kmeans_basic_two_clusters() {
        let data = vec![
            vec![0.0, 0.0],
            vec![0.1, 0.1],
            vec![0.2, -0.1],
            vec![10.0, 10.0],
            vec![10.1, 9.9],
            vec![9.9, 10.1],
        ];
        let config = ClusteringConfig::default();
        let (assignments, _centroids) = kmeans(&data, 2, &config);

        assert_eq!(assignments[0], assignments[1]);
        assert_eq!(assignments[1], assignments[2]);
        assert_eq!(assignments[3], assignments[4]);
        assert_eq!(assignments[4], assignments[5]);
        assert_ne!(assignments[0], assignments[3]);
    }

    #[test]
    fn kmeans_three_clusters() {
        let data = vec![
            vec![0.0, 0.0],
            vec![0.1, 0.0],
            vec![10.0, 0.0],
            vec![10.1, 0.0],
            vec![5.0, 10.0],
            vec![5.1, 10.0],
        ];
        let config = ClusteringConfig::default();
        let (assignments, centroids) = kmeans(&data, 3, &config);

        assert_eq!(centroids.len(), 3);
        // Each pair should be in the same cluster
        assert_eq!(assignments[0], assignments[1]);
        assert_eq!(assignments[2], assignments[3]);
        assert_eq!(assignments[4], assignments[5]);
        // All three clusters should be different
        assert_ne!(assignments[0], assignments[2]);
        assert_ne!(assignments[0], assignments[4]);
        assert_ne!(assignments[2], assignments[4]);
    }

    #[test]
    fn kmeans_identical_points() {
        // All points the same — should converge without panic
        let data = vec![vec![5.0, 5.0]; 10];
        let config = ClusteringConfig::default();
        let (assignments, centroids) = kmeans(&data, 2, &config);
        assert_eq!(assignments.len(), 10);
        assert_eq!(centroids.len(), 2);
    }

    #[test]
    fn kmeans_k_equals_n() {
        // Each point gets its own cluster
        let data = vec![vec![0.0], vec![10.0], vec![20.0]];
        let config = ClusteringConfig::default();
        let (assignments, centroids) = kmeans(&data, 3, &config);
        assert_eq!(centroids.len(), 3);
        // All assignments should be distinct
        let mut unique: Vec<usize> = assignments.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(unique.len(), 3);
    }

    #[test]
    fn kmeans_converges_within_max_iterations() {
        let data = vec![
            vec![0.0, 0.0],
            vec![0.1, 0.1],
            vec![100.0, 100.0],
            vec![100.1, 100.1],
        ];
        let config = ClusteringConfig {
            max_iterations: 5, // Very few iterations
            ..Default::default()
        };
        let (assignments, _) = kmeans(&data, 2, &config);
        // Well-separated clusters should converge quickly
        assert_eq!(assignments[0], assignments[1]);
        assert_eq!(assignments[2], assignments[3]);
        assert_ne!(assignments[0], assignments[2]);
    }

    #[test]
    fn kmeans_plus_plus_deterministic() {
        let data = vec![
            vec![0.0, 0.0],
            vec![1.0, 1.0],
            vec![5.0, 5.0],
            vec![6.0, 6.0],
        ];
        let config = ClusteringConfig::default();
        let (a1, _) = kmeans(&data, 2, &config);
        let (a2, _) = kmeans(&data, 2, &config);
        assert_eq!(a1, a2, "same seed should give same result");
    }

    #[test]
    fn kmeans_different_seeds_may_differ() {
        let data = vec![
            vec![0.0],
            vec![1.0],
            vec![2.0],
            vec![3.0],
            vec![4.0],
            vec![5.0],
        ];
        let c1 = ClusteringConfig {
            seed: 1,
            ..Default::default()
        };
        let c2 = ClusteringConfig {
            seed: 999,
            ..Default::default()
        };
        let (a1, _) = kmeans(&data, 3, &c1);
        let (a2, _) = kmeans(&data, 3, &c2);
        // Different seeds may produce different assignments (or same — we
        // just verify both run without error and produce valid output)
        assert_eq!(a1.len(), 6);
        assert_eq!(a2.len(), 6);
    }

    // ─── silhouette_score ───────────────────────────────────────────────

    #[test]
    fn silhouette_score_well_separated() {
        let data = vec![
            vec![0.0, 0.0],
            vec![0.1, 0.1],
            vec![10.0, 10.0],
            vec![10.1, 10.1],
        ];
        let assignments = vec![0, 0, 1, 1];
        let score = silhouette_score(&data, &assignments, 2);
        assert!(score > 0.9, "score was {score}");
    }

    #[test]
    fn silhouette_score_single_cluster() {
        let data = vec![vec![1.0], vec![2.0]];
        let assignments = vec![0, 0];
        let score = silhouette_score(&data, &assignments, 1);
        assert_eq!(score, 0.0);
    }

    #[test]
    fn silhouette_score_poorly_separated() {
        // Interleaved points → low silhouette
        let data = vec![
            vec![0.0],
            vec![1.0],
            vec![2.0],
            vec![3.0],
            vec![4.0],
            vec![5.0],
        ];
        // Interleaved assignment: 0,1,0,1,0,1
        let assignments = vec![0, 1, 0, 1, 0, 1];
        let score = silhouette_score(&data, &assignments, 2);
        assert!(
            score < 0.5,
            "interleaved clusters should have low score, got {score}"
        );
    }

    #[test]
    fn silhouette_score_data_smaller_than_k() {
        let data = vec![vec![1.0], vec![2.0]];
        let assignments = vec![0, 1];
        // k=3 but only 2 points → returns 0
        let score = silhouette_score(&data, &assignments, 3);
        assert_eq!(score, 0.0);
    }

    #[test]
    fn silhouette_score_three_clusters() {
        let data = vec![
            vec![0.0, 0.0],
            vec![0.1, 0.0],
            vec![10.0, 0.0],
            vec![10.1, 0.0],
            vec![5.0, 10.0],
            vec![5.1, 10.0],
        ];
        let assignments = vec![0, 0, 1, 1, 2, 2];
        let score = silhouette_score(&data, &assignments, 3);
        assert!(
            score > 0.7,
            "well-separated 3-cluster should have high score, got {score}"
        );
    }

    // ─── euclidean_dist_sq ──────────────────────────────────────────────

    #[test]
    fn euclidean_dist_sq_basic() {
        assert!((euclidean_dist_sq(&[0.0, 0.0], &[3.0, 4.0]) - 25.0).abs() < 1e-10);
    }

    #[test]
    fn euclidean_dist_sq_same_point() {
        assert_eq!(euclidean_dist_sq(&[1.0, 2.0, 3.0], &[1.0, 2.0, 3.0]), 0.0);
    }

    #[test]
    fn euclidean_dist_sq_one_dimensional() {
        assert!((euclidean_dist_sq(&[0.0], &[5.0]) - 25.0).abs() < 1e-10);
    }

    // ─── discover_personas ──────────────────────────────────────────────

    #[test]
    fn discover_personas_too_few_actors() {
        let profiles = vec![
            make_profile("a", 9, 0, 50, 30.0),
            make_profile("b", 14, 3, 100, 60.0),
        ];
        let config = ClusteringConfig {
            min_actors: 4,
            ..Default::default()
        };
        let result = discover_personas(&profiles, &config);
        assert!(result.is_none());
    }

    #[test]
    fn discover_personas_finds_clusters() {
        let mut profiles = Vec::new();
        for i in 0..10 {
            profiles.push(make_profile(
                &format!("morning_{i}"),
                8 + (i % 3),
                i % 5,
                50 + i as u64 * 5,
                30.0,
            ));
        }
        for i in 0..10 {
            profiles.push(make_profile(
                &format!("evening_{i}"),
                20 + (i % 3),
                5 + (i % 2),
                20 + i as u64 * 2,
                15.0,
            ));
        }

        let config = ClusteringConfig {
            seed: 42,
            min_actors: 4,
            ..Default::default()
        };
        let result = discover_personas(&profiles, &config);
        assert!(result.is_some());

        let result = result.unwrap();
        assert!(result.k >= 2, "should find at least 2 clusters, got {}", result.k);
        assert!(result.silhouette_score > 0.0, "silhouette should be positive");
        assert_eq!(result.personas.len(), result.k);
        assert_eq!(result.assignments.len(), 20);

        let weight_sum: f64 = result.personas.iter().map(|p| p.weight).sum();
        assert!((weight_sum - 1.0).abs() < 1e-10);

        for persona in &result.personas {
            assert!(!persona.name.is_empty());
            assert!(persona.traits.contains_key("peak_hours"));
            assert!(persona.traits.contains_key("active_days_pattern"));
            assert!(persona.traits.contains_key("activity_rate"));
        }
    }

    #[test]
    fn discover_personas_exactly_min_actors() {
        // Exactly 4 actors (min_actors default) — should attempt clustering
        let profiles = vec![
            make_profile("a", 8, 0, 100, 30.0),
            make_profile("b", 8, 0, 100, 30.0),
            make_profile("c", 20, 5, 10, 5.0),
            make_profile("d", 20, 5, 10, 5.0),
        ];
        let config = ClusteringConfig::default();
        let result = discover_personas(&profiles, &config);
        // May or may not find valid clusters depending on min cluster size,
        // but should not panic
        if let Some(r) = result {
            assert!(r.k >= 2);
            assert_eq!(r.assignments.len(), 4);
        }
    }

    #[test]
    fn discover_personas_all_identical() {
        // All profiles identical — no distinct clusters can form
        let profiles: Vec<ActorProfile> = (0..10)
            .map(|i| make_profile(&format!("u{i}"), 12, 3, 50, 30.0))
            .collect();
        let config = ClusteringConfig::default();
        let result = discover_personas(&profiles, &config);
        // Identical profiles produce zero-variance features, so silhouette
        // scores are 0.0 and no k beats the threshold. Expect None.
        assert!(
            result.is_none(),
            "expected None for identical profiles, got k={}",
            result.as_ref().map_or(0, |r| r.k)
        );
    }

    #[test]
    fn discover_personas_deterministic() {
        let mut profiles = Vec::new();
        for i in 0..8 {
            profiles.push(make_profile(
                &format!("a{i}"),
                8 + (i % 2),
                i % 5,
                50,
                30.0,
            ));
        }
        for i in 0..8 {
            profiles.push(make_profile(
                &format!("b{i}"),
                20 + (i % 2),
                5 + (i % 2),
                20,
                15.0,
            ));
        }

        let config = ClusteringConfig {
            seed: 123,
            ..Default::default()
        };
        let r1 = discover_personas(&profiles, &config);
        let r2 = discover_personas(&profiles, &config);

        match (r1, r2) {
            (Some(a), Some(b)) => {
                assert_eq!(a.k, b.k, "same seed should produce same K");
                assert_eq!(a.assignments, b.assignments, "same seed should produce same assignments");
            }
            (None, None) => {} // both None is fine
            _ => panic!("determinism failure: one returned Some, other None"),
        }
    }

    #[test]
    fn clustering_result_has_correct_assignment_count() {
        let profiles: Vec<ActorProfile> = (0..20)
            .map(|i| make_profile(&format!("u{i}"), i % 24, i % 7, 10 + i as u64, 10.0))
            .collect();
        let config = ClusteringConfig::default();
        let result =
            discover_personas(&profiles, &config).expect("20 diverse profiles should cluster");
        assert_eq!(result.assignments.len(), 20);
        for (_, cluster) in &result.assignments {
            assert!(*cluster < result.k);
        }
    }

    // ─── generate_persona_name ──────────────────────────────────────────

    #[test]
    fn generate_persona_name_variants() {
        let mut avg_hours = [0.0f64; 24];
        avg_hours[8] = 0.6;
        let name = generate_persona_name(0, &avg_hours, "weekday_heavy", 150.0);
        assert!(name.contains("power"), "expected 'power' in {name}");
        assert!(name.contains("early_bird"), "expected 'early_bird' in {name}");

        avg_hours[8] = 0.0;
        avg_hours[22] = 0.6;
        let name = generate_persona_name(1, &avg_hours, "weekend_heavy", 5.0);
        assert!(name.contains("casual"), "expected 'casual' in {name}");
        assert!(name.contains("evening"), "expected 'evening' in {name}");
        assert!(name.contains("weekender"), "expected 'weekender' in {name}");
    }

    #[test]
    fn generate_persona_name_time_labels() {
        let make = |hour: usize| -> String {
            let mut h = [0.0f64; 24];
            h[hour] = 1.0;
            generate_persona_name(0, &h, "uniform", 50.0)
        };

        assert!(make(3).contains("night_owl"), "hour 3 → night_owl");
        assert!(make(9).contains("early_bird"), "hour 9 → early_bird");
        assert!(make(14).contains("afternoon"), "hour 14 → afternoon");
        assert!(make(20).contains("evening"), "hour 20 → evening");
    }

    #[test]
    fn generate_persona_name_activity_levels() {
        let hours = [0.0f64; 24];
        let name_power = generate_persona_name(0, &hours, "uniform", 200.0);
        assert!(name_power.contains("power"));

        let name_regular = generate_persona_name(0, &hours, "uniform", 50.0);
        assert!(name_regular.contains("regular"));

        let name_casual = generate_persona_name(0, &hours, "uniform", 5.0);
        assert!(name_casual.contains("casual"));
    }

    // ─── build_personas ─────────────────────────────────────────────────

    #[test]
    fn build_personas_weights_sum_to_one() {
        let profiles = vec![
            make_profile("a", 8, 0, 100, 30.0),
            make_profile("b", 8, 0, 100, 30.0),
            make_profile("c", 8, 0, 100, 30.0),
            make_profile("d", 20, 5, 10, 5.0),
            make_profile("e", 20, 5, 10, 5.0),
        ];
        let assignments = vec![0, 0, 0, 1, 1];
        let centroids = vec![vec![0.0; 33], vec![0.0; 33]];
        let personas = build_personas(&centroids, &assignments, &profiles, 2);

        assert_eq!(personas.len(), 2);
        let weight_sum: f64 = personas.iter().map(|p| p.weight).sum();
        assert!(
            (weight_sum - 1.0).abs() < 1e-10,
            "weights should sum to 1.0, got {weight_sum}"
        );
        // Cluster 0 has 3/5 = 0.6, cluster 1 has 2/5 = 0.4
        assert!((personas[0].weight - 0.6).abs() < 1e-10);
        assert!((personas[1].weight - 0.4).abs() < 1e-10);
    }

    #[test]
    fn build_personas_includes_all_traits() {
        let profiles = vec![
            make_profile("a", 8, 0, 100, 30.0),
            make_profile("b", 20, 5, 10, 5.0),
        ];
        let assignments = vec![0, 1];
        let centroids = vec![vec![0.0; 33], vec![0.0; 33]];
        let personas = build_personas(&centroids, &assignments, &profiles, 2);

        assert_eq!(personas.len(), 2, "expected 2 personas for 2 clusters");
        for persona in &personas {
            assert!(persona.traits.contains_key("peak_hours"));
            assert!(persona.traits.contains_key("active_days_pattern"));
            assert!(persona.traits.contains_key("activity_rate"));
            assert!(persona.traits.contains_key("active_span_days"));
            assert!(!persona.name.is_empty());
        }
    }

    #[test]
    fn build_personas_day_pattern_detection() {
        // Weekday-heavy profile: activity on Mon-Fri
        let mut weekday = make_profile("wd", 12, 0, 50, 30.0);
        weekday.active_days = [0.2, 0.2, 0.2, 0.2, 0.2, 0.0, 0.0];
        // Weekend-heavy profile: activity on Sat-Sun
        let mut weekend = make_profile("we", 12, 5, 50, 30.0);
        weekend.active_days = [0.0, 0.0, 0.0, 0.0, 0.0, 0.5, 0.5];

        let profiles = vec![weekday, weekend];
        let assignments = vec![0, 1];
        let centroids = vec![vec![0.0; 33], vec![0.0; 33]];
        let personas = build_personas(&centroids, &assignments, &profiles, 2);

        // One should be weekday_heavy, other weekend_heavy or uniform
        let patterns: Vec<&Value> = personas
            .iter()
            .map(|p| &p.traits["active_days_pattern"])
            .collect();
        let has_weekday = patterns.iter().any(|v| matches!(v, Value::String(s) if s == "weekday_heavy"));
        let has_weekend = patterns.iter().any(|v| matches!(v, Value::String(s) if s == "weekend_heavy"));
        assert!(has_weekday, "should detect weekday_heavy pattern");
        assert!(has_weekend, "should detect weekend_heavy pattern");
    }

    // ─── ClusteringConfig ───────────────────────────────────────────────

    #[test]
    fn clustering_config_default() {
        let config = ClusteringConfig::default();
        assert_eq!(config.seed, 42);
        assert_eq!(config.max_iterations, 100);
        assert!(config.convergence_threshold > 0.0);
        assert_eq!(config.min_actors, 4);
    }
}
