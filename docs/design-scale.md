# Design: Multi-Dimensional Dataset Scaling (`knit scale`)

## 1. Motivation

`knit generate --count 10x` scales all entity row counts uniformly — every table
gets 10× more rows. But real datasets have **multiple independent scaling
dimensions**. A user who learned from a 10-week, 8-person, 3-location dataset may
want to generate a 52-week, 100-person, 20-location version. Uniform row
multiplication cannot express this.

`knit scale` introduces dimension-aware scaling: the tool analyzes a learned
blueprint, discovers which axes the data varies along, and lets the user scale each
independently.

---

## 2. User Experience

### 2.1 Discover Dimensions

```bash
knit scale blueprint.knit.toml --analyze
```

Output:

```
═══ Scaling Analysis ═══

  Dimension     Type       Current          Description
  ─────────     ────       ───────          ───────────
  actors        built-in   8 people         AnalyzedUser (actor entity, FK-root for Collab, PeopleHistorical)
  time          built-in   10 partitions    Collab.PartitionDate, cadence ≈ 7d, 2024-10-13 .. 2024-12-24
  location      custom     3 values         Collab.Region one_of: [US, EU, APAC] (weights: 0.5, 0.3, 0.2)
  rows          built-in   77 total         Uniform row scaling (--count)

Suggested commands:
  knit scale blueprint.knit.toml -o out/ --actors 100
  knit scale blueprint.knit.toml -o out/ --time 52w
  knit scale blueprint.knit.toml -o out/ --dim location=10
  knit scale blueprint.knit.toml -o out/ --actors 100 --time 52w --dim location=10
```

### 2.2 Generate Scaled Data

```bash
# Scale people to 100, extend time to 52 weeks
knit scale blueprint.knit.toml -o output/ --actors 100 --time 52w --format csv

# Scale a custom dimension (location) from 3 to 20 values
knit scale blueprint.knit.toml -o output/ --dim location=20

# Combine all dimensions
knit scale blueprint.knit.toml -o output/ --actors 100 --time 52w --dim location=20

# Preview what would be generated without writing files
knit scale blueprint.knit.toml --actors 100 --time 52w --dry-run
```

---

## 3. Dimension Types

### 3.1 Built-in Dimensions

These are auto-detected from blueprint structure and require no user annotation.

#### Actors (`--actors N`)

The "people" axis. Scales the actor/root entity — the entity that drives child
table row counts through foreign-key relationships.

**Detection:** Entity with `actor = true`, or the FK-root entity with the most
downstream dependents.

**Scaling behavior:**
- Actor entity count set to N
- Child entity counts scale proportionally: `new_child = N × (old_child / old_actor)`
- FK generators automatically sample from the larger key pool
- New actors get values drawn from the same learned distributions
- Persona weights and trait distributions are preserved

**Multiple actor entities:** If ambiguous, `--analyze` lists candidates with
confidence scores. User specifies `--actors "AnalyzedUser=100"`.

#### Time (`--time SPEC`)

The temporal axis. Extends the partition date range.

**Detection:** Entity with `partition_by` in its `OutputLayout` where partition
values parse as dates.

**SPEC formats:**
- Duration: `52w`, `6m`, `365d`, `2y`
- Explicit range: `2024-01-01..2025-12-31`
- Relative: `+26w` (extend 26 weeks beyond current end)

**Scaling behavior:**
- Cadence detected from median gap between sorted partition dates
- New partition values generated at cadence intervals to fill the target range
- Per-partition row count = learned average, scaled by actor ratio if combined
- Partition weights rebalanced to be uniform across new values

**Cadence detection:** If gap variance exceeds 50% of median, a warning is
emitted suggesting `--cadence` override. The user can specify `--cadence 7d` or
`--cadence 1w` to override the detected cadence. Only `d` (days) and `w` (weeks)
units are supported — month-based cadence (`1m`) is not available because
fixed-day stepping drifts off calendar month boundaries.

#### Uniform (`--count Nx`)

Existing mechanism. Multiplies all entity counts by a factor. Can combine with
`--actors` and `--time` for additional density scaling.

### 3.2 Custom Dimensions (`--dim NAME=N`)

Custom dimensions are categorical fields that the data naturally varies along.
They are **auto-detected** from the blueprint but scaled via the generic `--dim`
flag.

**Detection criteria — a field is a custom dimension candidate when:**
1. It uses a `one_of` generator with discrete weighted values
2. It appears in multiple child rows per actor (not a per-actor attribute)
3. It has low cardinality relative to row count (≤ 50 distinct values, or
   cardinality / rows < 0.1)
4. It is NOT the partition key (that's the time dimension)
5. It is NOT a foreign key (those scale with their parent entity)

**Examples of auto-detected custom dimensions:**
- `Region` with `one_of: [US, EU, APAC]` → location dimension
- `Department` with `one_of: [Eng, Sales, HR, Marketing]` → org dimension
- `ProductCategory` with `one_of: [Electronics, Books, Clothing]` → product dimension
- `SignalType` with `one_of: [Meeting, Email, IM]` → activity type dimension

**Scaling behavior for `--dim location=20`:**
- The `one_of` generator's value set is expanded from 3 to 20 values
- New values are generated using the field's naming pattern (e.g., `Location_4`,
  `Location_5`, ... or from a faker category if one is configured)
- Weights for new values follow the learned distribution shape (e.g., Zipfian
  if original weights were Zipfian)
- If the dimension field has conditional generators keyed on it, new values
  get the default branch behavior
- Total rows scale: `new_rows = old_rows × (new_cardinality / old_cardinality)`

**Naming strategies for new values:**
| Pattern | Strategy | Example |
|---------|----------|---------|
| Geographic codes | Faker locale names | `JP`, `BR`, `IN` |
| Numeric IDs | Sequential | `Region_4`, `Region_5` |
| English words | Faker category | `Furniture`, `Toys` |
| Unknown | Indexed suffix | `value_4`, `value_5` |

The naming strategy is inferred from the existing values. Users can override
with `--dim location=20:faker=country_code`.

**Interaction with conditional generators:** When a dimension field (e.g.,
`SignalType`) is used as the condition key for other fields' conditional
generators, scaling that dimension adds new values that route to the default
branch. This preserves blueprint consistency — new signal types produce null
values for signal-specific columns, matching the learned default behavior.

---

## 4. Dimension Interaction

When multiple dimensions are scaled simultaneously, they interact multiplicatively:

```
total_rows ≈ actors × time_periods × density_per_actor_per_period × dim_scaling
```

**Example:**
- Source: 8 actors, 10 weeks, 3 locations → 60 Collab rows
- Density = 60 / (8 × 10) = 0.75 rows/actor/week (across all locations)
- Scale: `--actors 100 --time 52w --dim location=10`
- New Collab rows ≈ 100 × 52 × 0.75 × (10/3) ≈ 13,000

The `--dry-run` flag shows computed counts before generating:

```bash
knit scale blueprint.knit.toml --actors 100 --time 52w --dim location=10 --dry-run

# Output:
# ═══ Scaling Plan (dry run) ═══
#
#   Entity            Current    Scaled     Factor
#   ──────            ───────    ──────     ──────
#   AnalyzedUser      8          100        12.5×
#   Collab            60         13,000     216.7×
#   PeopleHistorical  9          112        12.4×
#
#   Partitions: 10 → 52 (weekly, 2024-10-13 .. 2025-10-05)
#   Locations:  3 → 10 (US, EU, APAC, + 7 new)
#
#   Estimated output size: ~32 MiB
```

---

## 5. Implementation Architecture

### 5.1 Pipeline

```
analyze(blueprint) → ScalingPlan → rewrite(blueprint, plan) → generate(modified_blueprint)
```

`knit scale` is an orchestration layer over existing `generate`. No changes to
the core generation engine are required for v1.

### 5.2 Analysis Phase

```rust
pub struct ScalingAnalysis {
    /// Detected actor entity and its current count
    actor: Option<ActorDimension>,
    /// Detected time dimension (partition-based)
    time: Option<TimeDimension>,
    /// Detected custom dimensions (categorical fields)
    custom: Vec<CustomDimension>,
    /// Current total rows per entity
    entity_counts: BTreeMap<String, u64>,
}

pub struct ActorDimension {
    entity_name: String,
    current_count: u64,
    /// Entities whose row counts are driven by this actor via FK
    dependents: Vec<(String, f64)>,  // (entity_name, cardinality_ratio)
    confidence: f64,  // 0.0-1.0
}

pub struct TimeDimension {
    entity_name: String,
    partition_field: String,
    partition_values: Vec<String>,
    cadence: Duration,
    cadence_confidence: f64,
    range_start: NaiveDate,
    range_end: NaiveDate,
}

pub struct CustomDimension {
    entity_name: String,
    field_name: String,
    current_values: Vec<(String, f64)>,  // (value, weight)
    /// Whether this field is a condition key for other generators
    is_condition_key: bool,
    /// Suggested naming strategy for new values
    naming_strategy: NamingStrategy,
}
```

### 5.3 Rewrite Phase

The plan phase computes a `ScalingPlan`:

```rust
pub struct ScalingPlan {
    /// New entity counts
    entity_overrides: BTreeMap<String, u64>,
    /// New partition values (if time dimension scaled)
    new_partitions: Option<Vec<PartitionValue>>,
    /// New one_of values for custom dimensions
    dim_overrides: Vec<DimOverride>,
}

pub struct DimOverride {
    entity_name: String,
    field_name: String,
    new_values: Vec<(String, f64)>,  // expanded value set with weights
}
```

The rewrite phase applies the plan to a cloned `DataModel`:
1. Override entity counts
2. Replace `partition_values` in `OutputLayout`
3. Replace `one_of` choices in affected field generators
4. Pass modified model to `generate::run()`

### 5.4 File Organization

```
src/cli/commands/scale.rs    — CLI command, --analyze output formatting
src/scale/mod.rs             — ScalingAnalysis, ScalingPlan, rewrite logic
src/scale/analyze.rs         — Dimension detection algorithms
src/scale/time.rs            — Cadence detection, date extension
src/scale/custom.rs          — Custom dimension detection, value generation
```

---

## 6. CLI Specification

```
knit scale <SCHEMA> [OPTIONS]

Arguments:
    <SCHEMA>                  Path to the learned blueprint (.knit.toml)

Options:
    --analyze                 Show discovered dimensions without generating
    --dry-run                 Show scaling plan (computed counts) without generating
    -o, --output <DIR>        Output directory (required for generation)
    --actors <SPEC>           Scale actor dimension (N or "EntityName=N")
    --time <SPEC>             Scale time dimension (52w, 6m, 365d, 2024-01-01..2025-12-31)
    --dim <NAME=N>            Scale custom dimension (repeatable)
    --count <N|Nx>            Additional uniform row scaling
    --cadence <DURATION>      Override detected time cadence (e.g. 7d, 1w, 14d)
    --format <FMT>            Output format (csv, parquet, json, etc.)
    --seed <N>                Random seed
    --quiet                   Suppress progress output
    --json                    Machine-readable JSON output
```

---

## 7. Edge Cases and Error Handling

| Case | Behavior |
|------|----------|
| No actor entity detected | Error: "No actor entity found. Mark an entity with `actor = true` or ensure FK relationships exist." |
| Multiple actor candidates | List candidates with confidence, require `--actors "Name=N"` |
| No partitions for `--time` | Error: "No time dimension detected (no partitioned entities with date values)." |
| Irregular cadence (>50% variance) | Warning with detected confidence; suggests `--cadence` override |
| `--cadence` without `--time` | Error: "--cadence requires --time" |
| `--dim` for non-existent field | Error: "Dimension 'X' not found. Run `--analyze` to see available dimensions." |
| `--dim` on FK field | Error: "Cannot scale FK field 'X'. Scale the parent entity with `--actors` instead." |
| `--dim` on partition field | Error: "Cannot scale partition field 'X'. Use `--time` instead." |
| Scale to fewer than current | Allowed (downscaling). Proportional reduction. |
| `--dim SignalType=2` (reduce) | Allowed. Keeps the top-N values by weight, renormalizes. |
| Combined `--actors` + `--count` | `--actors` sets base count, `--count Nx` multiplies on top |
| >1000× total scaling | Warning: "This will generate ~X rows. Continue? [y/N]" |

---

## 8. Future Work (v2+)

### 8.1 Temporal Field Re-anchoring
Inject the partition date into `GenContext` so temporal generators (timestamps,
business_hours, event_streams) anchor their output to the partition date. Without
this, in-row timestamps may not match the partition folder date.

### 8.2 Density Control (`--density`)
Separate knob for rows-per-actor-per-period. Scoped per entity:
`--density Collab=2x` doubles Collab rows without changing actor count or time.

### 8.3 Correlated Dimension Scaling
When dimensions are correlated (e.g., more locations → more actors per location),
allow specifying the interaction: `--dim location=20 --actors-per-location 5`.

### 8.4 Blueprint-Level Dimension Annotations
Allow users to explicitly mark fields as scaling dimensions in the blueprint:
```toml
[[entities.fields]]
name = "Region"
scaling_dimension = true
scaling_name = "location"
```

### 8.5 Smart Value Generation for Custom Dimensions
Use learned data distributions, dictionary files, or faker categories to generate
semantically meaningful new values instead of indexed placeholders.

### 8.6 Constraint Propagation
When scaling a dimension that appears in multiple entities, ensure referential
consistency (e.g., if `Region` appears in both `Orders` and `Inventory`, new
region values appear in both).

---

## 9. Implementation Status

### v1 (Implemented)

The following features are implemented and available via `knit scale`:

| Feature | Status | Notes |
|---------|--------|-------|
| `--analyze` | ✅ | Discovers actor, time, and custom dimensions |
| `--actors N` | ✅ | Scales actor entity and proportional dependents |
| `--time SPEC` | ✅ | Duration (`52w`), relative (`+26w`), explicit range |
| `--dim NAME=N` | ✅ | Scales OneOf custom dimensions |
| `--dry-run` | ✅ | Shows planned counts without generating |
| `--format` | ✅ | Delegates to generate pipeline |
| `--seed` | ✅ | Deterministic scaling |
| Combined dimensions | ✅ | Multiplicative interaction |
| Cadence detection | ✅ | Median gap from sorted partition dates; monthly (28–31d) auto-detected |
| `--cadence` override | ✅ | Days (`7d`), weeks (`2w`), months (`1m`, `3m`) with calendar stepping |
| FK-root actor heuristic | ✅ | Selects entity with most incoming FKs |

**Architecture (as implemented):**

```
src/scale/mod.rs       — ScalingAnalysis, ScalingPlan, compute_plan(), rewrite()
src/scale/analyze.rs   — analyze(), detect_actor(), detect_time(), detect_custom_dimensions()
src/scale/time.rs      — compute_new_partitions(), parse_duration_days()
src/cli/commands/scale.rs — CLI handler, --analyze formatting, --dry-run display
```

The `generate::run()` function was refactored into `run()` + `run_from_model()` to
allow the scale command to inject a modified `DataModel` and reuse the full
generation pipeline (format selection, partitioning, output writing).

### Deferred to v2

- Smart value naming (faker-based) for expanded custom dimensions
- `--count Nx` uniform multiplier on top of dimensional scaling
- Constraint propagation for cross-entity dimension fields
- JSON machine-readable output for `--analyze`
- Estimated output size in `--dry-run`
