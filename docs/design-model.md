# Structured Knit Model — Design Document

**Version:** 0.1.0
**Status:** Draft

---

## Table of Contents

- [1. Motivation](#1-motivation)
- [2. Model Directory Layout](#2-model-directory-layout)
- [3. Root Manifest (`knit.toml`)](#3-root-manifest-knittoml)
- [4. Layout Model (`layout.toml`)](#4-layout-model-layouttoml)
- [5. Table Models (`tables/*.toml`)](#5-table-models-tablestoml)
- [6. Relationship Model (`relationships.toml`)](#6-relationship-model-relationshipstoml)
- [7. Correlation Model (`correlations.toml`)](#7-correlation-model-correlationstoml)
- [8. Shared Definitions (`shared.toml`)](#8-shared-definitions-sharedtoml)
- [9. Companion Files](#9-companion-files)
- [10. Design Principles](#10-design-principles)
- [11. Migration & Compatibility](#11-migration--compatibility)
- [12. How Other Features Benefit](#12-how-other-features-benefit)
- [13. Implementation Plan](#13-implementation-plan)
- [14. Implementation Status](#14-implementation-status)

---

## 1. Motivation

Today the knit model is a single flat TOML file (`schema.weave.toml`). For a
real-world dataset with 48 tables and hundreds of columns, this file grows to
**12,000+ lines** (160 KB). Problems include:

| Problem | Impact |
|---------|--------|
| **Single-file monolith** | Hard to navigate; one entity change requires scrolling past thousands of unrelated lines |
| **Mixed concerns** | Schema, statistics, output layout, relationships, and noise all interleaved |
| **Difficult to diff** | A single-column tweak shows as a small edit in a massive file, obscuring meaningful changes in version control |
| **Not modular** | Cannot share or version individual table models independently |
| **Editing friction** | Both humans and AI tools struggle with very long context; 12K-line TOML is impractical to edit by hand |
| **No summary layer** | No quick way to get an overview (how many tables? which are connected?) without parsing everything |

The structured model splits the flat file into a **directory of focused,
single-concern files** — each small enough to read, edit, diff, and reason
about individually.

---

## 2. Model Directory Layout

```
my_model/
├── knit.toml                    # Root manifest (identity, seed, global config)
├── layout.toml                  # Physical output structure (folders, partitions, column order)
├── tables/                      # One file per table/entity
│   ├── AnalyzedUser.toml
│   ├── Collab.toml
│   └── PeopleHistorical.toml
├── relationships.toml           # Foreign keys, associations, graph edges
├── correlations.toml            # Cross-field and cross-table statistical correlations
├── shared.toml                  # Custom types, mixins, personas (optional)
├── dictionaries/                # Dictionary CSV/JSON files referenced by generators
│   ├── Region.csv
│   └── SignalType.csv
└── companions/                  # Non-data files to copy through (schemas, configs)
    ├── AnalyzedUser/
    │   └── Schema/
    │       └── schema.json
    └── Mappings/
        └── ActOnInsights-SurfaceType.csv
```

**Conventions:**

- The directory name IS the model name (e.g., `my_model/`)
- `knit.toml` at the directory root identifies it as a knit model
- Table file names match the entity name: `tables/{EntityName}.toml`
- All paths within model files are relative to the model root
- The entire directory is self-contained and portable (zip, git, share)

---

## 3. Root Manifest (`knit.toml`)

The manifest is the entry point. It identifies the model, sets global defaults,
and optionally lists user-defined parameters.

```toml
# knit.toml — Root manifest for the structured model
schema_version = "2.0"

[model]
name = "tc_multiple_weeks"
description = "Collaboration and people data across multiple weeks"
seed = 42
locale = "en_US"
timezone = "UTC"

# User-defined parameters (accessible in generators via $param.key)
[model.params]
start_date = "2024-10-13"
scale_factor = 1.0

# Model metadata (written by knit, informational)
[model.metadata]
created_by = "knit learn 0.4.0"
source = "tc_multiple_weeks/Encoded"
learned_at = "2025-05-10T12:00:00Z"
table_count = 3
total_columns = 185
total_rows = 77
```

**Design notes:**

- `schema_version = "2.0"` distinguishes structured from flat format
- `metadata` section is informational only — not used during generation
- The manifest is ~20 lines, always fits on one screen

---

## 4. Layout Model (`layout.toml`)

Physical output structure: folder hierarchy, file-to-table mapping, partition
strategy, column ordering, and companion file inventory.

```toml
# layout.toml — Physical output structure

# Folder hierarchy (mirrors source dataset structure)
# Each entry maps a table to its output path and partitioning strategy
[[folders]]
table = "AnalyzedUser"
path = "AnalyzedUser"                     # Output folder relative to root
format = "parquet"                        # csv | parquet | json

[[folders]]
table = "Collab"
path = "Collab"
format = "parquet"

[folders.partition]
by = "PartitionDate"                      # Column to partition on
values = [                                # Learned partition values
    "2024-10-13", "2024-10-15", "2024-10-20",
    "2024-10-27", "2024-11-03", "2024-11-10",
    "2024-11-17", "2024-11-24", "2024-12-01",
    "2024-12-08", "2024-12-15", "2024-12-22",
    "2024-12-24"
]
cadence = "weekly"                        # Detected time cadence (informational)
counts = [13, 5, 4, 4, 4, 3, 4, 4, 3, 3, 3, 7, 3]  # Rows per partition

[[folders]]
table = "PeopleHistorical"
path = "PeopleHistorical"
format = "parquet"

[folders.partition]
by = "PartitionDate"
values = ["2024-10-13", "2024-10-15", "2024-10-20",
          "2024-10-27", "2024-11-03", "2024-11-10",
          "2024-11-17"]
cadence = "weekly"
counts = [1, 1, 1, 1, 1, 1, 1]

# Column ordering — preserves source column order for each table
[column_order]
AnalyzedUser = [
    "ObjectId", "IsLicensed", "Region", "HireDate"
]
Collab = [
    "ObjectId", "ActorId", "PartitionDate", "Duration",
    "ParticipantCount", "Subject", "SignalType"
    # ... full list in actual file
]
PeopleHistorical = [
    "ObjectId", "AnalyzedUserId", "PartitionDate",
    "PersonId", "DisplayName", "EmailAddress"
]

# Companion files — non-data files to copy through verbatim
[companions]
files = [
    "companions/AnalyzedUser/Schema/schema.json",
    "companions/Collab/Schema/schema.json",
    "companions/Mappings/ActOnInsights-SurfaceType.csv"
]
```

**Design notes:**

- `layout.toml` separates *where* data goes from *what* data contains
- Column order is presentation-only — doesn't affect generation logic
- Partition values + counts enable `knit scale --time` to extend intelligently
- `cadence` is informational; stored for the scale command to use

---

## 5. Table Models (`tables/*.toml`)

Each table gets its own file with three clear sections: **schema** (what columns
exist), **generators** (how to produce values), and **statistics** (what was
observed in source data).

```toml
# tables/Collab.toml — Table model for Collab entity

[table]
name = "Collab"
description = "Collaboration activity signals (meetings, emails, chats)"
count = 60
tags = ["activity", "temporal"]
actor = false

# ─────────────────────────────────────────────────
# COLUMNS — Schema + Generator + Statistics per field
# ─────────────────────────────────────────────────

[[columns]]
name = "ObjectId"
type = "int64"
nullable = false
primary_key = true

[columns.generator]
type = "sequence"
start = 1
step = 1

[columns.stats]
distinct_count = 60
null_rate = 0.0
min = 1
max = 60

# ─────────────────────────────────────────────────

[[columns]]
name = "ActorId"
type = "int64"
nullable = false

[columns.generator]
type = "foreign_key"
references = "AnalyzedUser.ObjectId"

[columns.stats]
distinct_count = 8
null_rate = 0.0
min = 1
max = 8

# ─────────────────────────────────────────────────

[[columns]]
name = "Duration"
type = "float64"
nullable = true

[columns.generator]
type = "normal"
mean = 45.0
std = 12.3

[columns.stats]
distinct_count = 58
null_rate = 0.02
min = 5.0
max = 120.0
mean = 45.2
std = 12.1
percentiles = { p25 = 36.0, p50 = 44.5, p75 = 54.0, p95 = 68.0, p99 = 95.0 }

# ─────────────────────────────────────────────────

[[columns]]
name = "SignalType"
type = "string"
nullable = false

[columns.generator]
type = "one_of"
choices = [
    { value = "Meeting", weight = 0.45 },
    { value = "Email", weight = 0.30 },
    { value = "Chat", weight = 0.15 },
    { value = "Call", weight = 0.10 }
]

[columns.stats]
distinct_count = 4
null_rate = 0.0
top_values = [
    { value = "Meeting", frequency = 0.45 },
    { value = "Email", frequency = 0.30 },
    { value = "Chat", frequency = 0.15 },
    { value = "Call", frequency = 0.10 }
]
value_entropy = 1.74

# ─────────────────────────────────────────────────

[[columns]]
name = "PartitionDate"
type = "date"
nullable = false

[columns.generator]
type = "partition_value"

[columns.stats]
distinct_count = 13
null_rate = 0.0
min = "2024-10-13"
max = "2024-12-24"
```

### 5.1 Table-Level Statistics (optional section)

```toml
# Aggregate statistics about the table itself
[table.stats]
total_rows = 60
rows_per_actor = { mean = 7.5, min = 4, max = 12 }
rows_per_partition = { mean = 4.6, min = 3, max = 13 }
```

### 5.2 Conditional Generators

Conditional logic (e.g., "Duration depends on SignalType") stays in the table
file because it's table-internal:

```toml
[[columns]]
name = "Duration"
type = "float64"
nullable = true

[columns.generator]
type = "conditional"
field = "SignalType"

[[columns.generator.branches]]
when = "Meeting"
generator = { type = "normal", mean = 45.0, std = 12.0 }

[[columns.generator.branches]]
when = "Email"
generator = { type = "constant", value = 0 }

[columns.generator.default]
type = "normal"
mean = 30.0
std = 10.0
```

### 5.3 Column-Level Traits (for `knit enrich`)

Each column can carry trait annotations — lightweight qualitative descriptors
that help knit make better decisions during enrichment and scaling:

```toml
[[columns]]
name = "EmailAddress"
type = "string"
nullable = false

[columns.traits]
semantic = "email"          # Detected semantic type
pii = true                  # Contains personally identifiable information
cardinality = "high"        # low | medium | high | unique
trend = "stable"            # stable | increasing | decreasing | seasonal | cyclic
distribution_shape = "uniform"  # uniform | normal | skewed | bimodal | long_tail
```

---

## 6. Relationship Model (`relationships.toml`)

All cross-table connections live in one file. This is the **graph layer** of the
model — FK constraints, many-to-many associations, self-referential hierarchies,
and actor relationship graphs.

```toml
# relationships.toml — Cross-table connections

# ─────────────────────────────────────────────────
# FOREIGN KEYS
# ─────────────────────────────────────────────────

[[foreign_keys]]
name = "Collab_to_AnalyzedUser"
from = "Collab.ActorId"
to = "AnalyzedUser.ObjectId"
kind = "many_to_one"
nullable = false

[foreign_keys.cardinality]
type = "range"
min = 4
max = 12

[[foreign_keys]]
name = "PeopleHistorical_to_AnalyzedUser"
from = "PeopleHistorical.AnalyzedUserId"
to = "AnalyzedUser.ObjectId"
kind = "many_to_one"
nullable = false

# ─────────────────────────────────────────────────
# SELF-REFERENTIAL HIERARCHIES
# ─────────────────────────────────────────────────

[[hierarchies]]
name = "OrgChart"
entity = "PeopleHistorical"
parent_field = "ManagerId"
child_field = "PersonId"
root_probability = 0.1
max_depth = 5
acyclic = true

# ─────────────────────────────────────────────────
# ACTOR RELATIONSHIP GRAPHS (social networks)
# ─────────────────────────────────────────────────

[[actor_graphs]]
name = "CollaborationNetwork"
from_entity = "AnalyzedUser"
to_entity = "AnalyzedUser"
graph_type = "small_world"

[actor_graphs.params]
k = 4.0
beta = 0.3

# ─────────────────────────────────────────────────
# INFERRED GRAPH SUMMARY (informational)
# ─────────────────────────────────────────────────

[graph_summary]
total_tables = 3
total_relationships = 2
root_entities = ["AnalyzedUser"]         # Entities with no inbound FKs
leaf_entities = ["Collab"]               # Entities with no outbound FKs
dependency_order = [                     # Topological generation order
    "AnalyzedUser",
    "PeopleHistorical",
    "Collab"
]
```

**Design notes:**

- Splitting relationships from table definitions makes the graph structure
  explicit and scannable — you can see all connections in one place
- `graph_summary` is informational metadata that helps both humans and AI
  quickly understand the data flow without parsing table files
- `dependency_order` shows the generation order (topological sort of FK graph)

---

## 7. Correlation Model (`correlations.toml`)

Statistical correlations between fields. Separated because correlations are
*cross-cutting* — they span fields within a table or across tables.

```toml
# correlations.toml — Statistical correlations

# ─────────────────────────────────────────────────
# INTRA-TABLE CORRELATIONS
# ─────────────────────────────────────────────────

[[intra_table]]
table = "Collab"
fields = ["ParticipantCount", "Duration"]
matrix = [
    [1.0, 0.42],
    [0.42, 1.0]
]

[[intra_table]]
table = "Collab"
fields = ["ParticipantCount", "ParticipantIds"]
matrix = [
    [1.0, 0.93],
    [0.93, 1.0]
]

# ─────────────────────────────────────────────────
# CONDITIONAL DISTRIBUTIONS
# ─────────────────────────────────────────────────

[[conditional]]
table = "Collab"
dependent = "Duration"
given = "SignalType"

[[conditional.distributions]]
when = "Meeting"
generator = { type = "normal", mean = 45.0, std = 12.0 }

[[conditional.distributions]]
when = "Email"
generator = { type = "constant", value = 0 }

[conditional.default]
type = "normal"
mean = 30.0
std = 10.0

# ─────────────────────────────────────────────────
# CROSS-TABLE CORRELATIONS (future — for knit enrich)
# ─────────────────────────────────────────────────

# [[cross_table]]
# fields = ["Collab.Duration", "PeopleHistorical.Tenure"]
# correlation = 0.15
# confidence = 0.6
# source = "enrichment"
```

---

## 8. Shared Definitions (`shared.toml`)

Reusable definitions that span multiple tables: custom types, mixins, and
persona profiles.

```toml
# shared.toml — Shared definitions

# ─────────────────────────────────────────────────
# CUSTOM TYPES
# ─────────────────────────────────────────────────

[[types]]
name = "EmailAddress"
base = "string"
generator = { type = "faker", method = "safe_email" }

[[types]]
name = "EmployeeId"
base = "int64"
generator = { type = "sequence", start = 10000, step = 1 }

# ─────────────────────────────────────────────────
# MIXINS (reusable field groups)
# ─────────────────────────────────────────────────

[[mixins]]
name = "audit_fields"
description = "Standard audit trail columns"

[[mixins.fields]]
name = "CreatedAt"
type = "datetime"
nullable = false
generator = { type = "now" }

[[mixins.fields]]
name = "UpdatedAt"
type = "datetime"
nullable = true
generator = { type = "now" }

# ─────────────────────────────────────────────────
# PERSONAS (behavioral profiles for actor entities)
# ─────────────────────────────────────────────────

[[personas]]
name = "power_user"
weight = 0.2

[personas.traits]
activity_rate = 2.5
meeting_preference = 0.7
response_time_hours = 0.5

[[personas]]
name = "casual_user"
weight = 0.6

[personas.traits]
activity_rate = 0.8
meeting_preference = 0.3
response_time_hours = 4.0
```

---

## 9. Companion Files

Non-data files (schemas, dictionaries, mappings, configs) are stored under
`companions/` in the model directory, preserving their relative path structure
from the source dataset.

During `knit learn`, companion files are:
1. Identified (any non-data file in the source)
2. Copied to `companions/` preserving subfolder structure
3. Registered in `layout.toml` under `[companions]`

During `knit generate`, companion files are:
1. Read from `layout.toml`
2. Copied from `companions/` to the output directory

**Dictionary files** referenced by generators (e.g., `type = "dictionary"`,
`file = "Region.csv"`) are stored under `dictionaries/` and are distinct from
companion files — dictionaries are *consumed* by generators, while companions
are *passed through* verbatim.

---

## 10. Design Principles

### 10.1 Separation of Concerns

| File | Concern | Question it answers |
|------|---------|-------------------|
| `knit.toml` | Identity & config | *What is this model?* |
| `layout.toml` | Physical structure | *Where does the data go?* |
| `tables/*.toml` | Column-level model | *What does this table contain?* |
| `relationships.toml` | Graph structure | *How are tables connected?* |
| `correlations.toml` | Statistical dependencies | *Which fields co-vary?* |
| `shared.toml` | Reusable definitions | *What abstractions are shared?* |

### 10.2 Human Friendly

- Each file is **self-contained** — you can open one table file and understand
  it without reading anything else
- Table files are named after their entity (`Collab.toml` not `entity_3.toml`)
- TOML is chosen for readability (vs JSON/YAML) with clear section headers
- Inline comments explain non-obvious fields
- Statistics sections give immediate insight into data characteristics
- The largest file (a 166-column table) would be ~800 lines — vs 12,000 in the
  monolith

### 10.3 AI Friendly

- **Consistent structure**: Every table file follows the exact same pattern
  (table → columns → generator → stats → traits)
- **Focused context**: An AI editing a single table only needs to load that
  table's file (~200-800 lines) — not the full 12K-line monolith
- **Predictable paths**: `tables/{Name}.toml` — discoverable without an index
- **Machine-parseable metadata**: `graph_summary`, `metadata`, statistics
  sections provide structured data that AI can reason about programmatically
- **Section markers**: Clear TOML section headers act as semantic anchors

### 10.4 Incrementally Updateable

- `knit enrich` can update a single table file without touching others
- `knit scale` can modify `layout.toml` (partition values) and `knit.toml`
  (entity counts) without touching table schemas
- `knit tokenize` can process table files independently
- Version control shows per-table diffs cleanly

---

## 11. Migration & Compatibility

### 11.1 Format Detection

Knit detects the model format by the path argument:

| Input | Detection |
|-------|-----------|
| `path/to/schema.weave.toml` | Flat format (v1) — single file |
| `path/to/my_model/` | Structured format (v2) — directory with `knit.toml` |
| `path/to/my_model/knit.toml` | Structured format (v2) — explicit manifest |

### 11.2 Bidirectional Conversion

```bash
# Convert flat → structured
knit model convert schema.weave.toml -o my_model/

# Convert structured → flat (for compatibility or sharing as single file)
knit model flatten my_model/ -o schema.weave.toml
```

**Conversion rules (flat → structured):**

1. `[model]` → `knit.toml`
2. `companion_files` list → `layout.toml` `[companions]` + copy files to
   `companions/`
3. Each `[[entities]]` → `tables/{Name}.toml`
   - `output` section → moved to `layout.toml` `[[folders]]`
   - `fields` → `[[columns]]` (renamed for clarity)
4. `[[relationships]]` → `relationships.toml` `[[foreign_keys]]`
5. `[[correlations]]` → `correlations.toml`
6. `[[personas]]`, `[[types]]`, `[[mixins]]` → `shared.toml`
7. `[[noise]]`, `[[actor_relationships]]` → `relationships.toml`

### 11.3 Full Backward Compatibility

- `knit generate schema.weave.toml` continues to work unchanged
- `knit generate my_model/` works with the new structured format
- `knit learn` gains a `--format structured` flag (default remains flat for now)
- Internal `DataModel` struct is unchanged — both formats deserialize to the
  same in-memory representation

---

## 12. How Other Features Benefit

### 12.1 `knit scale`

- Reads `layout.toml` for partition values, cadence, and per-partition counts
- Modifies `layout.toml` to extend partition values for `--time`
- Updates `knit.toml` entity counts for `--actors`
- Writes a new model directory with scaled parameters (non-destructive)

### 12.2 `knit enrich`

- Opens only the relevant `tables/*.toml` files to update
- Adds/updates `[columns.stats]` and `[columns.traits]` sections
- Merges correlation data into `correlations.toml`
- Tracks enrichment history in `knit.toml` metadata

### 12.3 `knit tokenize`

- Processes each `tables/*.toml` independently to build token dictionary
- Replaces string values in generators (one_of choices, constants, patterns)
- Updates dictionary files in `dictionaries/`
- Leaves structure, relationships, and correlations untouched

### 12.4 Version Control & Collaboration

- Per-table files enable clean diffs in PRs
- Multiple team members can edit different tables without merge conflicts
- Model evolution is tracked at the right granularity

---

## 13. Implementation Plan

### Phase 1: Model Directory Reader

- Add `ModelDirectory` struct that can load the structured format
- Implement `ModelDirectory::to_data_model()` → existing `DataModel`
- Format detection in CLI (file vs directory path)
- All existing commands (`generate`, `validate`) work with either format

### Phase 2: Model Directory Writer

- Add `DataModel::to_model_directory()` conversion
- `knit learn --format structured` writes directory format
- `knit model convert` command for flat ↔ structured conversion

### Phase 3: Incremental Operations

- `knit model info my_model/` — summary of model contents
- Direct table-level operations (update single table without rewriting all)
- Integration with `knit enrich`, `knit scale`, `knit tokenize`

### Phase 4: Statistics Layer

- Extend `knit learn` to populate `[columns.stats]` sections
- Auto-detect traits (`semantic`, `cardinality`, `trend`, `distribution_shape`)
- Summary statistics in `[table.stats]`

---

## 14. Implementation Status

**Status:** Phase 1–2 complete (PR #230)

### Completed

| Component | Location | Notes |
|-----------|----------|-------|
| Format detection | `src/model/mod.rs` | Auto-detects structured directory vs flat file |
| Directory reader | `src/model/reader.rs` | Loads `knit.toml`, `layout.toml`, `tables/*.toml`, `relationships.toml`, `shared.toml` |
| Directory writer | `src/model/writer.rs` | Writes full directory structure from `DataModel` |
| `knit model convert` | `src/cli/commands/model.rs` | Bidirectional flat ↔ structured conversion |
| `knit model info` | `src/cli/commands/model.rs` | Summary display for either format |
| Partition weights | reader + writer | Roundtrip-safe via `weights` field |
| Mixin references | reader + writer | Preserved through `mixins` field |
| `load_schema` integration | `src/cli/commands/mod.rs` | All commands auto-detect format |
| `save_schema` helper | `src/cli/commands/mod.rs` | Format-aware write (structured or flat TOML) |
| `knit enrich` integration | `src/cli/commands/enrich.rs` | Preserves structured format on output |

### Remaining (Phase 3–4)

- Direct table-level update operations (edit single table without full rewrite)
- Statistics layer (`[columns.stats]`, `[table.stats]`)
- Auto-detected traits (semantic, cardinality, trend, distribution shape)

---

## Appendix A: Complete File Size Comparison

For the `tc_multiple_weeks` dataset (3 tables, 185 columns, 77 rows):

| Format | Files | Largest File | Total Size |
|--------|-------|-------------|------------|
| **Flat** (current) | 1 | 11,905 lines (160 KB) | 160 KB |
| **Structured** (proposed) | 7+ | ~800 lines (~12 KB) | ~25 KB + companions |

The largest single file in structured format would be `tables/Collab.toml`
(166 columns × ~5 lines each ≈ 830 lines).

## Appendix B: Quick Reference — Where Does Each DataModel Field Go?

| DataModel field | Structured location | Section |
|----------------|--------------------|---------| 
| `name`, `description`, `seed`, `locale`, `timezone` | `knit.toml` | `[model]` |
| `schema_version` | `knit.toml` | top-level |
| `params` | `knit.toml` | `[model.params]` |
| `entities[].name`, `count`, `tags`, `actor` | `tables/{Name}.toml` | `[table]` |
| `entities[].fields[]` | `tables/{Name}.toml` | `[[columns]]` |
| `entities[].output` | `layout.toml` | `[[folders]]` |
| `entities[].constraints` | `tables/{Name}.toml` | `[[constraints]]` |
| `relationships[]` | `relationships.toml` | `[[foreign_keys]]` |
| `correlations[]` | `correlations.toml` | `[[intra_table]]` or `[[conditional]]` |
| `noise_profiles[]` | `tables/{Name}.toml` | `[noise]` (table-scoped) |
| `personas[]` | `shared.toml` | `[[personas]]` |
| `actor_relationships[]` | `relationships.toml` | `[[actor_graphs]]` |
| `custom_types[]` | `shared.toml` | `[[types]]` |
| `mixins[]` | `shared.toml` | `[[mixins]]` |
| `companion_files[]` | `layout.toml` | `[companions]` |
