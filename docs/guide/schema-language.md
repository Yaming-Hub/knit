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
- [Custom Types](#custom-types)
- [Mixins](#mixins)
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

> **Note:** The actual TOML section for noise is `[[noise]]`, not
> `[[noise_profiles]]`. See [Noise Injection Guide](noise.md) for details.

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
type = "one_of"           # Generator type (required)
choices = [               # Generator-specific params
  { value = "user1@test.com" },
  { value = "user2@test.com" },
]
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

### Nested Objects (`object`)

Fields with `data_type = "object"` define hierarchical document structures.
Sub-fields are declared inline using nested `[[entities.fields.fields]]`
sections. Nesting can be arbitrary depth.

```toml
[[entities.fields]]
name = "address"
data_type = "object"

[[entities.fields.fields]]
name = "city"
data_type = "string"
generator = { type = "faker", method = "city_name" }

[[entities.fields.fields]]
name = "zip"
data_type = "string"
generator = { type = "faker", method = "zip_code" }

# Nested within nested
[[entities.fields.fields]]
name = "coordinates"
data_type = "object"

[[entities.fields.fields.fields]]
name = "lat"
data_type = "float"
[entities.fields.fields.fields.generator]
type = "distribution"
kind = "uniform"
[entities.fields.fields.fields.generator.params]
min = -90.0
max = 90.0

[[entities.fields.fields.fields]]
name = "lon"
data_type = "float"
[entities.fields.fields.fields.generator]
type = "distribution"
kind = "uniform"
[entities.fields.fields.fields.generator.params]
min = -180.0
max = 180.0
```

**Restrictions on nested fields:**
- Only simple generators: `distribution`, `faker`, `constant`, `sequence`,
  `one_of`, `uuid_gen`
- No `primary_key` or `actor_column` on nested fields
- No FK, graph_target, persona, derived, relative, or conditional generators
- Precision (`precision`) works on nested float fields

**Output formats:**
- JSON/JSONL: native nested objects
- Parquet/Arrow IPC: native struct columns
- Avro: real nested record schemas
- SQL: structs serialized as JSON TEXT
- CSV: structs serialized as JSON strings

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

### `uuid_gen` — Random UUIDs

Generates universally unique identifiers.

```toml
[[entities.fields]]
name = "trace_id"
data_type = "uuid"
[entities.fields.generator]
type = "uuid_gen"
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
# NOTE: Currently produces null output
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

### `lookup` — Foreign Key References

Reference a field from another entity (foreign key):

```toml
[[entities.fields]]
name = "customer_id"
data_type = "int"
[entities.fields.generator]
type = "lookup"
entity = "customers"
field = "id"
```

Values are drawn from the referenced entity's generated values, ensuring
referential integrity. See [Relationships](#relationships) for defining
formal FK relationships.

### `external_lookup` — External Data Source Sampling

Sample values from an external CSV, JSON, or Parquet file:

```toml
[[entities.fields]]
name = "city"
data_type = "string"
[entities.fields.generator]
type = "external_lookup"
source = "data/cities.csv"
column = "city_name"
format = "csv"
sampling = "uniform"
```

**Parameters:**

| Parameter | Required | Description |
|-----------|----------|-------------|
| `source` | Yes | Path to data file (relative to schema file) |
| `column` | Yes | Column name to sample from |
| `format` | Yes | File format: `csv`, `json`, or `parquet` |
| `sampling` | No | `uniform` (default), `weighted`, or `sequential` |
| `weight_column` | When weighted | Column containing sampling weights |

**Weighted sampling example:**

```toml
[entities.fields.generator]
type = "external_lookup"
source = "data/cities.csv"
column = "city_name"
format = "csv"
sampling = "weighted"
weight_column = "population"
```

**Portability rules:**
- Paths must be relative to the schema file (no absolute paths, no `..`)
- Source files must be included alongside the schema
- Missing file or column is a validation error
- Sequential mode uses row offset for deterministic round-robin

### `expression` / `derived` — Computed Fields

Derive a field's value from other fields in the same entity:

```toml
[[entities.fields]]
name = "total"
data_type = "float"
[entities.fields.generator]
type = "derived"
expr = "${quantity} * ${unit_price}"
```

The expression language supports 63+ built-in functions with SQL-like null
semantics. Reference other fields with `${field_name}` and parameters with
`${param.key}`.

```toml
# String manipulation
expr = "upper(${first_name}) + \" \" + upper(${last_name})"

# Math functions
expr = "round(${amount} * ${param.tax_rate}, 2)"
expr = "sqrt(pow(${x}, 2) + pow(${y}, 2))"

# Conditional logic
expr = "if(${age} >= 18, \"adult\", \"minor\")"
expr = "case(${score} >= 90, \"A\", ${score} >= 80, \"B\", ${score} >= 70, \"C\", \"F\")"

# Date/time construction and extraction
expr = "make_date(2024, ${month}, ${day})"
expr = "year(${created_at})"
expr = "format_date(${event_time}, \"%Y-%m-%d\")"

# Date/time arithmetic
expr = "date_add(${start_date}, 30, \"day\")"
expr = "date_diff(${end_date}, ${start_date}, \"day\")"
expr = "start_of(${event_time}, \"month\")"

# String predicates
expr = "if(starts_with(${email}, \"admin\"), \"staff\", \"user\")"

# Padding and formatting
expr = "pad_left(cast_string(${id}), 8, \"0\")"

# Hashing for deterministic bucketing
expr = "hash(${user_id}) % 10"

# Global row numbering
expr = "row_number()"
```

**Function categories:**

| Category | Functions |
|----------|-----------|
| Math | `abs`, `ceil`, `floor`, `round`, `min`, `max`, `clamp`, `sqrt`, `pow`, `ln`, `log`, `exp` |
| String | `upper`, `lower`, `trim`, `len`, `concat`, `substr`, `replace`, `left`, `right`, `pad_left`, `pad_right`, `starts_with`, `ends_with`, `contains` |
| Conditional | `if`, `case`, `coalesce`, `nullif` |
| Type cast | `cast_int`, `cast_float`, `cast_string` |
| Random | `random_int`, `random_float`, `random_duration` |
| Date/time construction | `make_date`, `make_time`, `make_datetime`, `make_duration`, `to_date`, `to_datetime`, `epoch_seconds`, `from_epoch` |
| Date/time extraction | `year`, `month`, `day`, `hour`, `minute`, `second`, `day_of_week`, `day_of_year`, `week_of_year`, `quarter` |
| Date/time arithmetic | `date_add`, `date_sub`, `date_diff`, `duration_add`, `start_of`, `end_of` |
| Date/time formatting | `format_date`, `format_duration` |
| Timezone | `to_timezone`, `timezone_offset` |
| Utility | `hash`, `row_number` |

See the [Weave Specification](../weave-spec.md) for the full expression
function reference.

### `constant` — Fixed Values

Every row gets the same value:

```toml
[entities.fields.generator]
type = "constant"
value = "USD"
```

### `time_series` — Numeric Time Series

Generate composable numeric time series with additive components. Produces
Float64 values by summing a baseline with trend, seasonality, noise, and other
components. Ideal for generating realistic metrics like CPU usage, temperature,
and network traffic.

```toml
[[entities.fields]]
name = "temperature"
data_type = "float"
[entities.fields.generator]
type = "time_series"
baseline = 20.0
min = -10.0
max = 50.0
timestamp_field = "timestamp"  # optional: enables calendar-aware components

[[entities.fields.generator.components]]
type = "trend"
slope = 0.001
degree = 1

[[entities.fields.generator.components]]
type = "seasonality"
period = "24h"
amplitude = 8.0
phase = -1.57

[[entities.fields.generator.components]]
type = "noise"
std_dev = 1.5

[[entities.fields.generator.components]]
type = "ar"
coefficients = [0.7]
```

**Parameters:**

| Param | Type | Default | Description |
|-------|------|---------|-------------|
| `baseline` | float | `0.0` | Base value around which the series fluctuates |
| `components` | array | required | List of additive component specifications |
| `min` | float | none | Minimum output value (clamp) |
| `max` | float | none | Maximum output value (clamp) |
| `timestamp_field` | string | none | Field name for calendar-aware components |

**Available components:**

| Component | Parameters | Description |
|-----------|-----------|-------------|
| `trend` | `slope`, `degree` (default 1) | Polynomial drift: `slope × t^degree` |
| `seasonality` | `period`, `amplitude`, `phase` (default 0) | Sinusoidal: `amplitude × sin(2π×t/period + phase)`. Period can be duration string (`"24h"`, `"7d"`) or number of rows |
| `noise` | `std_dev` | Gaussian random noise |
| `ar` | `coefficients` | Autoregressive model. Sum of |coefficients| should be < 1 for stability |
| `spike` | `probability`, `magnitude`, `duration_steps` (default 1) | Anomalous bursts lasting N steps |
| `level_shift` | `probability`, `magnitude` | Permanent baseline shift |
| `mean_reversion` | `target`, `speed` | Pull toward target: `speed × (target - current)` |
| `weekend_effect` | `multiplier` | Multiply by factor on weekends (requires `timestamp_field`) |
| `business_hours_effect` | `start_hour`, `end_hour`, `active_multiplier` | Multiply during business hours (requires `timestamp_field`) |

**Notes:**
- Stateful components (AR, spike, level_shift, mean_reversion) maintain state
  across batches and force sequential partition execution
- Calendar-aware components (weekend_effect, business_hours_effect) require
  `timestamp_field` pointing to a datetime field in the same entity
- See `examples/time_series_metrics.weave.toml` for a complete example

### `event_stream` — Irregular Time Series (Event Streams)

Generate strictly-increasing timestamps with random inter-arrival times,
simulating event logs, page views, sensor readings, and other irregular
time series data (spec §9.3).

```toml
[[entities.fields]]
name = "timestamp"
data_type = "datetime"
generator = { type = "event_stream",
    start = "2024-01-01T00:00:00Z",
    arrival = { distribution = "exponential", params = { lambda = 0.1 }, unit = "second" },
    components = [
        { type = "seasonality", period = "24h", amplitude = 0.6 },
        { type = "weekend_effect", multiplier = 0.4 },
        { type = "business_hours", active_hours = [8, 22], active_multiplier = 3.0 },
    ]
}
```

**Parameters:**

| Parameter | Type | Description |
|-----------|------|-------------|
| `start` | string | ISO-8601 start time for the event stream |
| `arrival.distribution` | string | Inter-arrival distribution (currently `"exponential"`) |
| `arrival.params.lambda` | float | Base arrival rate (events per unit time) |
| `arrival.unit` | string | Time unit: `"second"`, `"minute"`, `"hour"`, `"day"` |
| `components` | array | Optional rate-modulation components |

**Rate-modulation components** (applied via Lewis-Shedler thinning):

| Component | Parameters | Effect |
|-----------|-----------|--------|
| `seasonality` | `period`, `amplitude` | Sinusoidal rate variation (e.g. daily cycles) |
| `weekend_effect` | `multiplier` | Scale rate on weekends (e.g. 0.4 = 40% of weekday rate) |
| `business_hours` | `active_hours`, `active_multiplier` | Concentrate events during active hours |

**Notes:**

- Produces `Timestamp(Millisecond)` values, always strictly increasing
- Stateful across batches — forces sequential partition execution
- With no components, generates a pure exponential (Poisson) process
- Seasonality `amplitude` should be in (0, 1) to keep the rate positive
- `business_hours` and `weekend_effect` currently evaluate in UTC; timezone-aware
  modulation is planned for a future release
- See `examples/event_stream.weave.toml` for a complete example

### Other Generators

Knit also includes these generators:

| Generator | Purpose | Status |
|-----------|---------|--------|
| `faker` | Locale-aware realistic data (names, addresses, companies, finance, etc.) | ✅ Working |
| `composite` | Generate arrays of values | ✅ Working |
| `conditional` | Choose generator based on another field's value | ✅ Working |
| `unique` | Wrapper enforcing uniqueness on any generator | ✅ Working |
| `relative` | Datetime offset from another field | ✅ Working |
| `business_hours` | Constrained to business hours with timezone awareness | ✅ Working |

**Faker method reference** — All supported `method` values for `type = "faker"`:

| Category | Methods |
|----------|---------|
| Person | `first_name`, `last_name`, `full_name`/`name`, `username`, `prefix`, `suffix` |
| Internet | `email`, `url`, `domain`, `ipv4`/`ip_address`, `ipv6`, `mac_address`, `user_agent` |
| Address | `address`/`street_address`, `city`/`city_name`, `state`, `country`, `country_code`, `zip_code` |
| Company | `company`, `industry`, `catch_phrase`, `bs` |
| Finance | `credit_card` (Luhn-valid), `iban` (valid check digits), `bic`/`swift`, `currency_code` |
| Phone | `phone` |
| Lorem | `word`, `sentence`, `paragraph`, `title` |
| Datetime | `date`, `datetime`, `time`, `month`, `day_of_week`, `timezone` |
| Color | `color`, `hex_color` |
| File | `file_extension`, `mime_type`, `file_name`, `file_path` |
| Geo | `latitude`, `longitude`, `coordinate` |
| Vehicle | `license_plate`, `vin`, `vehicle_make`, `vehicle_model` |
| Medical | `blood_type` |
| Barcode | `ean13`, `isbn13` |
| Product | `product_name`/`product` |
| Other | `hex_string` |

Dotted provider names are also supported: `internet.email`, `finance.credit_card`, etc.

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

Compute entity count from model parameters using expressions:

```toml
[model]
name = "scalable"

[model.params]
user_count = 10000
order_scale = 5

[[entities]]
name = "users"
count = { expr = "${param.user_count}" }

[[entities]]
name = "orders"
count = { expr = "${param.user_count} * ${param.order_scale}" }
```

Expressions support arithmetic (`+`, `-`, `*`, `/`, `%`), parameter references
(`${param.name}`), numeric literals, and pure functions like `min()`, `max()`,
`abs()`, `ceil()`, `floor()`, `round()`, and `clamp()`. Field references,
`row_number()`, and random functions are not allowed in count expressions.

Float results are rounded to the nearest integer. Zero or negative results
produce an error.

When used with `--count`, the expression is evaluated first, then the override
scale is applied. For example, `--count 0.5x` halves the computed count.

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
kind = "one_to_many"
foreign_key = "customer_id"
```

### Relationship Kinds

| Kind | Description |
|------|-------------|
| `one_to_many` | One parent row → many child rows |
| `one_to_one` | One child row → one parent row |
| `many_to_many` | Many-to-many via junction entity |

### Self-Referential Relationships (Hierarchies)

Entities can reference themselves — useful for org charts, categories, etc.:

```toml
[[relationships]]
name = "employee_manager"
from = "employees"
to = "employees"
kind = "one_to_many"
foreign_key = "manager_id"
nullable = true
root_probability = 0.05
max_depth = 6
acyclic = true
```

- **`root_probability`** — Chance a record has no parent (top-level nodes)
- **`max_depth`** — Maximum hierarchy depth
- **`acyclic`** — Prevent circular references

### Degree Distribution

Control how children are distributed across parents. Without `degree`, children
are assigned uniformly at random. With a Zipf distribution, some parents receive
disproportionately more children (power-law behavior):

```toml
[[relationships]]
name = "order_customer"
from = "orders"
to = "customers"
kind = "many_to_one"
foreign_key = "customer_id"

[relationships.degree]
kind = "zipf"
[relationships.degree.params]
exponent = 1.2
```

Higher `exponent` values produce stronger skew — a few parents attract most children.

### Selection Strategy

Control *how* a child picks its parent. The `selection` field is mutually
exclusive with `degree` — use one or the other.

**Uniform** (default — no need to specify):

```toml
selection = "uniform"
```

**Sequential** — deterministic round-robin assignment:

```toml
[[relationships]]
name = "employee_dept"
from = "employees"
to = "departments"
kind = "many_to_one"
foreign_key = "department_id"
selection = "sequential"
```

With 100 employees and 10 departments, each department gets exactly 10
employees. Assignment is based on child row position, making it fully
deterministic regardless of parallelism.

**Clustered** — locality-based grouping:

```toml
[[relationships]]
name = "task_assignee"
from = "tasks"
to = "employees"
kind = "many_to_one"
foreign_key = "assignee_id"

[relationships.selection]
strategy = "clustered"
cluster_size = 20
```

Consecutive child rows tend to reference the same window of parents. The
`cluster_size` controls the window width — smaller values produce tighter
clustering.

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
[[noise]]
name = "email_typos"
entity = "users"
fields = ["email"]
typo_rate = 0.02

[[noise]]
name = "amount_outliers"
entity = "orders"
fields = ["amount"]
outlier_rate = 0.01
```

### Scoped Noise

Restrict noise injection to rows matching a predicate expression. Only
matching rows are eligible for perturbation — probability is applied *after*
scope filtering (the two filters multiply).

```toml
# Only inject outliers in refunded orders
[[noise]]
name = "refund_outliers"
entity = "order"
fields = ["amount"]
outlier_rate = 0.5
scope = { where = '${status} == "refunded"' }

# Inject typos only when status is cancelled
[[noise]]
name = "cancel_typos"
entity = "order"
fields = ["customer_name"]
typo_rate = 0.3
scope = { where = '${status} == "cancelled"' }
```

Scope predicates use the **Knit expression language** (same syntax as derived
fields): `${field}` references, comparison operators (`==`, `!=`, `<`, `>`,
`<=`, `>=`), boolean connectives (`&&`, `||`, `!`), and functions.

| Property | Description |
|----------|-------------|
| `scope.where` | Predicate expression; rows where it evaluates to `true` are eligible |

Null predicate results are treated as `false` (row excluded from scope).

See `examples/scoped_noise.weave.toml` for a complete working example.

---

## Custom Types

Define reusable type aliases to avoid repeating the same data type, generator,
precision, and nullable settings across many fields. Custom types are defined
in the `[[types]]` section and referenced by name in field `data_type`:

```toml
# Define a "money" type — float with 2 decimals and a log-normal generator
[[types]]
name = "money"
description = "Monetary amount with 2 decimal places"
base = "float"
precision = 2
[types.generator]
type = "distribution"
kind = "log_normal"
params = { mu = 4.0, sigma = 0.8 }

# Define an "email_address" type — string with faker email generator
[[types]]
name = "email_address"
base = "string"
[types.generator]
type = "faker"
method = "email"
```

### Using Custom Types

Reference a custom type by name in any field's `data_type`:

```toml
[[entities.fields]]
name = "subtotal"
data_type = "money"       # inherits float, precision=2, log_normal generator

[[entities.fields]]
name = "contact_email"
data_type = "email_address"  # inherits string + faker email
```

### Overriding Inherited Properties

Field-level settings take precedence over the custom type defaults:

```toml
[[entities.fields]]
name = "shipping"
data_type = "money"        # inherits float + precision=2
precision = 4              # override: 4 decimal places instead of 2
[entities.fields.generator]
type = "distribution"
kind = "uniform"           # override: uniform instead of log_normal
params = { min = 0.0, max = 25.0 }
```

### Custom Type Properties

| Property | Required | Description |
|----------|----------|-------------|
| `name` | yes | Unique name (must not shadow built-in types) |
| `base` | yes | Underlying data type (any primitive: `int`, `float`, `string`, etc.) |
| `description` | no | Documentation string |
| `generator` | no | Default generator inherited by fields |
| `precision` | no | Default decimal precision inherited by fields |
| `nullable` | no | Default null specification inherited by fields |

### Restrictions

- Custom type names must not conflict with built-in types (`int`, `float`,
  `string`, `bool`, `uuid`, `date`, `datetime`, etc.)
- The `base` must be a primitive type — `object`, `array`, and `map` are not
  allowed as custom type bases
- Custom types cannot reference other custom types (no chaining)
- Custom types are fully resolved at parse time — downstream code never sees
  `DataType::Custom`

See `examples/custom_types.weave.toml` for a complete working example.

---

## Mixins

Mixins define reusable groups of fields that can be included in multiple
entities. This eliminates duplication for common patterns like audit trails,
timestamps, or versioning fields.

### Defining Mixins

```toml
[[mixins]]
name = "timestamped"
description = "Standard audit trail timestamps"

[[mixins.fields]]
name = "created_at"
data_type = "datetime"

[[mixins.fields]]
name = "updated_at"
data_type = "datetime"

[[mixins]]
name = "versioned"
description = "Version tracking"

[[mixins.fields]]
name = "version"
data_type = "int"
[mixins.fields.generator]
type = "distribution"
kind = "geometric"
params = { p = 0.7 }
```

### Using Mixins

Reference mixins by name in the entity's `mixins` array:

```toml
[[entities]]
name = "order"
count = 500
mixins = ["timestamped", "versioned"]

[[entities.fields]]
name = "id"
data_type = "int"
primary_key = true
```

This produces an entity with fields: `created_at`, `updated_at` (from
`timestamped`), `version` (from `versioned`), then `id` (entity's own field).

### Override Behavior

- **Mixin fields are prepended** — they appear before entity fields in output
- **Entity fields override mixin fields** — if an entity defines a field with
  the same name as a mixin field, the entity's definition wins
- **Mixin-vs-mixin collisions are errors** — if two mixins in the same entity
  define a field with the same name, resolution fails with a clear error
- **Mixins work with custom types** — mixin fields can use custom type
  references (mixins are resolved before custom types)

```toml
# Entity overrides the mixin's created_at with its own definition
[[entities]]
name = "order"
mixins = ["timestamped"]

[[entities.fields]]
name = "created_at"
data_type = "string"  # overrides mixin's datetime
```

See `examples/mixins.weave.toml` for a complete working example.

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
