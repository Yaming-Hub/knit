//! Persistent state for incremental learning.
//!
//! The [`LearnState`] type captures sufficient statistics accumulated across
//! multiple data chunks. It can be serialized to disk and loaded back to
//! continue learning from new data without re-reading previous chunks.
//!
//! ## State Hierarchy
//!
//! ```text
//! LearnState
//! ├── tables: BTreeMap<String, TableState>
//! │   └── columns: Vec<ColumnState>
//! │       ├── numeric: Option<NumericState>
//! │       ├── reservoir: ReservoirSample
//! │       └── top_k: TopKTracker
//! └── chunks: Vec<ChunkRecord>
//! ```
//!
//! ## File Format
//!
//! The state file uses JSON with a versioned envelope for debuggability.
//! Future versions may switch to a binary format for large states.

use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::learn::streaming::{HyperLogLog, NumericState, ReservoirSample, TopKTracker};

/// Current format version for state files.
const FORMAT_VERSION: u16 = 1;
/// Current algorithm version (tracks parameter changes).
const ALGORITHM_VERSION: u16 = 1;

/// Default reservoir sample capacity per column.
pub const DEFAULT_RESERVOIR_CAPACITY: usize = 10_000;
/// Default top-K tracker capacity per column.
pub const DEFAULT_TOPK_CAPACITY: usize = 1_000;
/// Default HyperLogLog precision (p=14 → 16K registers, ~0.8% error).
pub const DEFAULT_HLL_PRECISION: u8 = 14;

/// Persistent state for incremental learning.
///
/// Accumulates sufficient statistics across multiple data chunks. The state
/// is self-contained: a schema can always be derived from it via finalization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LearnState {
    /// Format version for forward-compatibility checks.
    pub format_version: u16,
    /// Algorithm version for parameter-change detection.
    pub algorithm_version: u16,
    /// Deterministic seed for reproducible sampling.
    pub seed: u64,
    /// Per-table states, keyed by entity name.
    pub tables: BTreeMap<String, TableState>,
    /// Records of processed chunks (for diagnostics and duplicate detection).
    pub chunks: Vec<ChunkRecord>,
    /// Total rows processed across all chunks and tables.
    pub total_rows: u64,
    /// Relationship evidence (FK candidates with HLL sketches).
    #[serde(default)]
    pub relationship_evidence: Vec<super::relationships::RelationshipEvidence>,
    /// Pairwise numeric correlations (running Pearson).
    #[serde(default)]
    pub correlations: Vec<super::relationships::PairwiseCorrelation>,
}

impl LearnState {
    /// Create a new empty state with the given seed.
    pub fn new(seed: u64) -> Self {
        Self {
            format_version: FORMAT_VERSION,
            algorithm_version: ALGORITHM_VERSION,
            seed,
            tables: BTreeMap::new(),
            chunks: Vec::new(),
            total_rows: 0,
            relationship_evidence: Vec::new(),
            correlations: Vec::new(),
        }
    }

    /// Load state from a file path. Returns `None` if the file does not exist.
    pub fn load(path: &Path) -> Result<Option<Self>, StateError> {
        if !path.exists() {
            return Ok(None);
        }
        let mut file = std::fs::File::open(path)
            .map_err(|e| StateError::Io(format!("failed to open state file: {e}")))?;
        let mut contents = String::new();
        file.read_to_string(&mut contents)
            .map_err(|e| StateError::Io(format!("failed to read state file: {e}")))?;

        let state: Self = serde_json::from_str(&contents)
            .map_err(|e| StateError::Deserialize(format!("invalid state file: {e}")))?;

        // Version compatibility check
        if state.format_version > FORMAT_VERSION {
            return Err(StateError::VersionMismatch {
                file_version: state.format_version,
                supported_version: FORMAT_VERSION,
            });
        }

        // Warn if algorithm parameters may have changed
        if state.algorithm_version != ALGORITHM_VERSION {
            eprintln!(
                "Warning: state file uses algorithm version {}, current is {}. \
                 Merge results may be inconsistent.",
                state.algorithm_version, ALGORITHM_VERSION
            );
        }

        Ok(Some(state))
    }

    /// Save state to a file path (atomic write via temp file + rename).
    pub fn save(&self, path: &Path) -> Result<(), StateError> {
        let json = serde_json::to_string_pretty(self)
            .map_err(|e| StateError::Serialize(format!("failed to serialize state: {e}")))?;

        // Atomic write: write to temp file, fsync, then rename
        let tmp_path = path.with_extension("state.tmp");
        let mut file = std::fs::File::create(&tmp_path)
            .map_err(|e| StateError::Io(format!("failed to create temp file: {e}")))?;

        let write_result = (|| -> Result<(), StateError> {
            file.write_all(json.as_bytes())
                .map_err(|e| StateError::Io(format!("failed to write state: {e}")))?;
            file.sync_all()
                .map_err(|e| StateError::Io(format!("failed to sync state: {e}")))?;
            Ok(())
        })();

        if let Err(e) = write_result {
            drop(file);
            let _ = std::fs::remove_file(&tmp_path);
            return Err(e);
        }
        drop(file);

        std::fs::rename(&tmp_path, path).map_err(|e| {
            let _ = std::fs::remove_file(&tmp_path);
            StateError::Io(format!("failed to rename state file: {e}"))
        })?;

        Ok(())
    }

    /// Record that a chunk has been processed.
    ///
    /// Returns `true` if the source path was already in the chunk history
    /// (potential duplicate).
    pub fn record_chunk(&mut self, source: &str, row_count: u64) -> bool {
        let is_duplicate = self.chunks.iter().any(|c| c.source == source);
        self.chunks.push(ChunkRecord {
            source: source.to_string(),
            row_count,
            processed_at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        });
        self.total_rows = self.total_rows.saturating_add(row_count);
        is_duplicate
    }

    /// Get or create a table state for the given entity name.
    pub fn get_or_create_table(&mut self, name: &str) -> &mut TableState {
        if !self.tables.contains_key(name) {
            self.tables.insert(
                name.to_string(),
                TableState::new(name.to_string(), self.seed),
            );
        }
        self.tables.get_mut(name).unwrap()
    }

    /// Number of tables in the state.
    pub fn table_count(&self) -> usize {
        self.tables.len()
    }

    /// Total chunks processed.
    pub fn chunks_processed(&self) -> usize {
        self.chunks.len()
    }
}

/// State for a single table/entity.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableState {
    /// Entity name.
    pub name: String,
    /// Total rows observed for this table.
    pub row_count: u64,
    /// Per-column states (ordered by first observation).
    pub columns: Vec<ColumnState>,
    /// Base seed for column-level RNG derivation.
    seed: u64,
}

impl TableState {
    /// Create a new empty table state.
    pub fn new(name: String, seed: u64) -> Self {
        Self {
            name,
            row_count: 0,
            columns: Vec::new(),
            seed,
        }
    }

    /// Get or create a column state for the given column name.
    pub fn get_or_create_column(
        &mut self,
        name: &str,
        data_type: ColumnDataType,
    ) -> &mut ColumnState {
        let pos = self.columns.iter().position(|c| c.name == name);
        match pos {
            Some(idx) => {
                // Widen type if needed
                self.columns[idx].widen_type(data_type);
                &mut self.columns[idx]
            }
            None => {
                let col_seed = self
                    .seed
                    .wrapping_add((self.columns.len() as u64).wrapping_mul(0x9e3779b97f4a7c15));
                self.columns
                    .push(ColumnState::new(name.to_string(), data_type, col_seed));
                self.columns.last_mut().unwrap()
            }
        }
    }

    /// Add to the row count.
    pub fn add_rows(&mut self, count: u64) {
        self.row_count = self.row_count.saturating_add(count);
    }
}

/// Simplified data type classification for state tracking.
///
/// This is a coarser classification than Arrow's DataType, sufficient for
/// determining which statistics to maintain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ColumnDataType {
    /// Integer (i8–i64, u8–u64).
    Integer,
    /// Floating point (f32, f64).
    Float,
    /// String/text (Utf8, LargeUtf8).
    String,
    /// Timestamp/date.
    Temporal,
    /// Boolean.
    Boolean,
    /// Other/unknown.
    Other,
}

impl ColumnDataType {
    /// Widen this type to accommodate a new observation.
    ///
    /// Integer + Float → Float; anything + String → String.
    pub fn widen(self, other: ColumnDataType) -> ColumnDataType {
        if self == other {
            return self;
        }
        match (self, other) {
            (ColumnDataType::Integer, ColumnDataType::Float)
            | (ColumnDataType::Float, ColumnDataType::Integer) => ColumnDataType::Float,
            _ => ColumnDataType::String, // fallback for incompatible types
        }
    }
}

/// State for a single column.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColumnState {
    /// Column name.
    pub name: String,
    /// Observed data type (may widen over time).
    pub data_type: ColumnDataType,
    /// Original Arrow type string (e.g., "Int64", "Timestamp(Nanosecond, None)").
    /// Preserved from the first observation for finalize fidelity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub arrow_type_hint: Option<String>,
    /// Maximum decimal places observed (for numeric columns).
    #[serde(default)]
    pub max_decimal_places: u8,
    /// Whether all observed numeric values are integers (no fractional part).
    #[serde(default = "default_true")]
    pub all_integer: bool,
    /// Numeric statistics (for integer/float columns).
    pub numeric: Option<NumericState>,
    /// HyperLogLog for cardinality estimation.
    pub hll: HyperLogLog,
    /// Reservoir sample for distribution fitting at finalize time.
    pub reservoir: ReservoirSample,
    /// Top-K frequency tracker.
    pub top_k: TopKTracker,
    /// Total non-null values observed.
    pub count: u64,
    /// Total null values observed.
    pub null_count: u64,
    /// Total empty-string values observed (CSV readers parse empty cells as "").
    #[serde(default)]
    pub empty_string_count: u64,
    /// Number of chunks that contained this column.
    pub chunks_present: u64,
}

fn default_true() -> bool {
    true
}

impl ColumnState {
    /// Create a new empty column state.
    pub fn new(name: String, data_type: ColumnDataType, seed: u64) -> Self {
        let numeric = match data_type {
            ColumnDataType::Integer | ColumnDataType::Float | ColumnDataType::Temporal => {
                Some(NumericState::new())
            }
            _ => None,
        };

        Self {
            name,
            data_type,
            arrow_type_hint: None,
            max_decimal_places: 0,
            all_integer: true,
            numeric,
            hll: HyperLogLog::new(DEFAULT_HLL_PRECISION),
            reservoir: ReservoirSample::new(DEFAULT_RESERVOIR_CAPACITY, seed),
            top_k: TopKTracker::new(DEFAULT_TOPK_CAPACITY),
            count: 0,
            null_count: 0,
            empty_string_count: 0,
            chunks_present: 0,
        }
    }

    /// Set the Arrow type hint (preserved from first observation).
    pub fn set_arrow_type_hint(&mut self, hint: &str) {
        if self.arrow_type_hint.is_none() {
            self.arrow_type_hint = Some(hint.to_string());
        }
    }

    /// Record that this column was present in a chunk.
    pub fn mark_chunk_present(&mut self) {
        self.chunks_present += 1;
    }

    /// Update with a non-null string value.
    pub fn update_string(&mut self, value: &str) {
        if value.is_empty() {
            self.empty_string_count += 1;
            return;
        }
        self.count += 1;
        self.hll.add(value);
        self.reservoir.add(value.to_string());
        self.top_k.add(value);
    }

    /// Update with a non-null numeric value and its string representation.
    pub fn update_numeric(&mut self, value: f64, str_repr: &str) {
        self.count += 1;
        self.hll.add(str_repr);
        self.reservoir.add(str_repr.to_string());
        self.top_k.add(str_repr);
        if let Some(ref mut numeric) = self.numeric {
            numeric.update(value);
        }
        // Track integer-ness and decimal precision
        if value.fract() != 0.0 {
            self.all_integer = false;
            // Count decimal places from string representation
            if let Some(dot_pos) = str_repr.find('.') {
                let decimals = str_repr[dot_pos + 1..].trim_end_matches('0').len().min(255) as u8;
                self.max_decimal_places = self.max_decimal_places.max(decimals);
            }
        }
    }

    /// Record a null observation.
    pub fn update_null(&mut self) {
        self.null_count += 1;
        if let Some(ref mut numeric) = self.numeric {
            numeric.update_null();
        }
    }

    /// Widen the data type to accommodate a new observation.
    pub fn widen_type(&mut self, new_type: ColumnDataType) {
        let widened = self.data_type.widen(new_type);
        if widened != self.data_type {
            self.data_type = widened;
            if matches!(widened, ColumnDataType::String | ColumnDataType::Other) {
                // No longer numeric — drop stale numeric state
                self.numeric = None;
            } else if widened == ColumnDataType::Float && self.numeric.is_none() {
                // Widened to float from integer — ensure numeric state exists
                self.numeric = Some(NumericState::new());
            }
        }
    }

    /// Estimated cardinality (distinct count).
    pub fn estimated_cardinality(&self) -> f64 {
        self.hll.cardinality()
    }

    /// Null rate (0.0–1.0).
    pub fn null_rate(&self) -> f64 {
        let total = self.count + self.null_count + self.empty_string_count;
        if total == 0 {
            0.0
        } else {
            self.null_count as f64 / total as f64
        }
    }

    /// Empty-string rate (0.0–1.0).
    pub fn empty_string_rate(&self) -> f64 {
        let total = self.count + self.null_count + self.empty_string_count;
        if total == 0 {
            0.0
        } else {
            self.empty_string_count as f64 / total as f64
        }
    }

    /// Total observations (null + non-null + empty-string).
    pub fn total_observations(&self) -> u64 {
        self.count
            .saturating_add(self.null_count)
            .saturating_add(self.empty_string_count)
    }

    /// Merge another column state into this one.
    pub fn merge(&mut self, other: &ColumnState) {
        self.widen_type(other.data_type);
        self.count = self.count.saturating_add(other.count);
        self.null_count = self.null_count.saturating_add(other.null_count);
        self.empty_string_count = self
            .empty_string_count
            .saturating_add(other.empty_string_count);
        self.chunks_present = self.chunks_present.saturating_add(other.chunks_present);

        // Merge sub-structures (only if precisions match to avoid panics)
        if self.hll.precision() == other.hll.precision() {
            self.hll.merge(&other.hll);
        }
        self.reservoir.merge(&other.reservoir);
        self.top_k.merge(&other.top_k);

        // Merge numeric only if both are numeric types
        if (self.numeric.is_some() || other.numeric.is_some())
            && matches!(
                self.data_type,
                ColumnDataType::Integer | ColumnDataType::Float | ColumnDataType::Temporal
            )
        {
            match (&mut self.numeric, &other.numeric) {
                (Some(ref mut s), Some(ref o)) => s.merge(o),
                (None, Some(o)) => self.numeric = Some(o.clone()),
                _ => {}
            }
        }
    }
}

/// Record of a processed chunk (for diagnostics and duplicate detection).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChunkRecord {
    /// Source file path.
    pub source: String,
    /// Number of rows in this chunk.
    pub row_count: u64,
    /// Unix timestamp when processed.
    pub processed_at: u64,
}

/// Errors that can occur during state file operations.
#[derive(Debug, thiserror::Error)]
pub enum StateError {
    /// I/O error reading or writing the state file.
    #[error("I/O error: {0}")]
    Io(String),
    /// State file could not be deserialized.
    #[error("deserialization error: {0}")]
    Deserialize(String),
    /// State file could not be serialized.
    #[error("serialization error: {0}")]
    Serialize(String),
    /// State file version is newer than supported.
    #[error("state file version {file_version} is newer than supported version {supported_version}; please upgrade knit")]
    VersionMismatch {
        /// Version found in the file.
        file_version: u16,
        /// Maximum version this build supports.
        supported_version: u16,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn new_state_is_empty() {
        let state = LearnState::new(42);
        assert_eq!(state.table_count(), 0);
        assert_eq!(state.chunks_processed(), 0);
        assert_eq!(state.total_rows, 0);
    }

    #[test]
    fn get_or_create_table() {
        let mut state = LearnState::new(42);
        state.get_or_create_table("users");
        assert_eq!(state.table_count(), 1);
        state.get_or_create_table("users");
        assert_eq!(state.table_count(), 1); // no duplicate
        state.get_or_create_table("orders");
        assert_eq!(state.table_count(), 2);
    }

    #[test]
    fn get_or_create_column() {
        let mut table = TableState::new("users".into(), 42);
        table.get_or_create_column("name", ColumnDataType::String);
        assert_eq!(table.columns.len(), 1);
        table.get_or_create_column("name", ColumnDataType::String);
        assert_eq!(table.columns.len(), 1); // no duplicate
        table.get_or_create_column("age", ColumnDataType::Integer);
        assert_eq!(table.columns.len(), 2);
    }

    #[test]
    fn column_type_widening() {
        let mut table = TableState::new("t".into(), 42);
        table.get_or_create_column("val", ColumnDataType::Integer);
        assert_eq!(table.columns[0].data_type, ColumnDataType::Integer);
        table.get_or_create_column("val", ColumnDataType::Float);
        assert_eq!(table.columns[0].data_type, ColumnDataType::Float);
    }

    #[test]
    fn column_state_updates() {
        let mut col = ColumnState::new("name".into(), ColumnDataType::String, 42);
        col.mark_chunk_present();
        col.update_string("Alice");
        col.update_string("Bob");
        col.update_null();
        assert_eq!(col.count, 2);
        assert_eq!(col.null_count, 1);
        assert_eq!(col.chunks_present, 1);
        assert!(col.estimated_cardinality() >= 1.5);
        assert!((col.null_rate() - 1.0 / 3.0).abs() < 0.01);
    }

    #[test]
    fn column_numeric_updates() {
        let mut col = ColumnState::new("age".into(), ColumnDataType::Integer, 42);
        col.update_numeric(25.0, "25");
        col.update_numeric(30.0, "30");
        col.update_numeric(35.0, "35");
        assert_eq!(col.count, 3);
        let num = col.numeric.as_ref().unwrap();
        assert!((num.mean() - 30.0).abs() < 1e-10);
        assert_eq!(num.min(), 25.0);
        assert_eq!(num.max(), 35.0);
    }

    #[test]
    fn column_merge() {
        let mut a = ColumnState::new("val".into(), ColumnDataType::Integer, 42);
        a.update_numeric(1.0, "1");
        a.update_numeric(2.0, "2");
        a.mark_chunk_present();

        let mut b = ColumnState::new("val".into(), ColumnDataType::Integer, 99);
        b.update_numeric(3.0, "3");
        b.update_numeric(4.0, "4");
        b.mark_chunk_present();

        a.merge(&b);
        assert_eq!(a.count, 4);
        assert_eq!(a.chunks_present, 2);
        let num = a.numeric.as_ref().unwrap();
        assert!((num.mean() - 2.5).abs() < 1e-10);
        assert_eq!(num.min(), 1.0);
        assert_eq!(num.max(), 4.0);
    }

    #[test]
    fn record_chunk_detects_duplicate() {
        let mut state = LearnState::new(42);
        let dup1 = state.record_chunk("data/file1.csv", 1000);
        assert!(!dup1);
        let dup2 = state.record_chunk("data/file2.csv", 2000);
        assert!(!dup2);
        let dup3 = state.record_chunk("data/file1.csv", 1000);
        assert!(dup3); // duplicate detected
        assert_eq!(state.total_rows, 4000);
        assert_eq!(state.chunks_processed(), 3);
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("test.state");

        let mut state = LearnState::new(42);
        let table = state.get_or_create_table("users");
        let col = table.get_or_create_column("name", ColumnDataType::String);
        col.update_string("Alice");
        col.update_string("Bob");
        table.add_rows(2);
        state.record_chunk("users.csv", 2);

        state.save(&path).unwrap();
        let loaded = LearnState::load(&path).unwrap().unwrap();

        assert_eq!(loaded.format_version, FORMAT_VERSION);
        assert_eq!(loaded.seed, 42);
        assert_eq!(loaded.table_count(), 1);
        assert_eq!(loaded.total_rows, 2);
        assert_eq!(loaded.chunks_processed(), 1);

        let table = &loaded.tables["users"];
        assert_eq!(table.row_count, 2);
        assert_eq!(table.columns.len(), 1);
        assert_eq!(table.columns[0].name, "name");
        assert_eq!(table.columns[0].count, 2);
    }

    #[test]
    fn load_nonexistent_returns_none() {
        let result = LearnState::load(Path::new("/nonexistent/path.state")).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn version_mismatch_error() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("future.state");

        // Write a state with a future version
        let mut state = LearnState::new(42);
        state.format_version = 999;
        let json = serde_json::to_string(&state).unwrap();
        std::fs::write(&path, json).unwrap();

        let result = LearnState::load(&path);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.to_string().contains("newer than supported"));
    }

    #[test]
    fn data_type_widening_rules() {
        assert_eq!(
            ColumnDataType::Integer.widen(ColumnDataType::Integer),
            ColumnDataType::Integer
        );
        assert_eq!(
            ColumnDataType::Integer.widen(ColumnDataType::Float),
            ColumnDataType::Float
        );
        assert_eq!(
            ColumnDataType::Float.widen(ColumnDataType::Integer),
            ColumnDataType::Float
        );
        assert_eq!(
            ColumnDataType::Integer.widen(ColumnDataType::String),
            ColumnDataType::String
        );
        assert_eq!(
            ColumnDataType::Temporal.widen(ColumnDataType::Integer),
            ColumnDataType::String
        );
    }
}