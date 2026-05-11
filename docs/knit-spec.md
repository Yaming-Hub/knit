# knit blueprint Language Specification

**Version:** 0.1.0
**Status:** Draft
**Project:** Knit — High-Performance Synthetic Data Generation Toolset

---

## 1. Introduction

### 1.1 What is Weave?

Weave is a declarative blueprint language for specifying synthetic datasets. A Weave
document describes the shape, statistical properties, relationships, and quality
characteristics of data to be generated. The Knit engine reads a Weave document and
produces datasets at arbitrary scale.

### 1.2 Design Goals

| Goal | Rationale |
|------|-----------|
| **AI-friendly** | LLMs can reliably read, generate, and modify Weave documents. One canonical way to express each concept. |
| **Statistically expressive** | First-class support for probability distributions, correlations, and temporal patterns. |
| **Relationally complete** | Multi-table blueprints with foreign keys, cardinality distributions, and cyclic references. |
| **Extensible** | Custom types, custom generators, parameterization, and plugin hooks. |
| **Format-agnostic** | Weave describes *data*, not *output format*. Output binding is a separate concern. |
| **High-performance** | Language constructs map to efficient columnar generation (100GB+ in hours). |

### 1.3 Serialization Format

Weave documents are serialized as **TOML** (primary) or **JSON** (for programmatic / AI
pipelines). Both formats parse into the same abstract model. The specification uses TOML
in examples, but every construct has an equivalent JSON representation.

**TOML subset restrictions** (for AI reliability):
- No dotted keys (use `[section.subsection]` form)
- No inline tables except for single-line generator specs
- Deterministic key ordering within sections
- UTF-8 encoding required

### 1.4 Terminology

| Term | Definition |
|------|-----------|
| **Document** | A complete Weave file describing a dataset |
| **Entity** | A logical table or collection (e.g., "users", "orders") |
| **Field** | A named column within an entity |
| **Generator** | A rule for producing values for a field |
| **Distribution** | A statistical probability distribution |
| **Relationship** | A foreign-key link between entities |
| **Perturbation** | A post-generation transformation that introduces noise or anomalies |
| **Type** | A reusable domain type definition |
| **Mixin** | A reusable group of fields that can be included in entities |
| **Param** | A user-supplied variable that can be referenced in the document |

---

## 2. Document Structure

A Weave document has the following top-level sections, all optional except `[model]`:

```toml
weave_version = "0.1"

[model]            # Required: dataset metadata
[params]           # Optional: user-configurable parameters
[[types]]          # Optional: reusable domain type definitions
[[mixins]]         # Optional: reusable field groups
[[entities]]       # Required: entity (table) definitions
[[relationships]]  # Optional: inter-entity relationships
[[correlations]]   # Optional: cross-field correlation specifications
[[noise]]          # Optional: perturbation profiles
```

### 2.1 Version

```toml
weave_version = "0.1"
```

Every document must declare the Weave version it conforms to. The engine rejects
documents with unsupported versions.

### 2.2 Model Metadata

```toml
[model]
name = "ecommerce"                           # Required: dataset identifier
description = "E-commerce platform dataset"  # Optional: human/AI-readable purpose
seed = 42                                    # Optional: global RNG seed (default: random)
locale = "en_US"                             # Optional: default locale for faker generators
timezone = "UTC"                             # Optional: default timezone for datetimetz fields (default: "UTC")
```

All string properties support Unicode. The `description` field is semantic metadata — it
is ignored by the engine but preserved for AI-driven workflows.

The `timezone` field sets the default timezone for all `datetimetz` fields that do not
specify their own timezone. Accepts IANA timezone names (`"America/New_York"`,
`"Europe/London"`, `"Asia/Tokyo"`) or fixed UTC offsets (`"+05:30"`, `"-08:00"`).

---

## 3. Parameters

Parameters make Weave documents configurable. They act as compile-time constants that
can be referenced anywhere a value is expected.

```toml
[params]
scale = { type = "float", default = 1.0, description = "Global scale multiplier" }
user_count = { type = "int", default = 100_000, description = "Number of users" }
start_date = { type = "string", default = "2020-01-01", description = "Dataset start date" }
enable_fraud = { type = "bool", default = true, description = "Include fraud scenarios" }
```

### 3.1 Parameter Types

Parameters support: `int`, `float`, `string`, `bool`, `date`, `datetime`, `duration`.

```toml
[params]
start_date = { type = "date", default = "2020-01-01", description = "Dataset start date" }
end_date = { type = "date", default = "2025-12-31", description = "Dataset end date" }
interval = { type = "duration", default = "1h", description = "Sampling interval for time series" }
```

### 3.2 Referencing Parameters

Parameters are referenced using the `$param.` prefix inside expression strings:

```toml
count = { expr = "$param.user_count * $param.scale" }
```

Parameters are resolved **before** generation begins (compile-time substitution).
Unresolved parameter references are validation errors.

### 3.3 CLI Override

```bash
knit generate blueprint.toml --param scale=10.0 --param user_count=1000000
```

---

## 4. Type System

### 4.1 Primitive Types

| Type | Description | Rust Mapping | Arrow Type |
|------|-------------|-------------|------------|
| `bool` | Boolean | `bool` | `BooleanArray` |
| `int` | 64-bit signed integer | `i64` | `Int64Array` |
| `float` | 64-bit IEEE float | `f64` | `Float64Array` |
| `string` | UTF-8 string | `String` | `StringArray` |
| `date` | Calendar date (YYYY-MM-DD) | `NaiveDate` | `Date32Array` |
| `time` | Time of day (HH:MM:SS.ffffff) | `NaiveTime` | `Time64MicrosecondArray` |
| `datetime` | Naive date + time (no timezone) | `NaiveDateTime` | `TimestampMicrosecondArray` |
| `datetimetz` | Timezone-aware date + time | `DateTime<Tz>` | `TimestampMicrosecondArray` (with tz) |
| `duration` | Signed time span between two instants | `Duration` | `DurationMicrosecondArray` |
| `uuid` | UUID v4 | `Uuid` | `StringArray` (canonical form) |
| `bytes` | Binary data | `Vec<u8>` | `BinaryArray` |

#### Temporal Type Details

**`date`** — A calendar date without time or timezone. ISO 8601 format: `YYYY-MM-DD`.
```toml
type = "date"    # "2024-03-15"
```

**`time`** — A time of day without date or timezone. ISO 8601 format: `HH:MM:SS[.ffffff]`.
Microsecond precision.
```toml
type = "time"    # "14:30:00", "14:30:00.123456"
```

**`datetime`** — A naive (timezone-unaware) date and time. ISO 8601 format:
`YYYY-MM-DDTHH:MM:SS[.ffffff]`. Use this when timezone is irrelevant or when all data
is implicitly in one timezone.
```toml
type = "datetime"    # "2024-03-15T14:30:00"
```

**`datetimetz`** — A timezone-aware date and time. Stored as UTC internally;
the `timezone` property controls the display/output timezone. ISO 8601 format with
offset or IANA timezone name.
```toml
type = "datetimetz"
timezone = "America/New_York"    # IANA timezone name
# or
timezone = "+05:30"              # Fixed UTC offset
```

When `timezone` is omitted on a `datetimetz` field, the entity-level or model-level
default timezone is used (see §2.2).

**`duration`** — A signed time span. Represented as microseconds internally. Can be
negative. ISO 8601 duration format in literals: `P[n]Y[n]M[n]DT[n]H[n]M[n]S`, or
shorthand strings like `"30d"`, `"2h30m"`, `"500ms"`.
```toml
type = "duration"    # "P1DT12H" = 1 day 12 hours, or "36h", or "1d12h"
```

**Duration shorthand units:**

| Unit | Meaning | Example |
|------|---------|---------|
| `us` | Microseconds | `"500us"` |
| `ms` | Milliseconds | `"100ms"` |
| `s` | Seconds | `"30s"` |
| `m` | Minutes | `"15m"` |
| `h` | Hours | `"2h"` |
| `d` | Days | `"7d"` |
| `w` | Weeks | `"2w"` |

Compound: `"1d12h30m"` = 1 day, 12 hours, 30 minutes.

**Precision:** All temporal types support microsecond precision by default. The output
format may truncate (e.g., Parquet timestamp precision is configurable).

### 4.2 Complex Types

```toml
# Array of integers
type = "array<int>"

# Nested object (inline entity)
type = "object"

# Enum (string with restricted values — prefer one_of generator)
type = "string"
```

Arrays are generated using the `composite` generator (see §6.8). Nested objects are
defined with inline `[[entities.fields.fields]]` sub-fields.

### 4.3 Custom Domain Types

Custom types define reusable (type + generator + constraint) bundles:

```toml
[[types]]
name = "money"
description = "Monetary amount in USD"
base = "float"
generator = { type = "distribution", distribution = "log_normal", params = { mu = 4.0, sigma = 1.2 } }
constraints = { min = 0.0, precision = 2 }

[[types]]
name = "email_address"
description = "Valid email address format"
base = "string"
generator = { type = "faker", params = { category = "internet.email" } }

[[types]]
name = "us_phone"
description = "US phone number format"
base = "string"
generator = { type = "pattern", params = { format = "(###) ###-####" } }

[[types]]
name = "percentage"
description = "Value between 0 and 1"
base = "float"
generator = { type = "distribution", distribution = "beta", params = { alpha = 2.0, beta = 5.0 } }
constraints = { min = 0.0, max = 1.0 }
```

When a field uses a custom type, it inherits the generator and constraints. Field-level
properties override type-level defaults:

```toml
[[entities.fields]]
name = "price"
type = "money"                     # inherits log_normal generator + constraints
# Field-level overrides are allowed:
# generator = { ... }             # would replace the inherited generator
# constraints = { min = 1.0 }     # would override min
```

**Override precedence:** field > type > global defaults.

---

## 5. Entities

Entities are the core building blocks — each represents a logical table or collection.

```toml
[[entities]]
name = "user"                              # Required: unique identifier
description = "Platform users"             # Optional: semantic annotation
tags = ["pii", "core"]                     # Optional: semantic tags
count = 100_000                            # Required: number of records
```

### 5.1 Count Specification

Count can be a fixed number, a range, or a parameter expression:

```toml
# Fixed count
count = 100_000

# Parameter-driven count
count = { expr = "$param.user_count * $param.scale" }

# Range (engine picks uniformly within range)
count = { min = 90_000, max = 110_000 }
```

### 5.2 Fields

Fields define the columns of an entity:

```toml
[[entities.fields]]
name = "id"                                # Required: field name
type = "uuid"                              # Required: data type (primitive or custom)
description = "Unique user identifier"     # Optional
primary_key = true                         # Optional: marks as PK (default: false)
unique = true                              # Optional: enforce uniqueness (default: false)
nullable = false                           # Optional: see §5.3 for full spec
generator = { type = "uuid" }              # Optional if type implies a generator
```

### 5.3 Nullability

Nullability controls how `NULL` values are injected:

```toml
# Never null (default)
nullable = false

# Always null
nullable = true

# Null with probability
nullable = { probability = 0.05 }

# Null every Nth record
nullable = { every_n = 100 }

# Null based on condition
nullable = { when = "tier == 'free'" }
```

### 5.4 Constraints

Entity-level constraints apply across fields:

```toml
[[entities]]
name = "order"
count = 500_000

# Composite unique constraint
[[entities.constraints]]
type = "unique"
fields = ["user_id", "order_number"]

# Check constraint (expression must evaluate to true)
[[entities.constraints]]
type = "check"
expr = "end_date >= start_date"

# Not-null constraint
[[entities.constraints]]
type = "not_null"
fields = ["id", "user_id"]
```

### 5.5 Nested Objects (Hierarchical Documents)

For document-oriented output (JSON, MongoDB-style), fields can contain nested objects:

```toml
[[entities]]
name = "product"
count = 10_000

[[entities.fields]]
name = "id"
type = "uuid"
primary_key = true

[[entities.fields]]
name = "name"
type = "string"
generator = { type = "faker", params = { category = "commerce.product_name" } }

[[entities.fields]]
name = "metadata"
type = "object"
description = "Nested product metadata"

[[entities.fields.fields]]
name = "weight_kg"
type = "float"
generator = { type = "distribution", distribution = "uniform", params = { min = 0.1, max = 50.0 } }

[[entities.fields.fields]]
name = "dimensions"
type = "object"

[[entities.fields.fields.fields]]
name = "length_cm"
type = "float"
generator = { type = "distribution", distribution = "normal", params = { mean = 30.0, std_dev = 10.0 }, min = 1.0 }

[[entities.fields.fields.fields]]
name = "width_cm"
type = "float"
generator = { type = "distribution", distribution = "normal", params = { mean = 20.0, std_dev = 8.0 }, min = 1.0 }
```

This generates hierarchical documents like:
```json
{
  "id": "a1b2c3...",
  "name": "Ergonomic Chair",
  "metadata": {
    "weight_kg": 12.3,
    "dimensions": { "length_cm": 45.2, "width_cm": 38.1 }
  }
}
```

### 5.6 Mixins

Mixins allow reusable field groups to be included in multiple entities:

```toml
[[mixins]]
name = "auditable"
description = "Standard audit trail fields"

[[mixins.fields]]
name = "created_at"
type = "datetimetz"
timezone = "UTC"
generator = { type = "distribution", distribution = "uniform", params = { min = "2020-01-01T00:00:00Z", max = "2025-12-31T23:59:59Z" } }

[[mixins.fields]]
name = "updated_at"
type = "datetimetz"
timezone = "UTC"
generator = { type = "relative", params = { anchor = "created_at", offset = { distribution = "log_normal", params = { mu = 10.0, sigma = 2.0 }, min = "1s", max = "365d", unit = "second" } } }

[[mixins.fields]]
name = "version"
type = "int"
generator = { type = "distribution", distribution = "geometric", params = { p = 0.7 } }
constraints = { min = 1 }
```

Usage in an entity:

```toml
[[entities]]
name = "order"
count = 500_000
mixins = ["auditable"]

[[entities.fields]]
name = "id"
type = "uuid"
primary_key = true
# ... auditable fields (created_at, updated_at, version) are auto-included
```

Mixin fields can be overridden by defining a field with the same name in the entity.

---

## 6. Generators

Generators define how values are produced. Every generator follows a uniform shape:

```toml
generator = { type = "<generator_type>", params = { ... } }
```

If a generator requires distribution parameters, they are specified inline. This
regularity enables AI to reliably produce valid generator specs.

### 6.1 Distribution Generator

Draw values from a statistical distribution. This is the most fundamental generator.

```toml
generator = {
    type = "distribution",
    distribution = "normal",
    params = { mean = 35.0, std_dev = 12.0 },
    min = 18,       # Optional: clamp/truncate minimum
    max = 99        # Optional: clamp/truncate maximum
}
```

#### Supported Distributions

| Distribution | Parameters | Domain | Typical Use |
|-------------|-----------|--------|-------------|
| `uniform` | `min`, `max` | continuous or discrete | IDs, dates, evenly spread values |
| `normal` | `mean`, `std_dev` | continuous | Ages, heights, measurements |
| `log_normal` | `mu`, `sigma` | positive continuous | Income, file sizes, prices |
| `exponential` | `lambda` | positive continuous | Inter-arrival times, durations |
| `poisson` | `lambda` | non-negative integer | Event counts, item quantities |
| `zipf` | `n`, `exponent` | positive integer | Popularity, word frequency |
| `bernoulli` | `p` | {0, 1} | Boolean flags, coin flips |
| `beta` | `alpha`, `beta` | [0, 1] | Probabilities, percentages |
| `gamma` | `shape`, `scale` | positive continuous | Wait times, claim sizes |
| `pareto` | `scale`, `shape` | ≥ scale | Wealth, city sizes, 80/20 data |
| `geometric` | `p` | positive integer | Retry counts, version numbers |
| `binomial` | `n`, `p` | {0..n} | Success counts in fixed trials |
| `weibull` | `shape`, `scale` | positive continuous | Reliability, lifetime data |
| `cauchy` | `median`, `scale` | continuous | Heavy-tailed noise |
| `chi_squared` | `df` | positive continuous | Statistical tests |
| `student_t` | `df` | continuous | Small-sample statistics |
| `dirichlet` | `alpha` (array) | simplex | Category proportions |
| `multinomial` | `n`, `p` (array) | integer vector | Multi-category counts |
| `custom` | `name`, arbitrary | varies | Plugin-defined distributions |

#### Date/Time Distributions

Distributions over temporal types use string parameters in ISO 8601 or shorthand format:

```toml
# Uniform dates
generator = {
    type = "distribution",
    distribution = "uniform",
    params = { min = "2020-01-01", max = "2025-12-31" }
}

# Uniform datetimes
generator = {
    type = "distribution",
    distribution = "uniform",
    params = { min = "2020-01-01T00:00:00", max = "2025-12-31T23:59:59" }
}

# Uniform times (time-of-day, e.g. business hours only)
generator = {
    type = "distribution",
    distribution = "uniform",
    params = { min = "09:00:00", max = "17:00:00" }
}

# Normal distribution around a center datetime
generator = {
    type = "distribution",
    distribution = "normal",
    params = { mean = "2024-06-15T12:00:00", std_dev = "30d" }
}

# Duration generation (e.g. session lengths)
generator = {
    type = "distribution",
    distribution = "log_normal",
    params = { mu = 5.5, sigma = 1.2 },    # in seconds
    min = "1s",
    max = "24h",
    unit = "second"                         # interpret distribution output as seconds
}

# Exponential inter-arrival times
generator = {
    type = "distribution",
    distribution = "exponential",
    params = { lambda = 0.5 },
    unit = "minute"                         # one event per ~2 minutes
}
```

**Temporal distribution rules:**
- When `min`/`max` are date/time strings, the distribution operates over the
  continuous range and snaps to the field's type precision.
- For `normal`/`log_normal` over datetimes, `mean` is a datetime string and
  `std_dev` is a duration string (e.g., `"7d"`, `"2h"`).
- The `unit` parameter specifies how numeric distribution output maps to duration
  (`"microsecond"`, `"millisecond"`, `"second"`, `"minute"`, `"hour"`, `"day"`).
  Default: `"second"`.

#### Timezone-Aware Generation

For `datetimetz` fields, the generator produces UTC timestamps internally. The field's
`timezone` property (or the model default) controls output formatting:

```toml
[[entities.fields]]
name = "event_time"
type = "datetimetz"
timezone = "America/Los_Angeles"
generator = {
    type = "distribution",
    distribution = "uniform",
    params = { min = "2024-01-01T00:00:00-08:00", max = "2024-12-31T23:59:59-08:00" }
}
```

When generator params include timezone offsets, they are normalized to UTC before
generation. Output is converted to the field's target timezone.

### 6.2 Faker Generator

Generate realistic structured data using locale-aware providers:

```toml
generator = { type = "faker", params = { category = "person.full_name" } }
generator = { type = "faker", params = { category = "internet.email" } }
generator = { type = "faker", params = { category = "address.city", locale = "de_DE" } }
```

#### Faker Categories

Faker categories follow a `provider.method` dotted notation:

| Provider | Methods | Examples |
|----------|---------|---------|
| `person` | `full_name`, `first_name`, `last_name`, `prefix`, `suffix` | "Jane Smith" |
| `internet` | `email`, `username`, `url`, `ipv4`, `ipv6`, `mac_address`, `user_agent` | "jane@example.com" |
| `address` | `street_address`, `city`, `state`, `country`, `zip_code`, `latitude`, `longitude` | "123 Main St" |
| `company` | `name`, `industry`, `catch_phrase`, `bs` | "Acme Corp" |
| `finance` | `credit_card`, `iban`, `bic`, `currency_code`, `bitcoin_address` | "4111..." |
| `phone` | `number`, `cell`, `country_code` | "+1-555-0100" |
| `lorem` | `word`, `sentence`, `paragraph`, `text` | "Lorem ipsum..." |
| `datetime` | `date`, `time`, `datetime`, `timezone`, `day_of_week`, `month` | "2024-03-15" |
| `color` | `hex`, `name`, `rgb` | "#FF5733" |
| `file` | `extension`, `mime_type`, `file_name`, `file_path` | "report.pdf" |
| `barcode` | `ean13`, `ean8`, `isbn10`, `isbn13` | "978-3-16..." |
| `geo` | `coordinate`, `latitude`, `longitude` | 40.7128 |
| `medical` | `blood_type`, `condition`, `drug` | "O+" |
| `vehicle` | `make`, `model`, `vin`, `plate` | "Toyota Camry" |

### 6.3 Sequence Generator

Deterministic sequences for IDs, counters, ordered values, and temporal progressions:

```toml
# Auto-increment integers
generator = { type = "sequence", params = { start = 1, step = 1 } }

# Cyclic sequence through a list
generator = { type = "sequence", params = { values = ["Mon", "Tue", "Wed", "Thu", "Fri"], cycle = true } }

# Date sequence (daily)
generator = { type = "sequence", params = { start = "2024-01-01", step = "1d" } }

# Datetime sequence (hourly)
generator = { type = "sequence", params = { start = "2024-01-01T00:00:00", step = "1h" } }

# Datetime sequence with jitter (±30 minutes random offset per step)
generator = { type = "sequence", params = { start = "2024-01-01T08:00:00", step = "1d", jitter = "30m" } }

# Time sequence (every 15 minutes within a day, cycling)
generator = { type = "sequence", params = { start = "00:00:00", step = "15m", cycle = true } }

# Timezone-aware datetime sequence
generator = { type = "sequence", params = { start = "2024-01-01T00:00:00-05:00", step = "6h", timezone = "America/New_York" } }
```

**Temporal step format:** Uses the same duration shorthand as the `duration` type
(`"1d"`, `"2h30m"`, `"500ms"`, etc.).

**Jitter:** When `jitter` is specified, each step adds a random offset drawn uniformly
from `[-jitter, +jitter]`. This creates realistic irregular-but-roughly-periodic
timestamps (e.g., daily logs that don't land at exactly midnight).

### 6.4 One-Of Generator (Weighted Choice)

Randomly select from a set of values with optional weights:

```toml
generator = { type = "one_of", params = { choices = [
    { value = "active",    weight = 0.70 },
    { value = "inactive",  weight = 0.20 },
    { value = "suspended", weight = 0.05 },
    { value = "banned",    weight = 0.05 },
] } }
```

If weights are omitted, uniform selection is used. Weights are automatically normalized.

### 6.5 Pattern Generator

Generate strings matching a pattern or regular expression:

```toml
# Format pattern (# = digit, ? = letter, * = alphanumeric)
generator = { type = "pattern", params = { format = "(###) ###-####" } }

# Regex pattern
generator = { type = "pattern", params = { regex = "[A-Z]{2}\\d{6}" } }

# Template with faker interpolation
generator = { type = "pattern", params = { template = "USR-{sequence:1:1}-{faker:address.state_abbr}" } }
```

### 6.6 Derived Generator (Expressions)

Compute a field's value from other fields in the same entity. Fields are referenced by
name and must be defined *before* the derived field (topological ordering).

```toml
[[entities.fields]]
name = "total"
type = "float"
generator = { type = "derived", params = { expr = "${quantity} * ${unit_price} * (1 - ${discount})" } }

[[entities.fields]]
name = "full_name"
type = "string"
generator = { type = "derived", params = { expr = "${first_name} + \" \" + ${last_name}" } }

[[entities.fields]]
name = "age_group"
type = "string"
generator = { type = "derived", params = { expr = "case(${age} < 18, \"minor\", ${age} < 65, \"adult\", \"senior\")" } }
```

#### Expression Language (Weave Expressions)

A small, deterministic expression language with explicit scope:

**Scope:** Fields in the same entity (by name), parameters (`$param.name`).

**Operators:** `+`, `-`, `*`, `/`, `%`, `==`, `!=`, `<`, `>`, `<=`, `>=`, `&&`, `||`, `!`

**Functions:**

| Function | Description | Example |
|----------|-------------|---------|
| **String** | | |
| `concat(a, b, ...)` | String concatenation | `concat(${first}, " ", ${last})` |
| `upper(s)` / `lower(s)` | Case conversion | `upper(${country_code})` |
| `len(s)` | String length (chars) | `len(${name})` |
| `substr(s, start, len)` | Substring | `substr(${code}, 0, 3)` |
| `left(s, n)` | First n characters | `left(${name}, 1)` |
| `right(s, n)` | Last n characters | `right(${phone}, 4)` |
| `pad_left(s, n, fill)` | Pad left to length n | `pad_left(${id}, 6, "0")` |
| `pad_right(s, n, fill)` | Pad right to length n | `pad_right(${name}, 20, " ")` |
| `starts_with(s, prefix)` | Starts-with predicate | `starts_with(${email}, "admin")` |
| `ends_with(s, suffix)` | Ends-with predicate | `ends_with(${file}, ".csv")` |
| `contains(s, needle)` | Contains predicate | `contains(${name}, "test")` |
| `replace(s, from, to)` | Replace substring | `replace(${name}, " ", "_")` |
| **Numeric** | | |
| `round(x, n)` | Round to n decimals | `round(${price}, 2)` |
| `floor(x)` / `ceil(x)` | Floor/ceiling | `floor(${age})` |
| `abs(x)` | Absolute value | `abs(${balance})` |
| `clamp(x, min, max)` | Clamp to range | `clamp(${score}, 0, 100)` |
| `min(a, b)` / `max(a, b)` | Minimum / maximum | `min(${stock}, ${demand})` |
| `sqrt(x)` | Square root (negative → null) | `sqrt(${variance})` |
| `pow(base, exp)` | Exponentiation | `pow(2, ${bits})` |
| `ln(x)` | Natural logarithm (x ≤ 0 → null) | `ln(${price})` |
| `log(x, base)` | Logarithm (domain checks) | `log(${value}, 10)` |
| `exp(x)` | Exponential (overflow → null) | `exp(${rate})` |
| **Conditional** | | |
| `if(cond, then, else)` | Conditional | `if(${age} >= 18, "adult", "minor")` |
| `case(cond, val, ...)` | Multi-branch conditional | `case(${x} > 0, "pos", ${x} < 0, "neg", "zero")` |
| `coalesce(a, b, ...)` | First non-null value | `coalesce(${nickname}, ${first_name})` |
| `nullif(a, b)` | Null if equal | `nullif(${value}, 0)` |
| **Type Cast** | | |
| `cast_int(x)` | Convert to integer | `cast_int(${price})` |
| `cast_float(x)` | Convert to float | `cast_float(${count})` |
| `cast_string(x)` | Convert to string | `cast_string(${id})` |
| **Utility** | | |
| `hash(s)` | Deterministic hash | `hash(${email})` |
| `row_number()` | Global row index (0-based) | `row_number()` |
| **Random** | | |
| `random_int(min, max)` | Random int in range (inclusive) | `random_int(1, 100)` |
| `random_float(min, max)` | Random float in range [min, max) | `random_float(0.0, 1.0)` |
| `random_duration(min, max)` | Random duration in ms range | `random_duration(0, 86400000)` |
| **Date / Time Construction** | | |
| `make_date(y, m, d)` | Construct a date from parts | `make_date(2024, 3, 15)` |
| `make_time(h, m, s)` | Construct a time from parts | `make_time(14, 30, 0)` |
| `make_datetime(y, M, d, h, m, s)` | Construct a datetime | `make_datetime(2024, 3, 15, 14, 30, 0)` |
| `make_duration(n, unit)` | Construct a duration | `make_duration(30, "day")` |
| `to_date(s, fmt)` | Parse string to date | `to_date("2024-03-15", "%Y-%m-%d")` |
| `to_datetime(s, fmt)` | Parse string to datetime | `to_datetime("2024-03-15 14:30", "%Y-%m-%d %H:%M")` |
| `epoch_seconds(dt)` | Datetime to Unix epoch seconds | `epoch_seconds(${created_at})` |
| `from_epoch(n)` | Unix epoch seconds to datetime | `from_epoch(1710500000)` |
| **Date / Time Extraction** | | |
| `year(d)` | Extract year | `year(${created_at})` → `2024` |
| `month(d)` | Extract month (1–12) | `month(${created_at})` → `3` |
| `day(d)` | Extract day of month (1–31) | `day(${created_at})` → `15` |
| `hour(dt)` | Extract hour (0–23) | `hour(${event_time})` → `14` |
| `minute(dt)` | Extract minute (0–59) | `minute(${event_time})` → `30` |
| `second(dt)` | Extract second (0–59) | `second(${event_time})` → `45` |
| `day_of_week(d)` | Day of week (0=Mon, 6=Sun) | `day_of_week(${order_date})` → `2` |
| `day_of_year(d)` | Day of year (1–366) | `day_of_year(${order_date})` → `75` |
| `week_of_year(d)` | ISO week number (1–53) | `week_of_year(${order_date})` → `11` |
| `quarter(d)` | Quarter (1–4) | `quarter(${order_date})` → `1` |
| **Date / Time Arithmetic** | | |
| `date_add(d, n, unit)` | Add to date/datetime | `date_add(${start_date}, 30, "day")` |
| `date_sub(d, n, unit)` | Subtract from date/datetime | `date_sub(${end_date}, 1, "month")` |
| `date_diff(d1, d2, unit)` | Difference between two dates | `date_diff(${end_date}, ${start_date}, "day")` |
| `duration_add(d, dur)` | Add a duration to date/datetime | `duration_add(${start}, ${processing_time})` |
| `start_of(d, unit)` | Truncate to start of unit | `start_of(${event_time}, "hour")` → `14:00:00` |
| `end_of(d, unit)` | End of unit boundary | `end_of(${order_date}, "month")` → last day |
| **Date / Time Formatting** | | |
| `format_date(d, fmt)` | Format date/datetime as string | `format_date(${created_at}, "%Y-%m")` |
| `format_duration(dur, style)` | Format duration as string | `format_duration(${elapsed}, "hms")` |
| **Timezone** | | |
| `to_timezone(dt, tz)` | Convert UTC millis to local wall-clock millis | `to_timezone(${event_time}, "Asia/Tokyo")` |
| `timezone_offset(dt, tz)` | Get UTC offset string for a moment in time | `timezone_offset(${event_time}, "America/New_York")` → `"-05:00"` |

**Date/time unit strings** (used in `date_add`, `date_diff`, `start_of`, etc.):
`"microsecond"`, `"millisecond"`, `"second"`, `"minute"`, `"hour"`, `"day"`,
`"week"`, `"month"`, `"quarter"`, `"year"`.

**Format strings** follow the `strftime` / `chrono` pattern:

| Token | Meaning | Example |
|-------|---------|---------|
| `%Y` | 4-digit year | `2024` |
| `%m` | Month (01–12) | `03` |
| `%d` | Day (01–31) | `15` |
| `%H` | Hour (00–23) | `14` |
| `%M` | Minute (00–59) | `30` |
| `%S` | Second (00–59) | `45` |
| `%f` | Microseconds (000000–999999) | `123456` |
| `%z` | UTC offset | `+0530` |
| `%Z` | Timezone abbreviation | `EST` |
| `%A` | Weekday name | `Friday` |
| `%B` | Month name | `March` |
| `%j` | Day of year (001–366) | `075` |
| `%U` | Week number (Sunday start) | `11` |
| `%W` | Week number (Monday start) | `11` |

**Restrictions:**
- No side effects, no I/O
- No cross-entity references (use relationships instead)
- No recursion
- Must form a DAG within the entity (cycle detection at validation time)

### 6.7 Conditional Generator

Choose a generator based on another field's value:

```toml
[[entities.fields]]
name = "discount_pct"
type = "float"
generator = { type = "conditional", params = {
    on = "tier",
    branches = [
        { when = "free",       then = { type = "constant", params = { value = 0.0 } } },
        { when = "basic",      then = { type = "distribution", distribution = "uniform", params = { min = 0.0, max = 0.05 } } },
        { when = "premium",    then = { type = "distribution", distribution = "uniform", params = { min = 0.05, max = 0.20 } } },
        { when = "enterprise", then = { type = "distribution", distribution = "uniform", params = { min = 0.15, max = 0.40 } } },
    ],
    default = { type = "constant", params = { value = 0.0 } },
} }
```

**Restrictions:**
- `on` must reference a field defined *before* this field
- Each `when` value must be a literal (no expressions)
- `default` is required

### 6.8 Composite Generator (Arrays)

Generate arrays with configurable length:

```toml
[[entities.fields]]
name = "tags"
type = "array<string>"
generator = { type = "composite", params = {
    element = { type = "one_of", params = { choices = [
        { value = "electronics" }, { value = "clothing" },
        { value = "food" }, { value = "books" },
    ] } },
    length = { distribution = "poisson", params = { lambda = 3.0 } },
    unique_elements = true,
} }
```

### 6.9 Lookup Generator (External Data Source)

Sample values from an external file:

```toml
[[entities.fields]]
name = "city"
type = "string"
generator = { type = "lookup", params = {
    source = "data/cities.csv",       # Relative path to data file
    column = "city_name",             # Column to sample from
    format = "csv",                   # csv, json, parquet
    sampling = "weighted",            # uniform, weighted, sequential
    weight_column = "population",     # For weighted sampling
} }
```

**Portability rules:**
- Paths must be relative to the blueprint file
- Source files must be included alongside the blueprint
- Supported formats: CSV, JSON (array), Parquet
- Missing file is a validation error
- Deterministic sampling (reproducible with same seed)

### 6.10 Constant Generator

```toml
generator = { type = "constant", params = { value = "USD" } }
generator = { type = "constant", params = { value = 0 } }
generator = { type = "constant", params = { value = true } }
```

### 6.11 UUID Generator

Shorthand — when a field has `type = "uuid"` and no explicit generator, a random UUID v4
generator is implied. Explicit form:

```toml
generator = { type = "uuid" }
generator = { type = "uuid", params = { version = 7 } }  # Time-ordered UUID v7
```

### 6.12 Unique Wrapper

Any generator can be wrapped to enforce uniqueness:

```toml
generator = { type = "unique", params = {
    inner = { type = "faker", params = { category = "internet.email" } },
    max_retries = 1000,
} }
```

The engine retries generation until a unique value is produced. If `max_retries` is
exhausted, generation fails with an error.

### 6.13 Temporal Generators

Specialized generators for temporal data patterns that go beyond simple distributions.

#### Relative Datetime Generator

Generate datetimes relative to another field or to the current record's context:

```toml
# Ship date is 1–14 days after order date
[[entities.fields]]
name = "ship_date"
type = "datetime"
generator = { type = "relative", params = {
    anchor = "order_date",
    offset = { distribution = "log_normal", params = { mu = 1.5, sigma = 0.8 }, min = "1d", max = "14d", unit = "day" },
} }

# Expiry is exactly 1 year after issue date
[[entities.fields]]
name = "expiry_date"
type = "date"
generator = { type = "relative", params = {
    anchor = "issue_date",
    offset = { type = "constant", value = "365d" },
} }
```

#### Business Hours Generator

Generate datetimes constrained to business hours, with timezone and holiday awareness:

```toml
[[entities.fields]]
name = "support_call_time"
type = "datetimetz"
timezone = "America/New_York"
generator = { type = "business_hours", params = {
    start_hour = 9,
    end_hour = 17,
    days = ["Mon", "Tue", "Wed", "Thu", "Fri"],
    date_range = { min = "2024-01-01", max = "2024-12-31" },
    # Optional: exclude specific dates
    exclude_dates = ["2024-12-25", "2024-01-01", "2024-07-04"],
} }
```

#### Duration Generator

Generate realistic durations for sessions, processing times, etc.:

```toml
# Session durations: mostly short, occasional long sessions
[[entities.fields]]
name = "session_duration"
type = "duration"
generator = { type = "distribution", distribution = "log_normal", params = { mu = 5.0, sigma = 1.5 }, min = "1s", max = "8h", unit = "second" }

# Processing time: bimodal (fast cache hit vs slow DB query)
[[entities.fields]]
name = "response_time"
type = "duration"
generator = { type = "one_of", params = { choices = [
    { value = { type = "distribution", distribution = "normal", params = { mean = 5.0, std_dev = 2.0 }, unit = "millisecond" }, weight = 0.8 },
    { value = { type = "distribution", distribution = "normal", params = { mean = 200.0, std_dev = 50.0 }, unit = "millisecond" }, weight = 0.2 },
] } }
```

#### Timezone Generator

Generate timezone values (for multi-region datasets):

```toml
[[entities.fields]]
name = "user_timezone"
type = "string"
generator = { type = "one_of", params = { choices = [
    { value = "America/New_York",    weight = 0.25 },
    { value = "America/Chicago",     weight = 0.15 },
    { value = "America/Denver",      weight = 0.08 },
    { value = "America/Los_Angeles", weight = 0.20 },
    { value = "Europe/London",       weight = 0.10 },
    { value = "Europe/Berlin",       weight = 0.07 },
    { value = "Asia/Tokyo",          weight = 0.08 },
    { value = "Asia/Shanghai",       weight = 0.07 },
] } }

# Timezone-aware event time that respects per-user timezone
[[entities.fields]]
name = "local_login_time"
type = "datetimetz"
generator = { type = "business_hours", params = {
    start_hour = 7,
    end_hour = 23,
    timezone_field = "user_timezone",    # Use per-row timezone from another field
    date_range = { min = "2024-01-01", max = "2024-12-31" },
} }
```

#### Temporal Window Generator

Generate pairs of start/end timestamps with controlled gap duration:

```toml
# Event with start and end time
[[entities.fields]]
name = "start_time"
type = "datetime"
generator = { type = "distribution", distribution = "uniform", params = { min = "2024-01-01T00:00:00", max = "2024-12-31T23:59:59" } }

[[entities.fields]]
name = "end_time"
type = "datetime"
generator = { type = "relative", params = {
    anchor = "start_time",
    offset = { distribution = "log_normal", params = { mu = 3.0, sigma = 1.0 }, min = "1m", max = "4h", unit = "minute" },
} }

# Derived duration field
[[entities.fields]]
name = "elapsed"
type = "duration"
generator = { type = "derived", params = { expr = "date_diff(end_time, start_time, 'second') |> make_duration('second')" } }
```

---

## 7. Relationships

Relationships define foreign-key links between entities. They control how entities
reference each other and how cardinality is distributed.

```toml
[[relationships]]
name = "order_user"                      # Required: unique relationship name
description = "Each order belongs to a user"
from = "order"                           # Child entity
to = "user"                              # Parent entity
kind = "many_to_one"                     # Relationship kind
from_field = "user_id"                   # FK field on child
to_field = "id"                          # PK field on parent
```

### 7.1 Relationship Kinds

| Kind | Semantics | FK Constraint |
|------|-----------|--------------|
| `many_to_one` | Many children → one parent | child.fk references parent.pk |
| `one_to_one` | One child → one parent | child.fk references parent.pk (unique) |
| `many_to_many` | Many ↔ many (via junction entity) | requires junction entity definition |

### 7.2 Cardinality Distribution

Control how children are distributed across parents:

```toml
[[relationships]]
name = "order_user"
from = "order"
to = "user"
kind = "many_to_one"
from_field = "user_id"
to_field = "id"

# Distribution of orders-per-user (degree distribution)
degree = { distribution = "zipf", params = { n = 100000, exponent = 1.2 } }
```

If `degree` is omitted, children are assigned to parents uniformly at random.

### 7.3 Target Selection Strategy

Control *how* a child picks its parent:

```toml
# Uniform random selection (default)
selection = "uniform"

# Weighted by a parent field
selection = { strategy = "weighted", weight_field = "popularity" }

# Clustered: children tend to reference recently generated parents
selection = { strategy = "clustered", cluster_size = 100 }

# Sequential: round-robin assignment
selection = "sequential"
```

### 7.4 Self-Referential Relationships

Entities can reference themselves (e.g., employee → manager):

```toml
[[relationships]]
name = "employee_manager"
from = "employee"
to = "employee"
kind = "many_to_one"
from_field = "manager_id"
to_field = "id"
nullable = true            # Top-level managers have null manager_id
acyclic = true             # No circular management chains
root_probability = 0.05    # 5% of employees are root nodes (null FK)
max_depth = 6              # Maximum hierarchy depth
```

### 7.5 Cyclic Relationships

When entities form mutual dependency cycles (A → B → A), the engine uses **two-phase
generation**:
1. Phase 1: Generate all records with primary keys; leave cyclic FK fields as NULL
2. Phase 2: Backpatch FK fields with valid references

Weave documents do not need special syntax for cycles — the engine detects them
automatically. However, cyclic FK fields **must** be nullable.

---

## 8. Correlations

Real-world data has correlated fields. Weave supports explicit correlation specifications
to go beyond independent marginal distributions.

### 8.1 Field-Pair Correlation

Specify Pearson correlation between two numeric fields in the same entity:

```toml
[[correlations]]
entity = "user"
fields = ["age", "income"]
coefficient = 0.6                  # Pearson correlation (-1 to 1)
method = "copula"                  # copula (default), rank, rejection
```

### 8.2 Correlation Matrix

Specify a full correlation matrix for a group of numeric fields:

```toml
[[correlations]]
entity = "user"
fields = ["age", "income", "credit_score"]
matrix = [
    [1.0,  0.6,  0.4],
    [0.6,  1.0,  0.7],
    [0.4,  0.7,  1.0],
]
method = "copula"
```

### 8.3 Conditional Distributions

Model how one field's distribution depends on another:

```toml
[[correlations]]
entity = "order"
type = "conditional_distribution"
dependent = "amount"
given = "category"
distributions = [
    { when = "electronics", distribution = "log_normal", params = { mu = 5.5, sigma = 1.0 } },
    { when = "books",       distribution = "log_normal", params = { mu = 2.5, sigma = 0.5 } },
    { when = "groceries",   distribution = "normal",     params = { mean = 50.0, std_dev = 20.0 } },
]
```

### 8.4 Copula-Based Joint Distributions

For advanced users, specify the copula family directly (inspired by SDV):

```toml
[[correlations]]
entity = "user"
type = "copula"
fields = ["age", "income", "spending_score"]
copula = "gaussian"                # gaussian, clayton, frank, gumbel
params = { }                      # Copula-specific parameters
```

---

## 9. Time Series & Temporal Patterns

For entities that represent time-indexed data (logs, metrics, events), Weave provides
temporal generators that model trends, seasonality, and autocorrelation.

### 9.1 Time Series Entity

```toml
[[entities]]
name = "server_metric"
description = "Time series of server CPU metrics"
count = 1_000_000
temporal = true                              # Marks as time-series entity

[[entities.fields]]
name = "timestamp"
type = "datetimetz"
timezone = "UTC"
generator = { type = "sequence", params = { start = "2024-01-01T00:00:00Z", step = "1m" } }

[[entities.fields]]
name = "cpu_usage"
type = "float"
generator = { type = "time_series", params = {
    baseline = 45.0,
    components = [
        { type = "trend",       params = { slope = 0.001 } },
        { type = "seasonality", params = { period = "24h", amplitude = 15.0 } },
        { type = "seasonality", params = { period = "7d",  amplitude = 5.0 } },
        { type = "noise",       params = { distribution = "normal", std_dev = 3.0 } },
        { type = "ar",          params = { coefficients = [0.7, 0.2] } },
    ],
    min = 0.0,
    max = 100.0,
} }
```

### 9.2 Time Series Components

| Component | Description | Parameters |
|-----------|-------------|------------|
| `trend` | Linear or polynomial trend | `slope`, `degree` |
| `seasonality` | Periodic pattern | `period` (duration string), `amplitude`, `phase` |
| `noise` | Random noise | `distribution`, distribution params |
| `ar` | Autoregressive component | `coefficients` (list of AR coefficients) |
| `spike` | Occasional spikes/anomalies | `probability`, `magnitude`, `duration` (duration string) |
| `level_shift` | Permanent level changes | `probability`, `magnitude` |
| `mean_reversion` | Mean-reverting behavior | `target`, `speed` |
| `weekend_effect` | Different behavior on weekends | `multiplier`, `shift` |
| `holiday_effect` | Spikes/dips on specific dates | `dates` (list), `magnitude` |
| `business_hours` | Day/night pattern | `active_hours` `[start, end]`, `active_multiplier` |

All duration-valued parameters (`period`, `duration`) accept the standard Weave duration
shorthand (`"24h"`, `"7d"`, `"15m"`, etc.).

#### Seasonality Details

Seasonality `period` maps directly to temporal concepts:

| Period | Pattern |
|--------|---------|
| `"1h"` | Hourly cycle (e.g., batch job patterns) |
| `"24h"` | Daily cycle (e.g., day/night traffic) |
| `"7d"` | Weekly cycle (e.g., weekday vs weekend) |
| `"30d"` or `"1M"` | Monthly cycle (e.g., billing cycles) |
| `"365d"` or `"1Y"` | Yearly cycle (e.g., seasonal retail) |

### 9.3 Event Streams

For irregular time series (events arriving at random intervals):

```toml
[[entities]]
name = "page_view"
count = 5_000_000
temporal = true

[[entities.fields]]
name = "timestamp"
type = "datetimetz"
timezone = "America/New_York"
generator = { type = "time_series", params = {
    start = "2024-01-01T00:00:00-05:00",
    arrival = { distribution = "exponential", params = { lambda = 0.1 }, unit = "second" },
    components = [
        { type = "seasonality",     params = { period = "24h", amplitude = 3.0 } },
        { type = "weekend_effect",  params = { multiplier = 0.4 } },
        { type = "business_hours",  params = { active_hours = [8, 22], active_multiplier = 5.0 } },
    ],
} }
```

### 9.4 Multi-Timezone Time Series

For datasets spanning multiple timezones (e.g., global services), combine timezone
fields with temporal generators:

```toml
[[entities]]
name = "global_transaction"
count = 10_000_000
temporal = true

[[entities.fields]]
name = "region"
type = "string"
generator = { type = "one_of", params = { choices = [
    { value = "us-east",  weight = 0.30 },
    { value = "us-west",  weight = 0.20 },
    { value = "eu-west",  weight = 0.25 },
    { value = "ap-east",  weight = 0.25 },
] } }

[[entities.fields]]
name = "region_timezone"
type = "string"
generator = { type = "conditional", params = {
    on = "region",
    branches = [
        { when = "us-east",  then = { type = "constant", params = { value = "America/New_York" } } },
        { when = "us-west",  then = { type = "constant", params = { value = "America/Los_Angeles" } } },
        { when = "eu-west",  then = { type = "constant", params = { value = "Europe/London" } } },
        { when = "ap-east",  then = { type = "constant", params = { value = "Asia/Tokyo" } } },
    ],
    default = { type = "constant", params = { value = "UTC" } },
} }

[[entities.fields]]
name = "event_time"
type = "datetimetz"
generator = { type = "business_hours", params = {
    start_hour = 8,
    end_hour = 20,
    timezone_field = "region_timezone",
    date_range = { min = "2024-01-01", max = "2024-12-31" },
} }

[[entities.fields]]
name = "event_time_utc"
type = "datetimetz"
timezone = "UTC"
generator = { type = "derived", params = { expr = "to_timezone(event_time, 'UTC')" } }
```

### 9.5 SLA / Deadline Patterns

Common pattern: generate a deadline relative to a creation time, with business-day
awareness:

```toml
[[entities.fields]]
name = "created_at"
type = "datetimetz"
timezone = "America/New_York"
generator = { type = "business_hours", params = {
    start_hour = 9, end_hour = 17,
    days = ["Mon", "Tue", "Wed", "Thu", "Fri"],
    date_range = { min = "2024-01-01", max = "2024-12-31" },
} }

[[entities.fields]]
name = "sla_deadline"
type = "datetimetz"
timezone = "America/New_York"
generator = { type = "relative", params = {
    anchor = "created_at",
    offset = { type = "one_of", params = { choices = [
        { value = "4h",  weight = 0.20 },
        { value = "8h",  weight = 0.30 },
        { value = "24h", weight = 0.30 },
        { value = "72h", weight = 0.20 },
    ] } },
} }

[[entities.fields]]
name = "resolved_at"
type = "datetimetz"
timezone = "America/New_York"
nullable = { probability = 0.15 }
generator = { type = "relative", params = {
    anchor = "created_at",
    offset = { distribution = "log_normal", params = { mu = 2.0, sigma = 1.5 }, min = "5m", max = "168h", unit = "hour" },
} }

[[entities.fields]]
name = "sla_met"
type = "bool"
generator = { type = "derived", params = { expr = "coalesce(resolved_at <= sla_deadline, false)" } }
```

---

## 10. Graph & Network Topology

For generating graph-structured data (social networks, knowledge graphs), Weave supports
graph topology specifications on relationships.

### 10.1 Network Topology Models

```toml
[[relationships]]
name = "friendship"
from = "user"
to = "user"
kind = "many_to_many"
from_field = "user_id"
to_field = "friend_id"

[relationships.topology]
model = "barabasi_albert"          # Network generation model
params = { m = 3 }                 # Each new node connects to 3 existing nodes
```

#### Supported Topology Models

| Model | Parameters | Properties |
|-------|-----------|------------|
| `erdos_renyi` | `p` (edge probability) | Random graph, uniform degree |
| `barabasi_albert` | `m` (edges per new node) | Scale-free, power-law degree |
| `watts_strogatz` | `k` (neighbors), `beta` (rewiring prob) | Small-world, high clustering |
| `stochastic_block` | `communities`, `p_intra`, `p_inter` | Community structure (simplified SBM) |
| `configuration` | `mean_degree`, `exponent` (opt), `min_degree` (opt) | Custom degree distribution |
| `forest` | `branching` (distribution) | Tree/forest structure |
| `complete` | — | Fully connected |

### 10.2 Edge Properties

Graph edges can carry attributes:

```toml
[[relationships]]
name = "friendship"
from = "user"
to = "user"
kind = "many_to_many"
from_field = "user_id"
to_field = "friend_id"

[[relationships.properties]]
name = "since"
type = "date"
generator = { type = "distribution", distribution = "uniform", params = { min = "2015-01-01", max = "2025-01-01" } }

[[relationships.properties]]
name = "strength"
type = "float"
generator = { type = "distribution", distribution = "beta", params = { alpha = 2.0, beta = 5.0 } }

[relationships.topology]
model = "watts_strogatz"
params = { k = 6, beta = 0.3 }
```

---

## 11. Noise & Perturbation Profiles

Perturbation profiles inject controlled imperfections into generated data. They are
applied **after** generation and **before** output binding.

### 11.1 Noise Declarations

```toml
[[noise]]
target = "user.email"                     # entity.field
type = "typo"                             # perturbation type
probability = 0.01                        # per-record probability
stage = "clean"                           # invariant stage
```

### 11.2 Noise Types

| Type | Description | Key Params |
|------|-------------|------------|
| `typo` | Character-level typos (swap, delete, insert, replace) | `probability` |
| `null_inject` | Replace value with NULL | `probability` |
| `outlier` | Replace with extreme values | `probability`, `multiplier` or `distribution` |
| `gaussian` | Add Gaussian noise to numeric fields | `std_dev` |
| `duplicate` | Duplicate entire records | `probability`, `near_duplicate` (bool) |
| `swap` | Swap values between records | `probability` |
| `truncate` | Truncate strings | `probability`, `max_length` |
| `format_error` | Invalid format (e.g., bad email) | `probability` |
| `fk_violate` | Replace FK with non-existent reference | `probability` |
| `temporal_spike` | Cluster timestamps around a point | `center`, `std_dev`, `probability` |
| `missing_field` | Omit field entirely (for document output) | `probability` |

### 11.3 Invariant Stages

Each noise type operates at one of three stages:

| Stage | Preserves Constraints? | Description |
|-------|----------------------|-------------|
| `clean` | Yes | Noise that preserves all blueprint constraints (types, uniqueness, FKs) |
| `constrained` | Partially | May violate soft constraints (nullability) but preserves structure |
| `breaking` | No | Intentionally violates constraints (FK integrity, type safety) |

The engine applies stages in order: `clean` → `constrained` → `breaking`.

### 11.4 Scoped Noise

Noise can be scoped to subsets of records:

```toml
[[noise]]
target = "order.amount"
type = "outlier"
probability = 0.01
scope = { where = "status == 'refunded'" }       # Only apply to refunded orders
params = { multiplier = { distribution = "uniform", params = { min = 5.0, max = 50.0 } } }
```

---

## 12. Blueprint Composition

### 12.1 Extends (Single Inheritance)

A Weave document can extend a base document, overriding or adding elements:

```toml
weave_version = "0.1"
extends = "base_ecommerce.toml"

[model]
name = "ecommerce_stress_test"
description = "10x scale with additional noise"

# Override: scale up user count
[[entities]]
name = "user"
count = 1_000_000

# Add a new field to user
[[entities.fields]]
name = "loyalty_points"
type = "int"
generator = { type = "distribution", distribution = "exponential", params = { lambda = 0.01 } }

# Add new noise
[[noise]]
target = "user.name"
type = "typo"
probability = 0.05
```

#### Merge Semantics

| Element | Merge Strategy |
|---------|---------------|
| `[model]` scalars | Child overrides parent |
| `[params]` | Child overrides by key; parent params preserved if not overridden |
| `[[types]]` | Merged by `name`; child replaces entire type definition |
| `[[mixins]]` | Merged by `name`; child replaces entire mixin |
| `[[entities]]` | Merged by `name`; child overrides scalars, merges fields by `name` |
| `[[entities.fields]]` | Merged by `name`; child replaces entire field definition |
| `[[relationships]]` | Merged by `name`; child replaces entire relationship |
| `[[correlations]]` | Appended (not merged) |
| `[[noise]]` | Appended (not merged) |

**Removing parent elements:**

```toml
[[entities]]
name = "legacy_table"
remove = true                # Removes this entity from the effective blueprint
```

**Inspecting effective blueprint:**

```bash
knit blueprint expand my_blueprint.toml     # Outputs fully flattened blueprint
```

### 12.2 Includes (Type Libraries)

Import reusable type and mixin definitions from external files:

```toml
includes = [
    "types/financial.toml",
    "types/healthcare.toml",
    "mixins/common.toml",
]
```

Included files may only contain `[[types]]` and `[[mixins]]` sections. They cannot
define entities, relationships, or noise profiles.

Naming conflicts between included files are validation errors. The including document's
definitions take precedence over included ones.

---

## 13. Complete Example

A comprehensive example demonstrating most language features:

```toml
weave_version = "0.1"

includes = ["types/common.toml"]

[model]
name = "online_marketplace"
description = "Multi-vendor marketplace with users, vendors, products, orders, and reviews"
seed = 2024
locale = "en_US"

[params]
scale = { type = "float", default = 1.0, description = "Scale factor for all entity counts" }
fraud_rate = { type = "float", default = 0.02, description = "Fraction of fraudulent orders" }

# ── Custom Types ────────────────────────────────────────────

[[types]]
name = "money"
base = "float"
generator = { type = "distribution", distribution = "log_normal", params = { mu = 3.5, sigma = 1.0 } }
constraints = { min = 0.01, precision = 2 }

[[types]]
name = "rating"
base = "int"
generator = { type = "one_of", params = { choices = [
    { value = 1, weight = 0.05 },
    { value = 2, weight = 0.10 },
    { value = 3, weight = 0.20 },
    { value = 4, weight = 0.35 },
    { value = 5, weight = 0.30 },
] } }

# ── Mixins ──────────────────────────────────────────────────

[[mixins]]
name = "timestamped"

[[mixins.fields]]
name = "created_at"
type = "datetime"
generator = { type = "distribution", distribution = "uniform", params = { min = "2020-01-01T00:00:00", max = "2025-12-31T23:59:59" } }

[[mixins.fields]]
name = "updated_at"
type = "datetime"
generator = { type = "derived", params = { expr = "date_add(created_at, random_int(0, 864000), 'second')" } }

# ── Entities ────────────────────────────────────────────────

[[entities]]
name = "user"
description = "Marketplace buyers"
tags = ["pii"]
count = { expr = "100000 * $param.scale" }
mixins = ["timestamped"]

[[entities.fields]]
name = "id"
type = "uuid"
primary_key = true

[[entities.fields]]
name = "first_name"
type = "string"
generator = { type = "faker", params = { category = "person.first_name" } }

[[entities.fields]]
name = "last_name"
type = "string"
generator = { type = "faker", params = { category = "person.last_name" } }

[[entities.fields]]
name = "email"
type = "string"
generator = { type = "unique", params = {
    inner = { type = "faker", params = { category = "internet.email" } },
} }

[[entities.fields]]
name = "age"
type = "int"
generator = { type = "distribution", distribution = "normal", params = { mean = 34.0, std_dev = 12.0 }, min = 18, max = 85 }

[[entities.fields]]
name = "income"
type = "float"
generator = { type = "distribution", distribution = "log_normal", params = { mu = 10.5, sigma = 0.8 } }

[[entities.fields]]
name = "country"
type = "string"
generator = { type = "lookup", params = {
    source = "data/countries.csv",
    column = "name",
    sampling = "weighted",
    weight_column = "population",
} }

[[entities.fields]]
name = "tier"
type = "string"
generator = { type = "one_of", params = { choices = [
    { value = "free",       weight = 0.60 },
    { value = "basic",      weight = 0.25 },
    { value = "premium",    weight = 0.10 },
    { value = "enterprise", weight = 0.05 },
] } }

# ── Vendor ──────────────────────────────────────────────────

[[entities]]
name = "vendor"
description = "Marketplace sellers"
count = { expr = "500 * $param.scale" }
mixins = ["timestamped"]

[[entities.fields]]
name = "id"
type = "uuid"
primary_key = true

[[entities.fields]]
name = "name"
type = "string"
generator = { type = "faker", params = { category = "company.name" } }

[[entities.fields]]
name = "rating"
type = "rating"

# ── Product ─────────────────────────────────────────────────

[[entities]]
name = "product"
description = "Products listed by vendors"
count = { expr = "20000 * $param.scale" }

[[entities.fields]]
name = "id"
type = "uuid"
primary_key = true

[[entities.fields]]
name = "vendor_id"
type = "uuid"

[[entities.fields]]
name = "name"
type = "string"
generator = { type = "faker", params = { category = "commerce.product_name" } }

[[entities.fields]]
name = "category"
type = "string"
generator = { type = "one_of", params = { choices = [
    { value = "electronics", weight = 0.25 },
    { value = "clothing",    weight = 0.20 },
    { value = "books",       weight = 0.15 },
    { value = "home",        weight = 0.15 },
    { value = "sports",      weight = 0.10 },
    { value = "food",        weight = 0.10 },
    { value = "other",       weight = 0.05 },
] } }

[[entities.fields]]
name = "price"
type = "money"

[[entities.fields]]
name = "in_stock"
type = "bool"
generator = { type = "distribution", distribution = "bernoulli", params = { p = 0.85 } }

# ── Order ───────────────────────────────────────────────────

[[entities]]
name = "order"
description = "Purchase orders"
count = { expr = "500000 * $param.scale" }
mixins = ["timestamped"]

[[entities.fields]]
name = "id"
type = "uuid"
primary_key = true

[[entities.fields]]
name = "user_id"
type = "uuid"

[[entities.fields]]
name = "product_id"
type = "uuid"

[[entities.fields]]
name = "quantity"
type = "int"
generator = { type = "distribution", distribution = "poisson", params = { lambda = 2.5 }, min = 1 }

[[entities.fields]]
name = "status"
type = "string"
generator = { type = "one_of", params = { choices = [
    { value = "completed",  weight = 0.65 },
    { value = "shipped",    weight = 0.15 },
    { value = "pending",    weight = 0.10 },
    { value = "cancelled",  weight = 0.07 },
    { value = "refunded",   weight = 0.03 },
] } }

[[entities.fields]]
name = "is_fraud"
type = "bool"
generator = { type = "distribution", distribution = "bernoulli", params = { p = "$param.fraud_rate" } }

# ── Review ──────────────────────────────────────────────────

[[entities]]
name = "review"
description = "Product reviews by users"
count = { expr = "200000 * $param.scale" }

[[entities.fields]]
name = "id"
type = "uuid"
primary_key = true

[[entities.fields]]
name = "user_id"
type = "uuid"

[[entities.fields]]
name = "product_id"
type = "uuid"

[[entities.fields]]
name = "rating"
type = "rating"

[[entities.fields]]
name = "text"
type = "string"
generator = { type = "faker", params = { category = "lorem.paragraph" } }

# ── Relationships ───────────────────────────────────────────

[[relationships]]
name = "product_vendor"
from = "product"
to = "vendor"
kind = "many_to_one"
from_field = "vendor_id"
to_field = "id"
degree = { distribution = "zipf", params = { n = 500, exponent = 1.3 } }

[[relationships]]
name = "order_user"
from = "order"
to = "user"
kind = "many_to_one"
from_field = "user_id"
to_field = "id"
degree = { distribution = "zipf", params = { n = 100000, exponent = 1.1 } }
selection = { strategy = "weighted", weight_field = "tier" }

[[relationships]]
name = "order_product"
from = "order"
to = "product"
kind = "many_to_one"
from_field = "product_id"
to_field = "id"
degree = { distribution = "zipf", params = { n = 20000, exponent = 1.5 } }

[[relationships]]
name = "review_user"
from = "review"
to = "user"
kind = "many_to_one"
from_field = "user_id"
to_field = "id"

[[relationships]]
name = "review_product"
from = "review"
to = "product"
kind = "many_to_one"
from_field = "product_id"
to_field = "id"

# ── Correlations ────────────────────────────────────────────

[[correlations]]
entity = "user"
fields = ["age", "income"]
coefficient = 0.55
method = "copula"

[[correlations]]
entity = "order"
type = "conditional_distribution"
dependent = "quantity"
given = "is_fraud"
distributions = [
    { when = true,  distribution = "poisson", params = { lambda = 8.0 } },
    { when = false, distribution = "poisson", params = { lambda = 2.5 } },
]

# ── Noise ───────────────────────────────────────────────────

[[noise]]
target = "user.email"
type = "typo"
probability = 0.005
stage = "clean"

[[noise]]
target = "user.first_name"
type = "null_inject"
probability = 0.01
stage = "constrained"

[[noise]]
target = "order.product_id"
type = "fk_violate"
probability = 0.001
stage = "breaking"
scope = { where = "is_fraud == true" }

[[noise]]
target = "review.rating"
type = "outlier"
probability = 0.02
stage = "clean"
params = { values = [1, 5] }
```

---

## 14. Validation Rules

The Knit engine validates Weave documents before generation. Validation errors are
reported with element paths and human-readable messages.

### 14.1 Structural Validation

- All required fields present (`name`, `type` for fields; `name`, `count` for entities)
- No duplicate names within scope (entities, fields, types, relationships)
- Valid `weave_version`

### 14.2 Type Validation

- Field types resolve to a primitive or defined custom type
- Generator output type is compatible with field type
- Distribution parameters are valid (e.g., `std_dev > 0`, `p ∈ [0,1]`)

### 14.3 Referential Validation

- Relationship `from`/`to` entities exist
- Relationship `from_field`/`to_field` exist in their respective entities
- FK field type matches PK field type
- Mixin names referenced in entities exist
- Custom type names used in fields exist
- `extends` and `includes` files exist and are valid

### 14.4 Semantic Validation

- Derived field expressions form a DAG (no cycles)
- Conditional generator `on` field exists and precedes the conditional field
- Uniqueness feasibility (domain space ≥ entity count)
- Correlation matrix is positive semi-definite
- Correlation fields exist and are numeric
- Cyclic relationships have nullable FK fields
- Self-referential relationships have `nullable = true`
- Lookup file exists and contains referenced column

### 14.5 Warnings (Non-Fatal)

- Large entity counts without `scale` parameterization
- Distributions with extreme parameters (likely unintended)
- Fields without explicit generators (engine will use type defaults)
- Unused types or mixins

---

## 15. Default Generator Rules

When a field has no explicit `generator`, the engine applies defaults based on the
field's type:

| Type | Default Generator |
|------|-------------------|
| `bool` | `bernoulli(p=0.5)` |
| `int` | `uniform(min=0, max=1000000)` |
| `float` | `uniform(min=0.0, max=1000.0)` |
| `string` | `faker(category="lorem.word")` |
| `date` | `uniform(min="2020-01-01", max="2025-12-31")` |
| `time` | `uniform(min="00:00:00", max="23:59:59")` |
| `datetime` | `uniform(min="2020-01-01T00:00:00", max="2025-12-31T23:59:59")` |
| `datetimetz` | `uniform(min="2020-01-01T00:00:00Z", max="2025-12-31T23:59:59Z")` with model default timezone |
| `duration` | `uniform(min="0s", max="24h")` |
| `uuid` | `uuid(version=4)` |
| `bytes` | `random_bytes(length=uniform(8,256))` |

Fields that are FK targets in a relationship have their values generated by the
relationship resolver, not by their default generator.

---

## 16. JSON Representation

Every Weave TOML document has an equivalent JSON form. This enables AI pipelines
that prefer JSON output:

```json
{
  "weave_version": "0.1",
  "model": {
    "name": "example",
    "seed": 42
  },
  "entities": [
    {
      "name": "user",
      "count": 1000,
      "fields": [
        {
          "name": "id",
          "type": "uuid",
          "primary_key": true
        },
        {
          "name": "age",
          "type": "int",
          "generator": {
            "type": "distribution",
            "distribution": "normal",
            "params": { "mean": 35.0, "std_dev": 12.0 },
            "min": 18,
            "max": 99
          }
        }
      ]
    }
  ]
}
```

---

## 17. Comparison with Other Tools

Weave is designed to be a superset of capabilities found in existing data generation
tools. The following table maps features from popular tools to Weave constructs:

| Feature | Synth | Mockaroo | SDV | Faker | Weave |
|---------|-------|---------|-----|-------|-------|
| Declarative blueprint | JSON | UI/JSON | Python/YAML | Code | TOML/JSON |
| Statistical distributions | Limited | No | Learned | No | 17+ built-in |
| Weighted choices | `one_of` | Weighted | N/A | N/A | `one_of` with weights |
| Regex patterns | ✓ | ✓ | No | No | `pattern` generator |
| Faker integration | ✓ | Built-in | No | Native | `faker` generator |
| Sequential/series | `series` | Row Number | No | No | `sequence` generator |
| Foreign keys | `datasource` | Limited | Learned | No | `[[relationships]]` |
| Conditional logic | Limited | Formula | No | No | `conditional` generator |
| Computed fields | No | Formula | No | No | `derived` generator |
| External data lookup | No | Dataset | Source data | No | `lookup` generator |
| Uniqueness | `unique` | ✓ | No | No | `unique` wrapper |
| Nested objects | ✓ | No | No | No | Nested `fields` |
| Array fields | ✓ | No | No | No | `composite` generator |
| Multi-table relationships | Limited | No | ✓ (learned) | No | Full relational model |
| Cardinality distribution | No | No | Learned | No | `degree` distribution |
| Correlation modeling | No | No | Copula/GAN | No | `[[correlations]]` |
| Time series | No | No | TimeGAN | No | `time_series` generator |
| Graph topology | No | No | No | No | `[topology]` on relationships |
| Noise injection | No | No | No | No | `[[noise]]` profiles |
| Custom types | No | No | No | No | `[[types]]` |
| Reusable field groups | No | No | No | No | `[[mixins]]` |
| Blueprint composition | No | No | No | No | `extends` / `includes` |
| Parameterization | No | No | No | No | `[params]` |
| Self-referential relations | No | No | No | No | ✓ with `acyclic`, `max_depth` |
| Cyclic relations | No | No | No | No | Automatic two-phase gen |
| Invariant-aware noise | No | No | No | No | Three-stage pipeline |
| AI-friendly format | No | No | No | No | Canonical TOML + JSON |
| **Temporal features** | | | | | |
| Timezone-aware datetimes | No | No | No | No | `datetimetz` type + IANA tz |
| Duration / timespan type | No | No | No | No | `duration` type |
| Relative temporal gen | No | Formula | No | No | `relative` generator |
| Business hours gen | No | No | No | No | `business_hours` generator |
| Temporal sequences w/ jitter | No | No | No | No | `sequence` + `jitter` |
| Time series seasonality | No | No | TimeGAN | No | Composable components |
| Multi-timezone datasets | No | No | No | No | `timezone_field` + per-field tz |
| SLA / deadline patterns | No | Formula | No | No | `relative` + `derived` |
| Temporal noise (spikes) | No | No | No | No | `temporal_spike` noise |

---

## 18. Extension Points

### 18.1 Custom Generators

Users can register custom generators by name. In the blueprint:

```toml
generator = { type = "custom", params = { name = "acme::iban", country = "DE" } }
```

Custom generators are resolved via the Knit plugin registry at runtime.

### 18.2 Custom Distributions

```toml
generator = { type = "distribution", distribution = "custom", params = { name = "bimodal", peaks = [10, 50], weights = [0.3, 0.7] } }
```

### 18.3 Custom Noise Types

```toml
[[noise]]
target = "user.address"
type = "custom"
params = { name = "address_anonymizer", k = 5 }
```

### 18.4 Custom Faker Providers

```toml
generator = { type = "faker", params = { category = "custom::product_sku", provider = "acme_skus" } }
```

---

## 19. Reserved Keywords

The following names are reserved and cannot be used as entity, field, type, or mixin names:

`model`, `params`, `types`, `mixins`, `entities`, `relationships`, `correlations`,
`noise`, `extends`, `includes`, `weave_version`, `remove`, `true`, `false`, `null`.

---

## 20. File Extension

Weave documents use the `.knit.toml` extension (TOML format) or `.weave.json`
(JSON format). The engine auto-detects format from the extension.

```
my_dataset.knit.toml
my_dataset.weave.json
```

---

## Appendix A: Grammar Summary (EBNF-like)

```
Document        = Version Model [Params] {Type} {Mixin} {Entity} {Relationship}
                  {Correlation} {Noise}

Version         = "weave_version" "=" STRING

Model           = "[model]" "name" "=" STRING { ModelProp }
ModelProp       = "description" "=" STRING
                | "seed" "=" INTEGER
                | "locale" "=" STRING

Params          = "[params]" { ParamDef }
ParamDef        = NAME "=" ParamValue
ParamValue      = LITERAL | "{" "type" "=" STRING "," "default" "=" LITERAL
                  ["," "description" "=" STRING] "}"

Type            = "[[types]]" "name" "=" STRING "base" "=" STRING
                  ["generator" "=" Generator] ["constraints" "=" Constraints]

Mixin           = "[[mixins]]" "name" "=" STRING {Field}

Entity          = "[[entities]]" "name" "=" STRING "count" "=" CountSpec
                  ["mixins" "=" "[" {STRING} "]"] {Field} {Constraint}

CountSpec       = INTEGER | "{" "expr" "=" STRING "}" | "{" "min" "=" INT "," "max" "=" INT "}"

Field           = "[[entities.fields]]" "name" "=" STRING "type" "=" STRING
                  ["primary_key" "=" BOOL] ["unique" "=" BOOL]
                  ["nullable" "=" NullSpec] ["generator" "=" Generator]
                  ["description" "=" STRING]

Generator       = "{" "type" "=" STRING ["," GeneratorParams] "}"

Relationship    = "[[relationships]]" "name" "=" STRING
                  "from" "=" STRING "to" "=" STRING "kind" "=" STRING
                  "from_field" "=" STRING "to_field" "=" STRING
                  ["degree" "=" DistSpec] ["selection" "=" SelectionSpec]
                  ["nullable" "=" BOOL] ["acyclic" "=" BOOL]

Correlation     = "[[correlations]]" "entity" "=" STRING
                  "fields" "=" "[" {STRING} "]" CorrelationSpec

Noise           = "[[noise]]" "target" "=" STRING "type" "=" STRING
                  "probability" "=" FLOAT ["stage" "=" STRING]
                  ["scope" "=" Scope] ["params" "=" Table]
```

---

## Appendix B: Versioning Policy

Weave follows semantic versioning for the blueprint language:

- **Patch** (0.1.x): Bug fixes, clarifications. All valid documents remain valid.
- **Minor** (0.x.0): New features (additive). Existing documents remain valid.
- **Major** (x.0.0): Breaking changes. Old documents may need migration.

The engine reports clear errors when encountering unsupported versions and provides
migration guidance where possible.
