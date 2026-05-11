# Changelog

All notable changes to Knit are documented in this file.

## [Unreleased]

### Added

- **Source data statistics** — `knit learn` now populates `[stats]` sections on
  both fields and entities. Field stats include numeric summaries (min/max/mean/std
  /percentiles), string lengths, temporal ranges, and top values. Entity stats
  capture `source_rows` and optional per-partition row distribution.
- **Auto-detected field traits** — `knit learn` detects qualitative column traits:
  `semantic` (email, uuid, date, integer, float, categorical, etc.), `pii` (true
  for email/phone/name patterns), `cardinality` (low/medium/high/unique),
  `distribution_shape` (uniform/normal/skewed/long_tail). Traits are metadata-only
  and stored alongside stats in the learned model.
- **Output folder hierarchy** — `knit learn` captures the source dataset's
  directory layout (e.g. `Entity/Results/`) and `knit generate` reproduces
  the same folder structure in output via the new `[entities.output]` section.
- **Column order preservation** — Generated output now preserves the original
  column order from the schema, even when fields have inter-dependencies that
  require a different generation order internally.
- **Companion schema discovery** — `knit learn` detects structured dataset layouts
  with `Schema/schema.json` alongside data files and uses metadata for richer
  learning (entity naming, row-type discriminators, dictionary references).
- **Dictionary file handling** — Dictionary CSV files referenced by companion
  schemas are automatically copied to the output `Mappings/` directory.
- **Row-type conditional generation** — Columns with `rowType` metadata in
  companion schemas produce `conditional` generators keyed on the discriminator
  column (e.g. `SignalType`), ensuring row-type-specific columns are null when
  the discriminator doesn't match.
- **Partitioned CSV merging** — Partitioned data directories (e.g.
  `PartitionDate=YYYY-MM-DD`) are merged into a single logical table during
  ingestion, with automatic schema unification across partitions.

### Fixed

- **Always-null column detection** — Columns that are 100% null or empty string
  in source data now correctly get `NullSpec::Always` instead of being fitted
  with distribution generators. Handles both Arrow nulls and CSV empty strings.
- **Partition schema drift** — Partitions with `Null`-typed columns (entirely
  empty in that partition) are now correctly cast to the concrete type found in
  other partitions, preventing concatenation errors.

### Added (prior)

- **WASM plugin architecture** — Load custom generator plugins from `.wasm`
  modules at runtime without recompilation (requires `wasm-plugins` feature):
  - CLI flags: `--plugin path/to/gen.wasm` (repeatable) and `--plugin-dir ./plugins/`
  - ABI v1 contract: `knit_abi_version`, `knit_name`, `knit_output_type`,
    `knit_create`, `knit_generate`, `knit_destroy`, `knit_alloc`, `knit_free`
  - Supports Int64, Float64, Utf8, and Boolean output types
  - JSON array data exchange between host and guest
  - Seed-based deterministic RNG (host passes per-call seed to guest)
  - Graceful fallback: WASM traps produce null arrays + error log
  - Duplicate plugin name detection for file-loaded plugins
  - Example plugin: `examples/wasm_plugin/` (random float generator)
  - Feature-gated: `cargo install knit --features wasm-plugins`

### Fixed

- Removed unused `Utc` import in compiler.rs

## [0.4.0] — 2026-05-10

### Added

- **Relative generator enhancement** — Structured offset distributions for relative
  datetime generators (spec §9.5):
  - **Distribution offsets**: `offset = { distribution = "log_normal", params = { mu = 1.5, sigma = 0.8 }, min = "1d", max = "14d", unit = "day" }`
  - Supports Normal, LogNormal, Uniform, and Exponential distributions
  - Duration-based `min`/`max` clamping (e.g. `min = "1d"`, `max = "14d"`)
  - **Constant offsets**: `offset = { type = "constant", value = "365d" }` for deterministic intervals
  - **Simple offsets** (backward compatible): `offset = 86400` still works
  - Renamed `field` to `anchor` (with `field` as alias for backward compatibility)
  - Schema validation: only scalar continuous distributions allowed, min ≤ max checked,
    duration strings validated
  - Int64 base field support for anchoring to temporal sequences
- **Temporal sequences** — Sequence generators now accept string date/datetime starts
  and duration-string steps (spec §6.3):
  - `start = "2024-01-01"` or `start = "2024-01-01T08:00:00"` (parsed to epoch ms)
  - `step = "1d"` or `step = "1h"` (parsed to ms duration)
  - Supports date-only, naive datetime, UTC, and offset-aware formats
  - Combined with `data_type = "datetime"` for realistic timestamp columns
  - Integer start/step continue to work unchanged
- **Sequence jitter** — Random offset for sequence generators (spec §6.13):
  - `jitter = "30m"` adds uniform random offset from `[-30m, +30m]` per value
  - Supports duration strings: `"500ms"`, `"5m"`, `"1h"`, `"1d"`, `"1w"`
  - Primarily useful for temporal sequences (epoch-ms timestamps)
  - Deterministic with same seed; partition-safe
- **`not_null` constraint** — Declare fields that must never contain null (spec §5.4):
  - `type = "not_null"` with `fields = ["id", "email"]`
  - Schema validation checks referenced fields exist and list is non-empty
  - Also validates field references in `unique` and `range` constraints
- **Entity tags** — Freeform semantic annotation on entities (spec §5):
  - `tags = ["pii", "core"]` on any `[[entities]]` section
  - Tags are metadata-only (preserved in schema, do not affect generation)
  - Defaults to empty; omitted when serializing empty tags
- **Pipe operator `|>`** — Expression function composition for derived fields
  (spec §6.6):
  - `expr |> func(args)` desugars to `func(expr, args)` at parse time
  - Supports chaining: `${x} |> abs() |> round(2)` → `round(abs(${x}), 2)`
  - Lowest precedence — all arithmetic/comparison/logical operators bind tighter
  - Right side must be a function call; invalid usage produces clear parse errors
  - Expressions containing `|>` never fall back to legacy string template mode
  - New example: `examples/pipe_operator.knit.toml`
- **Dirichlet and Multinomial distributions** — Vector-valued distributions
  for generating category proportions and multi-category counts (spec §6.1):
  - `kind = "dirichlet"` with `array_params.alpha` → probability simplex
  - `kind = "multinomial"` with `params.n` + `array_params.p` → integer counts
  - `array_params` field on `DistributionSpec` for array-typed parameters
  - Dirichlet uses `rand_distr::Dirichlet`; Multinomial uses sequential-binomial method
  - Output as `ListArray` (compatible with JSON, Parquet, Avro sinks)
  - Validation: alpha ≥ 2 elements all > 0; p ≥ 2 elements summing to ~1.0, n ≥ 1
  - New example: `examples/vector_distributions.knit.toml`
- **Holiday effect time series component** — Date-based multiplicative
  spikes and dips in both numeric time series and event streams (spec §9.2):
  - `type = "holiday_effect"` component with `dates` and `multiplier`
  - Dates in `YYYY-MM-DD` format; `multiplier > 1.0` = spike, `< 1.0` = dip
  - Pre-compiled `HashSet<NaiveDate>` for O(1) date lookup in numeric time series
  - Supported in both `time_series` (numeric) and `event_stream` generators
  - Validation: non-empty dates, valid YYYY-MM-DD, non-zero multiplier (positive for event streams)
  - New example: `examples/holiday_effect.knit.toml`
- **Timezone-aware business hours** — Full timezone, date range, and holiday
  support for business hours timestamps (spec §6.13):
  - `timezone = "America/New_York"` for fixed timezone (local→UTC conversion)
  - `timezone_field = "tz_col"` for per-row timezone from another field
  - `days = ["Monday", "Wednesday", "Friday"]` for custom active days
  - `date_range = { min = "2024-01-01", max = "2024-12-31" }` to constrain dates
  - `exclude_dates = ["2024-12-25", "2024-07-04"]` for holidays/blackout dates
  - DST-aware: ambiguous → earliest, nonexistent → shift forward
  - Validation: timezone/timezone_field mutual exclusivity, day names, date formats
  - New example: `examples/timezone_business_hours.knit.toml`
- **Conditional distributions** — Model distribution-dependent correlations
  between fields (spec §8.3):
  - `type = "conditional_distribution"` on `[[correlations]]`
  - Different distributions per condition branch (e.g., log_normal for
    groceries, normal for travel)
  - Optional `default` distribution for unmatched values
  - Compiles into existing conditional generator infrastructure
  - Validation: field existence, mutual exclusivity, duplicate branches
  - New example: `examples/conditional_distribution.knit.toml`
- **Sequence cyclic values** — Cycle through fixed value lists round-robin
  (spec §6.3):
  - `values = ["Mon", "Tue", "Wed", ...]` with `cycle = true`
  - Deterministic, partition-safe assignment via row position
  - Validation: empty values, mutual exclusivity with start/step, cycle requires values
  - New example: `examples/sequence_values.knit.toml`
- **Graph edge properties** — Attach additional columns to relationship edges via
  `[[relationships.properties]]` (spec §10.2):
  - Edge properties become extra columns on the `from` entity
  - Support all generator types (distribution, one_of, faker, etc.)
  - Nullable specification per edge property
  - Validation: name conflict detection, many_to_many rejection
  - New example: `examples/edge_properties.knit.toml`
- **Self-referential hierarchy controls** — Configure tree/forest structure for
  self-referential relationships (spec §7.4):
  - `acyclic = true` — guarantees no circular reference chains (true tree/forest)
  - `root_probability = 0.05` — controls fraction of root nodes (null FK)
  - `max_depth = 6` — limits maximum hierarchy depth
  - `nullable = true` on relationships — required for hierarchies with root nodes
  - Hierarchical assignment uses O(N) algorithm with eligible-parents vector
  - Processing order shuffled deterministically for varied tree shapes
  - Schema validation: range checks, self-ref-only constraints, nullable enforcement
  - New example: `examples/hierarchy.knit.toml`
- **Parameter expressions in count** — Entity counts can now be computed from
  model parameters using expressions (spec §3). Enables scalable blueprints where
  all entity sizes are driven by a few top-level parameters.
  - `count = { expr = "${param.user_count} * ${param.scale}" }` syntax
  - Supports arithmetic, parameter refs, numeric literals, and pure math functions
  - Rejects field references, `row_number()`, and random functions in count context
  - Float results are rounded; zero/negative results produce errors
  - Works with `--count` scale override (expression evaluated first, then scaled)
  - Schema validation for expression parse errors and forbidden AST nodes
  - New example: `examples/count_expressions.knit.toml`
- **Relationship selection strategies** — Control how children pick their parent
  FK target (spec §7.3). Three strategies available:
  - `selection = "sequential"` — deterministic round-robin based on child row
    position; produces perfectly even distribution across parents
  - `selection = { strategy = "clustered", cluster_size = 20 }` — consecutive
    children reference nearby parents for locality-based grouping
  - `selection = "uniform"` — random (default, same as omitting `selection`)
  - Mutually exclusive with `degree` (validated at schema level)
  - Works with both integer and string/UUID foreign keys
  - Schema validation for cluster_size > 0, weight_field existence/type
  - New example: `examples/selection_strategies.knit.toml`
- **Relationship degree distribution** — Non-uniform FK assignment using Zipf
  or other distributions (spec §7.2). Some parents receive disproportionately
  more children, producing realistic power-law cardinality patterns.
  - Add `degree = { kind = "zipf", params = { exponent = 1.2 } }` to any
    `[[relationships]]` block
  - Supports Zipf distribution (other kinds fall back to uniform)
  - Works with both integer and string/UUID foreign keys
  - Direct Zipf sampling via `rand_distr::Zipf` — O(1) per sample
  - `StringKeyStore` now supports `get_by_index()` for rank-based lookup
  - Schema validation for degree distribution parameters
  - New example: `examples/degree_distribution.knit.toml`
- **Event streams** — Irregular time series with random inter-arrival times
  (spec §9.3). Generates strictly-increasing timestamps using an exponential
  distribution, optionally modulated by rate components.
  - Add `type = "event_stream"` generator with `start`, `arrival`, and `components`
  - Arrival distribution: `exponential` with configurable `lambda` and time unit
  - Rate modulation via Lewis-Shedler thinning: `seasonality`, `weekend_effect`,
    `business_hours` components control temporal event density
  - Stateful across batches — cumulative timestamps remain monotonic
  - Forces sequential execution to maintain inter-arrival state
  - Schema validation for start time, distribution, lambda, unit, and components
  - New example: `examples/event_stream.knit.toml`
- **Scoped noise** — Conditional noise injection that restricts perturbation
  to rows matching a predicate expression (spec §11.4).
  - Add `scope = { where = '${field} == "value"' }` to any `[[noise]]` profile
  - Scope predicates use the Knit expression language (same as derived fields)
  - Probability is applied *after* scope filtering (the two multiply)
  - Works with all 11 perturbator types including row-level injectors
    (DuplicateInjector, SwapInjector)
  - Scope expressions are parsed once and evaluated per-batch for efficiency
  - Schema validation checks expression syntax and field references
  - New example: `examples/scoped_noise.knit.toml`
- **Mixins** — Reusable field groups that can be included in multiple entities
  via the `[[mixins]]` schema section (spec §5.6).
  - Define named field groups with `[[mixins]]` and reference them with
    `mixins = ["name"]` on entities
  - Mixin fields are prepended to entity fields in declared order
  - Entity fields with the same name override mixin definitions
  - Mixin-vs-mixin field name collisions produce clear errors
  - Works with custom types (mixin fields can use custom type references)
  - Works with include and extends composition (mixins merge by name)
  - New example: `examples/mixins.knit.toml`
- **Custom domain types** — Define reusable type aliases with default generators,
  precision, and nullable settings via the `[[types]]` schema section (spec §4.3).
  - Fields using a custom type inherit `base`, `generator`, `precision`, and
    `nullable` from the type definition
  - Field-level overrides take precedence over custom type defaults
  - Validation: built-in name conflicts, duplicate names, complex base type
    rejection, undefined type reference errors
  - Works with include and extends composition (custom types merge by name)
  - New example: `examples/custom_types.knit.toml`
- **SQL INSERT output format** — New `--format sql` output that generates
  standard SQL INSERT statements from Arrow data. Features:
  - Multi-row VALUES syntax with configurable batch size (default: 100 rows
    per INSERT statement)
  - Optional CREATE TABLE DDL via `--sql-create-table` flag
  - Optional transaction wrapping via `--sql-transaction` flag
  - Proper identifier quoting (double-quotes) for reserved words
  - Complete Arrow-to-SQL type mapping (INTEGER, BIGINT, REAL, DOUBLE
    PRECISION, TEXT, BOOLEAN, DATE, TIMESTAMP, TIME, NUMERIC)
  - Correct literal formatting: single-quote escaping, ISO dates/timestamps,
    NaN/Infinity → NULL, hex-encoded binary
  - Complex types (struct, list, map) serialized as JSON TEXT
- **Expanded faker generator** — 40+ new faker methods covering spec §6.2
  provider categories. Dotted provider names (`internet.email`,
  `finance.credit_card`) are normalized automatically.
  - Person: `prefix`, `suffix`
  - Internet: `mac_address`, `user_agent`
  - Finance: `credit_card` (valid Luhn checksum), `iban` (valid mod-97 check
    digits), `bic`/`swift`, `currency_code`
  - Geo: `latitude`, `longitude`, `coordinate` (string output)
  - Datetime: `time`, `month`, `day_of_week`, `timezone`
  - File: `file_extension`, `mime_type`, `file_name`, `file_path`
  - Vehicle: `license_plate`, `vin` (valid 17-char, no I/O/Q), `vehicle_make`,
    `vehicle_model`
  - Medical: `blood_type`
  - Barcode: `ean13` (valid check digit), `isbn13` (978/979 prefix + check)
  - Company: `industry`, `catch_phrase`, `bs`
  - Address: `street_address`, `city_name`, `country_code`
- **Schema composition** — `include` directive for composing blueprints from
  reusable fragment files. Supports recursive includes, diamond-safe
  include-once semantics, cycle detection, and security path restrictions.
  Fragment validation rejects `[model]` sections and `extends` in included
  files.
- Modular example blueprints in `examples/modular/`
- **Expression engine** — Full expression language for derived fields with:
  - Pratt parser with proper operator precedence
  - 63+ built-in functions: math (`abs`, `ceil`, `floor`, `round`, `min`,
    `max`, `clamp`, `sqrt`, `pow`, `log`, `ln`, `exp`), string (`upper`,
    `lower`, `trim`, `len`, `concat`, `substr`, `replace`, `left`, `right`,
    `pad_left`, `pad_right`, `starts_with`, `ends_with`, `contains`), type
    casts (`cast_int`, `cast_float`, `cast_string`), conditionals (`if`,
    `coalesce`, `nullif`, `case`), utility (`hash`, `row_number`),
    random (`random_int`, `random_float`, `random_duration`),
    date/time construction (`make_date`, `make_time`, `make_datetime`,
    `make_duration`, `to_date`, `to_datetime`, `epoch_seconds`, `from_epoch`),
    date/time extraction (`year`, `month`, `day`, `hour`, `minute`, `second`,
    `day_of_week`, `day_of_year`, `week_of_year`, `quarter`),
    date/time arithmetic (`date_add`, `date_sub`, `date_diff`, `duration_add`,
    `start_of`, `end_of`), date/time formatting (`format_date`, `format_duration`),
    timezone (`to_timezone`, `timezone_offset`)
  - SQL three-valued null logic for `&&`/`||`
  - Domain-error handling (sqrt of negative → null, ln of non-positive → null)
  - Deterministic SipHash for `hash()` function
  - Immutable per-row seeding for `random_*` functions — batch-size independent,
    no shared RNG coupling between fields
  - Global `row_number()` with cross-batch offset tracking
  - Mixed numeric type promotion (Int64/Float64 → Float64 in if/coalesce)
  - UTF-8 safe string operations (character-based indexing)
  - Vectorized evaluation over Arrow arrays with SQL-like null propagation
  - Backward compatible with legacy string templates
  - AST-based dependency extraction in the plan compiler
- **Noise pipeline expansion** — 4 new perturbators:
  - `SwapInjector` — swap values between rows (clean stage, preserves multiset)
  - `TruncateInjector` — truncate strings at random char boundaries (UTF-8 safe)
  - `FkViolateInjector` — corrupt FK values with non-existent references
  - `TemporalSpikeInjector` — cluster timestamps around spike points (Gaussian spread)
  - Total: 11 built-in perturbators across clean/constrained/breaking stages
- **External lookup generator** — Sample values from external CSV, JSON, or
  Parquet files with three sampling modes (uniform, weighted, sequential).
  File loading is deferred to plan resolution time (like dictionary). Supports
  weighted sampling via a weight column, deterministic sequential round-robin
  via row offset, and path traversal protection.
- **Graph topology expansion** — 3 new topology models for relationship graphs:
  - `stochastic_block` — community structure with configurable intra/inter
    edge probabilities (simplified SBM with equal community sizes)
  - `configuration` — custom degree distribution (Poisson or power-law with
    stub-pairing algorithm)
  - `complete` — fully connected graph (uniform random target selection)
  - Total: 7 topology models (was 4)
- **Avro output format** — Apache Avro Object Container Format (OCF) output
  via `--format avro`. Supports Null, Deflate, and Snappy compression codecs.
  Full Arrow-to-Avro type mapping including nullable columns (union types),
  timestamps, lists, and binary data. Entity names are used as Avro record
  names.
- **`missing_field` noise type** — Randomly omit fields from document output
  (JSON/JSONL) to simulate semi-structured data. Controlled via
  `missing_field_rate` in noise profiles. Deterministic per-row RNG for
  reproducibility. Non-document formats (CSV, Parquet, Avro) emit a warning.
- **Schema validation improvements** — Enhanced semantic validation:
  - Derived expression validation: parse expressions at schema-check time,
    verify field references exist on the entity, detect self-references
  - Legacy template fallback: `"Hello ${name}"` templates pass validation
    even when they fail expression parsing
  - Dependency cycle detection via DFS across derived/relative/conditional
    field references
  - Learn output validation: `knit learn` now validates assembled schemas
    and warns about issues before writing output
- **Copula-based joint distributions** — Multivariate dependency modeling via
  copula families (spec §8.4):
  - Gaussian copula: arbitrary n-dimensional with Cholesky decomposition
  - Clayton copula: bivariate, lower-tail dependence (θ > 0)
  - Frank copula: bivariate, symmetric dependence (θ ≠ 0)
  - Gumbel copula: bivariate, upper-tail dependence (θ ≥ 1)
  - Marginal distribution preservation via inverse CDF transform
  - Schema validation: family-specific parameter checks, PSD matrix verification,
    Archimedean family restricted to exactly 2 fields
  - Entity-level plan compilation with `CopulaPlan` on `EntityPlan`
- **Nested objects/structs** — Hierarchical document structures (spec §5.5):
  - `data_type = "object"` fields with recursive `fields` sub-fields
  - Arbitrary nesting depth for document-oriented output (JSON, Parquet, Avro)
  - Precision rounding applied within nested struct fields
  - All output formats supported: JSON/JSONL (native nesting), Parquet/Arrow IPC
    (native struct columns), Avro (real nested record schemas), CSV (JSON strings)
  - Schema validation: restricts nested fields to simple generators only
    (distribution, faker, constant, sequence, one_of, uuid_gen); disallows
    primary_key, actor_column, FK, graph_target, persona, derived, relative,
    conditional in nested fields
  - Avro struct support: full Arrow Struct → Avro Record type mapping with
    recursive schema conversion and union-wrapped nullable fields
- **Numeric time series generator** — Composable additive time series for
  generating realistic metric data (CPU usage, temperature, network traffic):
  - 9 component types: trend (polynomial), seasonality (sinusoidal), noise
    (Gaussian), autoregressive (AR with configurable lag coefficients), spike
    (anomalous bursts), level_shift (permanent baseline change), mean_reversion,
    weekend_effect, and business_hours_effect
  - Calendar-aware components via `timestamp_field` reference
  - Optional `[min, max]` output clamping
  - Stateful components use interior mutability with automatic sequential
    partition execution for deterministic output
  - Duration string parsing for seasonality periods (`"24h"`, `"7d"`, `"15m"`)
  - Schema validation: AR coefficient stability, calendar field existence,
    business hours range, min < max
  - Example schema: `examples/time_series_metrics.knit.toml`

## [0.3.0] — 2026-05-08

### Changed

- **Single-crate refactor** — Consolidated 9-crate workspace into a single
  `knit` crate for simpler installation, faster compilation, and crates.io
  publishing. All functionality is preserved; only the internal module layout
  changed.
- Module paths now use `knit::{core,blueprint,plan,gen,noise,bind,learn,cli}::`
  instead of separate `knit_core::`, `knit_blueprint::`, etc. crates.

### Fixed

- Cross-partition uniqueness enforcement (shared seen-set across partitions)
- Plugin system pipeline integration (graceful errors, correct lock handling)

## [0.2.0] — 2026-05-07

### Added

- **Behavioral modeling** — Full human behavior simulation pipeline:
  - **Personas** — Define actor segments with weighted trait distributions
    (activity rates, temporal preferences, spending patterns)
  - **Activity-driven rows** — Entity row counts determined by per-actor trait
    sums instead of fixed counts (`activity_count`)
  - **Actor temporal** — Timestamp generation biased toward persona peak hours
    with burst/session clustering and inter-event gap enforcement
  - **Actor relationships** — Graph-based relationships between actors
    (scale-free, small-world, hierarchical, Erdős–Rényi topologies)
  - **Relationship ref** — FK generation following graph edges instead of
    uniform sampling
  - **Persona field** — Field values drawn from persona trait distributions
  - **Thread ref** — Self-referential conversation threading with configurable
    reply probability, depth limits, and recency-weighted reply targets
  - **Cross-entity causal ordering** — `temporal_after` ensures child entity
    timestamps follow parent entity events
  - **Actor identity resolution** — Cross-entity actor unification via FK-based
    and name-based namespace inference (`knit learn --actors`)
- **Learn behavioral patterns** — `knit learn --actors` detects actor columns,
  profiles per-actor features, clusters into personas, and discovers actor
  relationship graphs
- **Inspect behavioral summary** — `knit inspect <schema> --actors` shows
  persona groups, actor relationships, and behavioral generators
- **Behavioral CLI flags** — `--actor-column` for explicit actor column
  specification, `--personas` for persona count control in `knit learn`
- **Behavioral output in validate/plan** — `knit validate` and `knit plan`
  now display persona counts, actor relationships, and behavioral generator
  summaries
- **Behavioral examples** — Four example blueprints exercising behavioral modeling:
  `social_platform`, `ecommerce_behavioral`, `email_traffic`, `hr_org`
- **GitHub Actions CI** — Automated check, test, clippy, fmt, and doc jobs

### Changed

- README updated with behavioral modeling documentation, examples, and CLI
  reference for `--actors` flag
- Design doc §12.3 example filenames corrected to match actual files
- Applied `cargo fmt` across entire workspace

### Fixed

- All clippy warnings resolved across workspace
- Ignored doc tests converted to compilable `no_run` examples (graph, plugin,
  pipeline)
- Fixed 6 broken rustdoc links (types.rs, actor_pool.rs, string_fk.rs,
  temporal_store.rs, clustering.rs, learn.rs)

## [0.1.0] — 2026-05-02

Initial release with core synthetic data generation capabilities.

### Core

- **Declarative blueprint language** — `.knit.toml` and `.weave.json` formats
  with `extends` inheritance
- **Schema validation** — Type compatibility checks, FK/PK constraint
  verification, generator parameter validation
- **Execution planner** — Topological ordering, RNG tree seeding, batch
  partitioning with parallelism
- **Deterministic generation** — Seeded RNG tree ensures reproducible output
  across runs

### Generators

- Sequence, UUID, pattern, one-of (weighted), distribution (normal, uniform,
  log-normal, Pareto, Zipf, beta, exponential, Poisson, Bernoulli, binomial),
  faker (20+ methods), dictionary (sample/combinatorial/suffix expansion),
  derived expressions, temporal (business hours, relative), correlation,
  conditional, graph topology (scale-free, small-world, Erdős–Rényi), unique
  wrapper, constant

### Output

- **Formats** — Parquet, CSV, JSON, JSONL, Arrow IPC
- **Compression** — Snappy, LZ4, Zstd
- **Template engine** — MiniJinja for custom output formats

### Noise Pipeline

- 7 built-in perturbators: Gaussian noise, null injection, typo injection,
  outlier injection, value drift, field swap/truncation, format corruption
- Three-stage pipeline (clean → constrained → breaking)
- Per-perturbator rate configuration

### Reverse Engineering

- `knit learn` — Ingest CSV/JSON/Parquet, profile distributions, fit blueprints
- Incremental learning with streaming statistics and persistent state files
- Dictionary extraction from high-cardinality string columns
- Progress feedback during ingestion

### CLI

- Commands: `validate`, `plan`, `generate`, `blueprint` (expand/normalize/diff/doc),
  `init`, `learn`, `inspect`, `generators`, `completions`
- Global flags: `--seed`, `--format`, `--compression`, `--parallel`,
  `--batch-size`, `--count`, `--param`, `--json`, `--dry-run`, `--no-noise`,
  `--quiet`, `--verbose`
- Shell completion generation (bash, zsh, fish, PowerShell)

### Architecture

- 8-crate workspace: knit-core, knit-blueprint, knit-plan, knit-gen, knit-noise,
  knit-bind, knit-learn, knit-cli
- Batch-oriented Arrow columnar engine with Rayon parallelism
- Sampled key stores for 100M+ row entities
- Plugin architecture for custom generators
