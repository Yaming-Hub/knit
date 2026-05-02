# Weave Schema Language Tutorial

A practical, example-driven tutorial for writing Knit schemas. For the formal
grammar and exhaustive reference, see the
[Weave Specification](../weave-spec.md).

**[← Back to User Guide](index.md)**

---

## Table of Contents

- [Schema Structure](#schema-structure)
- [Field Definitions](#field-definitions)
- [Data Types](#data-types)
- [Generators](#generators)
- [Count Specifications](#count-specifications)
- [Null Injection](#null-injection)
- [Relationships](#relationships)
- [Schema Inheritance](#schema-inheritance)
- [Correlations](#correlations)
- [Noise Profiles](#noise-profiles)
- [Constraints](#constraints)

---

## Schema Structure

Every `.weave.toml` file has this top-level structure:

```toml
schema_version = "1.0"

[model]
name = "my_dataset"
seed = 42

[[entities]]
# ... entity definitions ...

[[relationships]]
# ... relationship definitions (optional) ...

[[correlations]]
# ... correlation specs (optional) ...

[[noise_profiles]]
# ... noise configuration (optional) ...
```

### Required Fields

| Field | Description |
|-------|-------------|
| `schema_version` | Always `"1.0"` |
| `[model].name` | Dataset name (used in output file naming) |
| `[model].seed` | RNG seed for deterministic, reproducible output |

---

## Field Definitions

Each entity contains an array of fields. A field has:

```toml
[[entities.fields]]
name = "email"            # Column name (required)
data_type = "string"      # Data type (required)
primary_key = false       # Is this the primary key? (default: false)
nullable = false          # Can this field be null? (default: false)
[entities.fields.generator]
type = "pattern"          # Generator type (required)
pattern = "??##@test.com" # Generator-specific params
```

| Attribute | Required | Default | Description |
|-----------|----------|---------|-------------|
| `name` | ✅ | — | Column name |
| `data_type` | ✅ | — | One of the supported data types |
| `primary_key` | ❌ | `false` | Marks this field as the entity's PK |
| `nullable` | ❌ | `false` | Whether null values can appear |
| `[generator]` | ✅ | — | How to generate values |

---

## Data Types

Knit supports the following data types:

| Type | Description | Arrow Mapping |
|------|-------------|---------------|
| `int` | 64-bit signed integer | Int64 |
| `float` | 64-bit floating point | Float64 |
| `string` | UTF-8 text | Utf8 |
| `bool` | Boolean true/false | Boolean |
| `date` | Calendar date (YYYY-MM-DD) | Date32 |
| `datetime` | Naive date + time | Timestamp(μs) |
| `datetimetz` | Timezone-aware date + time | Timestamp(μs, tz) |
| `time` | Time of day (HH:MM:SS) | Time64(μs) |
| `duration` | Time span | Duration(μs) |
| `uuid` | UUID v4 or v7 | Utf8 (string) |
| `bytes` | Binary data | Binary |
| `array<T>` | Array of typed elements | List |
| `object` | Nested document | Struct |

---

## Generators

Generators control how values are produced for each field. Every field needs
exactly one generator.

### `sequence` — Auto-Increment IDs

Produces an ordered series of values.

```toml
[[entities.fields]]
name = "id"
data_type = "int"
primary_key = true
[entities.fields.generator]
type = "sequence"
start = 1
step = 1
```

| Param | Type | Default | Description |
|-------|------|---------|-------------|
| `start` | int | `1` | Starting value |
| `step` | int | `1` | Increment between values |

### `uuid` — Random UUIDs

Generates universally unique identifiers.

```toml
[[entities.fields]]
name = "trace_id"
data_type = "uuid"
[entities.fields.generator]
type = "uuid"
version = 4
```

| Param | Type | Default | Description |
|-------|------|---------|-------------|
| `version` | int | `4` | UUID version: `4` (random) or `7` (time-sortable) |

### `distribution` — Statistical Distributions

Draw values from a statistical distribution. Knit supports 17+ distributions.

#### Normal Distribution

```toml
[entities.fields.generator]
type = "distribution"
kind = "normal"
[entities.fields.generator.params]
mean = 50.0
std_dev = 15.0
```

#### Uniform Distribution

```toml
[entities.fields.generator]
type = "distribution"
kind = "uniform"
[entities.fields.generator.params]
min = 0.0
max = 100.0
```

#### Log-Normal Distribution

Great for prices, salaries, and other right-skewed data:

```toml
[entities.fields.generator]
type = "distribution"
kind = "log_normal"
[entities.fields.generator.params]
mu = 4.0
sigma = 1.0
```

#### Exponential Distribution

Models time between events:

```toml
[entities.fields.generator]
type = "distribution"
kind = "exponential"
[entities.fields.generator.params]
lambda = 0.5
```

#### Poisson Distribution

Count data — e.g., events per time period:

```toml
[entities.fields.generator]
type = "distribution"
kind = "poisson"
[entities.fields.generator.params]
lambda = 5.0
```

#### Zipf Distribution

Power-law — models popularity, word frequency, page views:

```toml
[entities.fields.generator]
type = "distribution"
kind = "zipf"
[entities.fields.generator.params]
n = 1000
exponent = 1.2
```

#### Beta Distribution

Values between 0 and 1 — scores, ratings, probabilities:

```toml
[entities.fields.generator]
type = "distribution"
kind = "beta"
[entities.fields.generator.params]
alpha = 2.0
beta = 5.0
```

#### Bernoulli Distribution

Boolean coin flip with configurable probability:

```toml
[entities.fields.generator]
type = "distribution"
kind = "bernoulli"
[entities.fields.generator.params]
p = 0.8
```

#### Other Distributions

Knit also supports: `gamma`, `pareto`, `geometric`, `binomial`, `weibull`,
`cauchy`, `chi_squared`, `student_t`, and more. See the
[Weave Specification](../weave-spec.md) for the full list.

### `one_of` — Weighted Categorical Choices

Pick from a list of values with optional weights:

```toml
[entities.fields.generator]
type = "one_of"
choices = [
  { value = "active",    weight = 70 },
  { value = "inactive",  weight = 20 },
  { value = "suspended", weight = 10 },
]
```

Without weights, all choices are equally likely:

```toml
[entities.fields.generator]
type = "one_of"
choices = [
  { value = "red" },
  { value = "green" },
  { value = "blue" },
]
```

### `pattern` — Pattern-Based Strings

Generate strings from a format pattern:

| Token | Meaning | Example |
|-------|---------|---------|
| `#` | Digit (0–9) | `###` → `847` |
| `?` | Lowercase letter | `???` → `qmz` |
| `A` | Uppercase letter | `AAA` → `KPX` |

```toml
# Phone number
[entities.fields.generator]
type = "pattern"
pattern = "(###) ###-####"

# SKU code
[entities.fields.generator]
type = "pattern"
pattern = "SKU-AAA-####"

# Email
[entities.fields.generator]
type = "pattern"
pattern = "user####@example.com"
```

### `ref` — Foreign Key References

Reference a field from another entity (foreign key):

```toml
[[entities.fields]]
name = "customer_id"
data_type = "int"
[entities.fields.generator]
type = "ref"
entity = "customers"
field = "id"
```

Values are drawn from the referenced entity's generated values, ensuring
referential integrity. See [Relationships](#relationships) for defining
formal FK relationships.

### `expression` / `derived` — Computed Fields

Derive a field's value from other fields in the same entity:

```toml
[[entities.fields]]
name = "total"
data_type = "float"
[entities.fields.generator]
type = "derived"
expr = "quantity * unit_price"
```

The expression language supports 50+ functions:

```toml
# String manipulation
expr = "upper(first_name) || ' ' || upper(last_name)"

# Date arithmetic
expr = "date_add(start_date, duration_days, 'day')"

# Conditional logic
expr = "case when age >= 18 then 'adult' else 'minor' end"

# Numeric
expr = "round(amount * tax_rate, 2)"
```

See the [Weave Specification](../weave-spec.md) for the full expression
function reference.

### `constant` — Fixed Values

Every row gets the same value:

```toml
[entities.fields.generator]
type = "constant"
value = "USD"
```

### `time_series` — Temporal Data Generation

Generate time series data with composable components:

```toml
[entities.fields.generator]
type = "time_series"
baseline = 100.0
[[entities.fields.generator.components]]
type = "trend"
slope = 0.5

[[entities.fields.generator.components]]
type = "seasonality"
period = 24
amplitude = 10.0

[[entities.fields.generator.components]]
type = "noise"
distribution = "normal"
std_dev = 2.0
```

Available components:

| Component | Purpose |
|-----------|---------|
| `trend` | Linear or polynomial drift |
| `seasonality` | Periodic patterns (daily, weekly, yearly) |
| `noise` | Random perturbation |
| `ar` | Autoregressive (AR) model |
| `spike` | Anomaly bursts |
| `level_shift` | Permanent step changes |
| `mean_reversion` | Converge toward a target value |
| `weekend_effect` | Day-of-week patterns |
| `holiday_effect` | Date-specific spikes |
| `business_hours` | Day/night patterns |

### Other Generators

Knit also includes these generators:

| Generator | Purpose |
|-----------|---------|
| `faker` | Locale-aware realistic data (names, addresses, companies) |
| `conditional` | Choose generator based on another field's value |
| `composite` | Generate arrays of values |
| `lookup` | Sample from an external data file (CSV, JSON, Parquet) |
| `unique` | Wrapper enforcing uniqueness on any generator |
| `relative` | Datetime offset from another field |
| `business_hours` | Constrained to business hours with timezone awareness |

---

## Count Specifications

The `count` field on an entity controls how many rows to generate.

### Fixed Count

```toml
[[entities]]
name = "users"
count = 10000
```

### Range Count

Generate a random count within a range:

```toml
[[entities]]
name = "users"
count = { min = 9000, max = 11000 }
```

### Expression Count

Use parameters or expressions:

```toml
[[entities]]
name = "users"
count = { expr = "$param.user_count * $param.scale" }
```

---

## Null Injection

Control how null values appear in a field:

```toml
# Never null (default)
nullable = false

# 5% of values are null
nullable = { probability = 0.05 }

# Every 100th record is null
nullable = { every_n = 100 }

# Conditional nulls
nullable = { when = "tier == 'free'" }
```

Example — a nullable comment field:

```toml
[[entities.fields]]
name = "comment"
data_type = "string"
nullable = { probability = 0.30 }
[entities.fields.generator]
type = "faker"
category = "lorem.sentence"
```

---

## Relationships

Relationships define foreign key connections between entities.

### Basic Many-to-One

```toml
[[relationships]]
name = "order_customer"
from = "orders"           # Child entity (has the FK)
to = "customers"          # Parent entity (has the PK)
kind = "many_to_one"
from_field = "customer_id"
to_field = "id"
```

### Relationship Kinds

| Kind | Description |
|------|-------------|
| `many_to_one` | Many child rows → one parent row |
| `one_to_one` | One child row → one parent row |
| `many_to_many` | Many-to-many via junction entity |

### Self-Referential Relationships (Hierarchies)

Entities can reference themselves — useful for org charts, categories, etc.:

```toml
[[relationships]]
name = "employee_manager"
from = "employees"
to = "employees"
kind = "many_to_one"
from_field = "manager_id"
to_field = "id"
nullable = true
root_probability = 0.05
max_depth = 6
acyclic = true
```

- **`root_probability`** — Chance a record has no parent (top-level nodes)
- **`max_depth`** — Maximum hierarchy depth
- **`acyclic`** — Prevent circular references

### Degree Distribution

Control how many children each parent gets:

```toml
[[relationships]]
name = "order_customer"
from = "orders"
to = "customers"
kind = "many_to_one"
from_field = "customer_id"
to_field = "id"
degree = { distribution = "zipf", params = { n = 100000, exponent = 1.2 } }
```

---

## Schema Inheritance

Reuse and extend base schemas with `extends`:

```toml
# stress_test.weave.toml
extends = "base_ecommerce.weave.toml"

[model]
name = "ecommerce_stress_test"

# Override the user count
[[entities]]
name = "users"
count = 1_000_000

# Add a new field to users
[[entities.fields]]
name = "loyalty_points"
data_type = "int"
[entities.fields.generator]
type = "distribution"
kind = "exponential"
[entities.fields.generator.params]
lambda = 0.01
```

**Merge rules:**

- Entities and fields merge by `name` — matching names override the parent
- New entities/fields are added
- Use `remove = true` to delete an inherited element
- Child model settings override parent settings

### Mixins

Reusable field groups that can be included in multiple entities:

```toml
[[mixins]]
name = "auditable"

[[mixins.fields]]
name = "created_at"
data_type = "datetimetz"
[mixins.fields.generator]
type = "distribution"
kind = "uniform"
[mixins.fields.generator.params]
min = "2024-01-01T00:00:00Z"
max = "2024-12-31T23:59:59Z"

[[mixins.fields]]
name = "updated_at"
data_type = "datetimetz"
[mixins.fields.generator]
type = "relative"
anchor = "created_at"
offset = { distribution = "exponential", params = { lambda = 0.001 } }

# Use the mixin
[[entities]]
name = "orders"
count = 10000
mixins = ["auditable"]
```

---

## Correlations

Specify statistical correlations between fields:

### Pairwise Correlation

```toml
[[correlations]]
entity = "accounts"
fields = ["balance", "risk_score"]
coefficient = -0.4
method = "copula"
```

### Correlation Matrix

For three or more correlated fields:

```toml
[[correlations]]
entity = "employees"
fields = ["years_experience", "salary", "performance_rating"]
matrix = [
  [1.0, 0.7, 0.5],
  [0.7, 1.0, 0.3],
  [0.5, 0.3, 1.0],
]
method = "copula"
```

### Conditional Distribution

Different distributions for different categories:

```toml
[[correlations]]
type = "conditional_distribution"
entity = "transactions"
dependent = "amount"
given = "category"
distributions = [
  { when = "groceries",    distribution = "log_normal", params = { mu = 3.0, sigma = 0.8 } },
  { when = "electronics",  distribution = "log_normal", params = { mu = 5.5, sigma = 1.2 } },
  { when = "dining",       distribution = "log_normal", params = { mu = 2.8, sigma = 0.5 } },
]
```

---

## Noise Profiles

Add realistic data quality issues to test your pipelines. For a deep dive,
see the [Noise Injection Guide](noise.md).

```toml
[[noise_profiles]]
target = "users.email"
type = "typo"
probability = 0.02

[[noise_profiles]]
target = "orders.amount"
type = "outlier"
probability = 0.01
multiplier = 10.0
direction = "high"
```

---

## Constraints

Add integrity constraints beyond simple types:

### Composite Uniqueness

```toml
[[entities.constraints]]
type = "unique"
fields = ["user_id", "order_number"]
```

### Check Constraints

```toml
[[entities.constraints]]
type = "check"
expr = "end_date >= start_date"
```

### Not-Null Constraints

```toml
[[entities.constraints]]
type = "not_null"
fields = ["id", "user_id"]
```

---

## What's Next?

- **[CLI Reference](cli-reference.md)** — All commands and flags
- **[Examples Walkthrough](examples.md)** — See these features in real schemas
- **[Noise Injection Guide](noise.md)** — Configure data quality testing
- **[Weave Specification](../weave-spec.md)** — Formal grammar and full reference
