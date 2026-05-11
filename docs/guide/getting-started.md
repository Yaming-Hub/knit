# Getting Started with Knit

This tutorial walks you through installing Knit, writing your first blueprint,
and generating synthetic data — all in under 10 minutes.

**[← Back to User Guide](index.md)**

---

## Installation

### Build from Source

```bash
# Clone the repository
git clone https://github.com/Yaming-Hub/knit.git
cd knit

# Build the release binary
cargo build --release

# The binary is at target/release/knit
# Optionally, add it to your PATH:
# Linux/macOS:
export PATH="$PWD/target/release:$PATH"
# Windows (PowerShell):
$env:PATH = "$PWD\target\release;$env:PATH"
```

Verify the installation:

```bash
knit --version
```

---

## Your First Blueprint

A knit blueprint is a `.knit.toml` file that declares your data model: entities
(tables), fields (columns), and how to generate values for each field.

### Step 1: Create the Blueprint File

Create a file called `my_first.knit.toml`:

```toml
blueprint_version = "1.0"

[model]
name = "my_first_dataset"
seed = 42

[[entities]]
name = "customers"
count = 100

[[entities.fields]]
name = "id"
data_type = "int"
primary_key = true
[entities.fields.generator]
type = "sequence"
start = 1
step = 1

[[entities.fields]]
name = "email"
data_type = "string"
[entities.fields.generator]
type = "one_of"
choices = [
  { value = "alice@example.com" },
  { value = "bob@example.com" },
  { value = "carol@example.com" },
  { value = "dave@example.com" },
  { value = "eve@example.com" },
]

[[entities.fields]]
name = "age"
data_type = "int"
[entities.fields.generator]
type = "distribution"
kind = "normal"
[entities.fields.generator.params]
mean = 35.0
std_dev = 10.0
```

Let's break down what each section does:

- **`blueprint_version`** — Declares the blueprint format version.
- **`[model]`** — Names your dataset and sets a seed for deterministic output.
- **`[[entities]]`** — Defines a table called `customers` with 100 rows.
- **`[[entities.fields]]`** — Defines columns. Each field has a `name`,
  `data_type`, and a `[generator]` that controls how values are produced.

### Step 2: Validate the Blueprint

Before generating, check that your blueprint is well-formed:

```bash
knit validate my_first.knit.toml
```

Expected output for a valid blueprint:

```
✓ Blueprint is valid (1 entity, 3 fields)
```

If there are errors, Knit reports the exact location and a suggestion:

```
error[E0301]: unknown generator type "sequnce"
  --> my_first.knit.toml:12:8
   |
   = help: did you mean "sequence"?
```

### Step 3: Preview the Execution Plan

See what Knit will do without generating any data:

```bash
knit plan my_first.knit.toml
```

This shows the entity ordering, row counts, generator assignments, and
estimated output size.

### Step 4: Generate Data

Generate the data in Parquet format (the default):

```bash
knit generate my_first.knit.toml --output ./output
```

You'll see a progress bar:

```
customers  [████████████████████████████████] 100/100  done
✓ Generated 1 entity (100 rows) in 0.02s
```

### Step 5: Examine the Output

The output directory contains one file per entity:

```
output/
└── customers.parquet
```

To generate CSV instead:

```bash
knit generate my_first.knit.toml --output ./output --format csv
```

Or JSON:

```bash
knit generate my_first.knit.toml --output ./output --format json
```

### Step 6: Change the Seed

The `seed` in the blueprint ensures deterministic output — the same seed always
produces identical data. Override it from the command line to get different data:

```bash
# Different seed → different data
knit generate my_first.knit.toml --output ./output --seed 123
```

---

## Adding a Second Entity with a Relationship

Real datasets have multiple related tables. Let's add an `orders` entity that
references `customers`:

```toml
blueprint_version = "1.0"

[model]
name = "my_first_dataset"
seed = 42

# ── Customers ──────────────────────────────────────────────
[[entities]]
name = "customers"
count = 100

[[entities.fields]]
name = "id"
data_type = "int"
primary_key = true
[entities.fields.generator]
type = "sequence"
start = 1
step = 1

[[entities.fields]]
name = "email"
data_type = "string"
[entities.fields.generator]
type = "one_of"
choices = [
  { value = "alice@example.com" },
  { value = "bob@example.com" },
  { value = "carol@example.com" },
  { value = "dave@example.com" },
  { value = "eve@example.com" },
]

[[entities.fields]]
name = "age"
data_type = "int"
[entities.fields.generator]
type = "distribution"
kind = "normal"
[entities.fields.generator.params]
mean = 35.0
std_dev = 10.0

# ── Orders─────────────────────────────────────────────────
[[entities]]
name = "orders"
count = 500

[[entities.fields]]
name = "id"
data_type = "int"
primary_key = true
[entities.fields.generator]
type = "sequence"
start = 1
step = 1

[[entities.fields]]
name = "customer_id"
data_type = "int"
[entities.fields.generator]
type = "lookup"
entity = "customers"
field = "id"

[[entities.fields]]
name = "amount"
data_type = "float"
[entities.fields.generator]
type = "distribution"
kind = "log_normal"
[entities.fields.generator.params]
mu = 3.5
sigma = 1.0

[[entities.fields]]
name = "status"
data_type = "string"
[entities.fields.generator]
type = "one_of"
choices = [
  { value = "pending",   weight = 10 },
  { value = "shipped",   weight = 50 },
  { value = "delivered", weight = 35 },
  { value = "cancelled", weight = 5 },
]

# ── Relationships ──────────────────────────────────────────
[[relationships]]
name = "order_customer"
from = "orders"
to = "customers"
kind = "one_to_many"
foreign_key = "customer_id"
```

Generate the two-table dataset:

```bash
knit generate my_first.knit.toml --output ./output --format csv
```

Output:

```
output/
├── customers.csv
└── orders.csv
```

Every `customer_id` in `orders.csv` points to a valid `id` in `customers.csv`
— Knit guarantees referential integrity automatically.

---

## What's Next?

- **[Blueprint Language Tutorial](blueprint-language.md)** — Deep dive into all
  generators, data types, relationships, and advanced features
- **[CLI Reference](cli-reference.md)** — Every command and flag
- **[Examples Walkthrough](examples.md)** — Learn from the five bundled blueprints
- **[Noise Injection Guide](noise.md)** — Add realistic data quality issues
