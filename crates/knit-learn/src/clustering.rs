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

use knit_core::Value;
use rand::rngs::StdRng;
use rand::{Rng, SeedableRng};

use crate::behavioral::ActorProfile;

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
        let variance: f64 = raw_matrix.iter().map(|r| (r[col] - mean).powi(2)).sum::<f64>() / n;
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
/// Returns (assignments, centroids) where assignments[i] is the cluster
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
                for j in 0..d {
                    new_centroids[c][j] /= counts[c] as f64;
                }
            } else {
                // Empty cluster — reinitialize randomly
                let random_idx = rng.gen_range(0..n);
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
fn silhouette_score_seeded(
    data: &[Vec<f64>],
    assignments: &[usize],
    k: usize,
    seed: u64,
) -> f64 {
    if k <= 1 || data.len() <= k {
        return 0.0;
    }

    let n = data.len();

    // For large datasets, sample to keep O(N²) manageable
    let (sample_data, sample_assignments): (Vec<&Vec<f64>>, Vec<usize>) = if n > SILHOUETTE_SAMPLE_SIZE {
        let mut rng = StdRng::seed_from_u64(seed);
        let mut indices: Vec<usize> = (0..n).collect();
        // Fisher-Yates shuffle, take first SILHOUETTE_SAMPLE_SIZE
        for i in 0..SILHOUETTE_SAMPLE_SIZE {
            let j = rng.gen_range(i..n);
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
    let max_k = ((profiles.len() as f64).sqrt().ceil() as usize).min(MAX_K).min(profiles.len() / MIN_CLUSTER_SIZE);
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
            .filter(|(_, &a)| a == cluster_idx)
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
        let mut hour_scores: Vec<(usize, f64)> = avg_hours
            .iter()
            .enumerate()
            .map(|(h, &v)| (h, v))
            .collect();
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
        traits.insert(
            "activity_rate".into(),
            Value::Float(mean_activity),
        );

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
    a.iter()
        .zip(b.iter())
        .map(|(x, y)| (x - y).powi(2))
        .sum()
}

/// K-means++ initialization: select initial centroids with probability
/// proportional to squared distance from nearest existing centroid.
fn kmeans_plus_plus_init(data: &[Vec<f64>], k: usize, rng: &mut StdRng) -> Vec<Vec<f64>> {
    let n = data.len();
    let mut centroids = Vec::with_capacity(k);

    // First centroid: random point
    let first_idx = rng.gen_range(0..n);
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
            let idx = rng.gen_range(0..n);
            centroids.push(data[idx].clone());
            continue;
        }
        for d in &mut distances {
            *d /= total;
        }

        // Weighted random selection
        let r: f64 = rng.gen::<f64>();
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
    use crate::behavioral::ActorProfile;

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
    fn kmeans_basic_two_clusters() {
        // Two obvious clusters: [0,0] area and [10,10] area
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

        // Points 0-2 should be in one cluster, 3-5 in another
        assert_eq!(assignments[0], assignments[1]);
        assert_eq!(assignments[1], assignments[2]);
        assert_eq!(assignments[3], assignments[4]);
        assert_eq!(assignments[4], assignments[5]);
        assert_ne!(assignments[0], assignments[3]);
    }

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
        // Well-separated clusters should have high silhouette
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
        // Two distinct groups: morning workers vs evening workers
        let mut profiles = Vec::new();
        for i in 0..10 {
            profiles.push(make_profile(
                &format!("morning_{i}"),
                8 + (i % 3),   // peak hour 8-10
                i % 5,         // weekdays
                50 + i as u64 * 5,
                30.0,
            ));
        }
        for i in 0..10 {
            profiles.push(make_profile(
                &format!("evening_{i}"),
                20 + (i % 3),  // peak hour 20-22
                5 + (i % 2),   // weekends
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

        // Weights should sum to 1.0
        let weight_sum: f64 = result.personas.iter().map(|p| p.weight).sum();
        assert!((weight_sum - 1.0).abs() < 1e-10);

        // Each persona should have traits
        for persona in &result.personas {
            assert!(!persona.name.is_empty());
            assert!(persona.traits.contains_key("peak_hours"));
            assert!(persona.traits.contains_key("active_days_pattern"));
            assert!(persona.traits.contains_key("activity_rate"));
        }
    }

    #[test]
    fn generate_persona_name_variants() {
        let mut avg_hours = [0.0f64; 24];
        // Peak at hour 8
        avg_hours[8] = 0.6;
        let name = generate_persona_name(0, &avg_hours, "weekday_heavy", 150.0);
        assert!(name.contains("power"), "expected 'power' in {name}");
        assert!(name.contains("early_bird"), "expected 'early_bird' in {name}");

        // Peak at hour 22, low activity
        avg_hours[8] = 0.0;
        avg_hours[22] = 0.6;
        let name = generate_persona_name(1, &avg_hours, "weekend_heavy", 5.0);
        assert!(name.contains("casual"), "expected 'casual' in {name}");
        assert!(name.contains("evening"), "expected 'evening' in {name}");
        assert!(name.contains("weekender"), "expected 'weekender' in {name}");
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
    fn clustering_result_has_correct_assignment_count() {
        let profiles: Vec<ActorProfile> = (0..20)
            .map(|i| make_profile(&format!("u{i}"), i % 24, i % 7, 10 + i as u64, 10.0))
            .collect();
        let config = ClusteringConfig::default();
        if let Some(result) = discover_personas(&profiles, &config) {
            assert_eq!(result.assignments.len(), 20);
            for (_, cluster) in &result.assignments {
                assert!(*cluster < result.k);
            }
        }
    }
}
