# Design: Dataset Tokenization (`knit tokenize`)

## 1. Motivation

When users generate data with knit and encounter issues, troubleshooting requires
inspecting their dataset. However, datasets often contain sensitive or proprietary
content — customer names, internal codes, business terms — that users cannot share.

`knit tokenize` solves this by replacing all meaningful string content with opaque
tokens while **preserving dataset structure, relationships, and statistical
properties**. The result is a structurally identical dataset that is safe to share
for debugging, with an optional token dictionary that can restore the original data.

**Key properties:**
- A tokenized dataset produces the same blueprint when learned by knit
- All foreign-key relationships, null patterns, and cardinalities are preserved
- No original string content is recoverable without the dictionary
- Numeric values, dates, and booleans are optionally preserved or tokenized

---

## 2. User Experience

### 2.1 Tokenize a Dataset

```bash
# Tokenize a dataset directory (same layout knit learns from)
knit tokenize Q:\data\my_dataset -o Q:\data\tokenized

# Output:
# ═══ Tokenization Complete ═══
#   files:       14 data files, 3 schema files, 27 dictionary files
#   tokens:      2,847 unique string values → tokenized
#   numeric:     preserved (use --tokenize-numbers to obfuscate)
#   dictionary:  Q:\data\tokenized\.knit-tokens.json (DO NOT share if you want privacy)
#
# The tokenized dataset at Q:\data\tokenized/ is safe to share.
# Keep .knit-tokens.json private — it can restore the original data.
```

### 2.2 Restore from Tokens

```bash
# Restore original data using the token dictionary
knit tokenize --restore Q:\data\tokenized -o Q:\data\restored \
    --dictionary Q:\data\tokenized\.knit-tokens.json
```

### 2.3 Verify Tokenization

```bash
# Verify that tokenized dataset has the same structure as original
knit tokenize --verify Q:\data\my_dataset Q:\data\tokenized

# Output:
# ═══ Verification ═══
#   blueprint match:      ✓ (same entities, fields, types)
#   row counts:        ✓ (identical)
#   null patterns:     ✓ (identical)
#   relationships:     ✓ (FK integrity preserved)
#   content overlap:   0 strings in common (fully tokenized)
```

---

## 3. Tokenization Rules

### 3.1 What Gets Tokenized

| Content Type | Default | Flag to Override |
|-------------|---------|-----------------|
| String cell values | **Tokenized** | `--preserve-strings` (skip) |
| String values in schema.json | **Tokenized** | — |
| Dictionary file entries | **Tokenized** | — |
| Column headers / field names | **Preserved** | `--tokenize-headers` (tokenize) |
| File names and paths | **Preserved** | `--tokenize-paths` |
| Integer values | **Preserved** | `--tokenize-numbers` (tokenize) |
| Float values | **Preserved** | `--tokenize-numbers` (tokenize) |
| Date/timestamp values | **Preserved** | `--tokenize-dates` (shift by random offset) |
| Boolean values | **Preserved** | — (no useful info) |
| Null values | **Preserved** | — (structural) |
| Partition folder names | **Tokenized** (if string) | `--preserve-partitions` |
| Partition dates | **Preserved** | `--tokenize-dates` |

### 3.2 Tokenization Modes

**String tokenization (default):** Each unique string value maps to a random
token of the same "shape":

| Original | Token | Rule |
|----------|-------|------|
| `"John Smith"` | `"xkqm bvrl"` | Same word count, similar word lengths |
| `"john.smith@contoso.com"` | `"rqpx.wvnz@tknmbc.com"` | Email structure preserved |
| `"US"` | `"JK"` | Same length, same case pattern |
| `""` | `""` | Empty stays empty |
| `"Meeting"` | `"Ghwqpxr"` | Same length, capitalization pattern |

**Why preserve shape:** The token shape (length, word count, case pattern)
preserves statistical properties that knit's learn command uses — a column of
2-letter codes will still look like 2-letter codes, enabling correct generator
inference.

**Consistency:** The same original value always maps to the same token within
a tokenization run. This preserves:
- Foreign-key relationships (if `"US"` appears in both tables, both become `"JK"`)
- Conditional generator patterns (if SignalType `"Meeting"` maps to `"Ghwqpxr"`,
  all Meeting-conditional columns tokenize consistently)
- Dictionary lookups (dictionary keys and data values use the same mapping)

### 3.3 File-Type-Aware Processing

knit already classifies files during learn. Tokenization reuses this:

| File Type | How Tokenized |
|-----------|--------------|
| **Data files** (CSV, Parquet, JSON) | Cell-by-cell string replacement |
| **Schema files** (schema.json) | Tokenize display names, descriptions; preserve field types, structure |
| **Dictionary files** (Mappings/*.csv) | Tokenize both keys and values; preserve row count and structure |
| **Other companion files** | Copied unchanged (not data) |

**Schema files require special handling:**
- Field names/column names: preserved (structural)
- `tableName`, `description`: tokenized (may contain sensitive naming)
- `colNumber`, `dataType`, `rowType`: preserved (structural metadata)
- Dictionary file references: path structure preserved, file content tokenized

**Dictionary files:**
- The key column (typically ID) and value column (display name) are both tokenized
- Row count preserved exactly
- The tokenized dictionary remains internally consistent with tokenized data files

---

## 4. Token Dictionary Format

The token dictionary is a JSON file mapping original values to tokens:

```json
{
  "version": 1,
  "created": "2026-05-10T12:00:00Z",
  "seed": 42,
  "stats": {
    "unique_tokens": 2847,
    "files_processed": 44
  },
  "tokens": {
    "John Smith": "xkqm bvrl",
    "jane.doe@contoso.com": "rqpx.wvnz@tknmbc.com",
    "US": "JK",
    "Meeting": "Ghwqpxr",
    ...
  },
  "numbers": {},
  "dates": {}
}
```

**Security considerations:**
- The dictionary is the **only** artifact that can reverse tokenization
- It is written to the output directory by default but can be redirected:
  `--dictionary /secure/path/tokens.json`
- Users are warned not to share it alongside the tokenized data
- Without the dictionary, tokenization is a one-way mapping

---

## 5. Implementation Architecture

### 5.1 Pipeline

```
scan(dataset) → build_token_map → apply_tokens → write_output + dictionary
```

**Phase 1: Scan** — Walk the dataset directory, classify files (data, schema,
dictionary, companion), detect formats, read all string values. When
`--tokenize-headers` is enabled, also registers CSV column headers, JSON object
keys, and Parquet column names in the token map. When `--tokenize-numbers` is
enabled, numeric cell values are registered. When `--tokenize-dates` is enabled,
date/timestamp strings are detected and registered with shifted values (using a
consistent random offset derived from the seed). Date strings are preserved
(skipped) by default — only shifted when the flag is explicitly set.

**Phase 2: Build Token Map** — For each unique string value, generate a token
preserving shape (length, word count, case pattern). Use a seeded RNG for
deterministic token generation. Headers and numbers share the same token map.
Dates use consistent shifting (same offset for all dates) to preserve ordering.

**Phase 3: Apply** — Re-read each file, replace string values using the token
map, write to output directory preserving folder structure. When
`--tokenize-headers` is enabled, CSV headers are replaced before writing,
JSON object keys are rewritten with tokenized names, and Parquet schemas are
rebuilt with tokenized column names.

**Phase 4: Emit Dictionary** — Write the token map as JSON.

### 5.2 Token Generation Algorithm

```rust
fn generate_token(original: &str, rng: &mut impl Rng) -> String {
    // Preserve structure: split into "segments" by detected separators
    // For each segment, generate random chars matching:
    //   - length
    //   - case pattern (upper, lower, mixed, numeric)
    //   - character class (alpha, alphanumeric, numeric-only)
    // Reassemble with original separators
}
```

**Separator detection:** Split on `@`, `.`, `-`, `_`, `/`, ` `, `,` and preserve
the separator sequence. This keeps email-like strings looking like emails, paths
looking like paths, etc.

**Character class preservation:**
| Original Char | Token Char Pool |
|--------------|----------------|
| `A-Z` | Random `A-Z` |
| `a-z` | Random `a-z` |
| `0-9` | Random `0-9` |
| Unicode letter (uppercase) | Random `A-Z` |
| Unicode letter (lowercase) | Random `a-z` |
| Unicode digit | Random `0-9` |
| Other (punctuation, symbols) | Preserved as-is |

### 5.3 File Organization

```
src/cli/commands/tokenize.rs  — CLI command, progress reporting
src/tokenize/mod.rs           — Orchestration, file classification reuse
src/tokenize/scanner.rs       — Dataset scanning, string extraction
src/tokenize/mapper.rs        — Token map builder, shape-preserving generation
src/tokenize/apply.rs         — File rewriting (CSV, Parquet, JSON, schema)
src/tokenize/dictionary.rs    — Token dictionary I/O (write/read JSON)
```

---

## 6. CLI Specification

```
knit tokenize <INPUT> [OPTIONS]

Arguments:
    <INPUT>                   Path to dataset directory to tokenize

Modes:
    (default)                 Tokenize the dataset
    --restore                 Restore tokenized dataset to original using dictionary
    --verify <ORIGINAL>       Verify tokenized dataset matches original structure

Options:
    -o, --output <DIR>        Output directory (required)
    --dictionary <PATH>       Token dictionary path (default: <output>/.knit-tokens.json)
    --seed <N>                Random seed for token generation (deterministic)
    --tokenize-numbers        Also tokenize numeric values
    --tokenize-dates          Also tokenize date/timestamp values
    --tokenize-headers        Also tokenize column headers
    --tokenize-paths          Also tokenize file/folder names
    --preserve-partitions     Keep partition folder values as-is
    --quiet                   Suppress progress output
    --json                    Machine-readable JSON output
```

---

## 7. Edge Cases and Error Handling

| Case | Behavior |
|------|----------|
| Empty string values | Preserved as empty (not tokenized) |
| Null values | Preserved as null |
| Very long strings (>1000 chars) | Tokenized with truncated shape matching (cap at 200 chars) |
| Binary/non-UTF8 content | Skipped with warning |
| Duplicate values across files | Same token (global consistency) |
| Parquet files with nested types | Flatten to leaf strings for tokenization |
| Compressed files (.gz, .snappy) | Decompress → tokenize → recompress |
| Mixed formats in one dataset | Each format handled by its reader/writer |
| Token collision (two originals → same token) | Retry with different seed; extremely unlikely with sufficient entropy |
| `--restore` without dictionary | Error: "Token dictionary required for restore" |
| `--restore` with partial dictionary | Warning per missing token; leave as-is |

---

## 8. Relationship Preservation Guarantees

Tokenization must preserve these properties for knit troubleshooting:

| Property | Preserved? | How |
|----------|-----------|-----|
| Entity count and names | ✓ | File/folder structure unchanged |
| Row counts per entity | ✓ | 1:1 row mapping |
| Column count and names | ✓ | Headers preserved by default |
| Column types (int/string/date) | ✓ | Only strings replaced |
| Null patterns | ✓ | Nulls pass through unchanged |
| FK relationships | ✓ | Same original → same token globally |
| Conditional patterns | ✓ | Discriminator values tokenized consistently |
| Value distributions | ~✓ | Cardinality preserved; shape approximated |
| Partition structure | ✓ | Folder hierarchy preserved |
| Schema metadata | ✓ | Structural fields preserved |
| Dictionary structure | ✓ | Key-value mapping preserved |

---

## 9. Future Work

### 9.1 Differential Privacy Integration
Add noise to numeric values during tokenization to provide formal privacy
guarantees (ε-differential privacy).

### 9.2 Selective Tokenization
Allow users to specify which columns to tokenize vs preserve:
`--tokenize-columns "Name,Email,Address" --preserve-columns "Country,Status"`

### 9.3 Format Conversion During Tokenization
Convert between formats while tokenizing: tokenize a Parquet dataset and
output as CSV (or vice versa) for easier inspection.

### 9.4 Streaming Tokenization
Process files in streaming fashion for datasets too large to scan in memory.
Build the token map incrementally with a two-pass approach.

### 9.5 Tokenization Report
Generate an HTML/markdown report showing tokenization coverage, value
distribution changes, and structural integrity verification.

---

## 10. Implementation Status

### v1 (Implemented)

| Feature | Status | Notes |
|---------|--------|-------|
| String tokenization (CSV) | ✅ | Cell-level replacement via `csv` crate |
| String tokenization (JSON/JSONL) | ✅ | Value-level replacement, line-by-line JSONL |
| String tokenization (Parquet) | ✅ | Columnar replacement via Arrow |
| Shape-preserving tokens | ✅ | Length, case, separators, char class preserved |
| Unicode letter replacement | ✅ | Non-ASCII letters → random ASCII letters |
| Global consistency | ✅ | Same value → same token across all files |
| Schema-aware tokenization | ✅ | Only data payloads, not structural fields |
| File classification | ✅ | Data, schema, dictionary, companion |
| Collision avoidance | ✅ | 1000 retries + length-varied fallback |
| Token dictionary I/O | ✅ | .knit-tokens.json with BTreeMap sorted output |
| Restore mode (`--restore`) | ✅ | Reverse map from dictionary |
| Verify mode (`--verify`) | ✅ | Structural equivalence check |
| Deterministic (`--seed`) | ✅ | Seeded StdRng for reproducibility |

**Architecture (as implemented):**

```
src/tokenize/mod.rs         — Pipeline orchestration (tokenize, restore entry points)
src/tokenize/scanner.rs     — Directory walking, file classification, string extraction
src/tokenize/mapper.rs      — Shape-preserving token generation, collision avoidance
src/tokenize/apply.rs       — File rewriting (CSV, JSON/JSONL, Parquet, schema-selective)
src/tokenize/dictionary.rs  — Token dictionary serialization (.knit-tokens.json)
src/cli/commands/tokenize.rs — CLI handler (tokenize/restore/verify modes)
```

### Known Limitations (v1)

| Limitation | Impact | Mitigation |
|-----------|--------|-----------|
| Native Parquet timestamp columns not shifted | Only string-encoded dates shifted | Warning could be added |
| Native Parquet numeric columns not tokenized | Only string-encoded numbers replaced | Warning emitted at runtime |
| Restore ambiguity | A generated token may match an untouched literal | Rare in practice; requires field-level metadata to fix fully |
| Memory usage | All unique strings held in HashMap | Acceptable for datasets < 10M unique strings |

### Deferred to v2

- Native Parquet timestamp column shifting (typed Date32/Date64/Timestamp replacement)
- Native Parquet numeric column tokenization (typed i32/i64/f32/f64 replacement)
- Field-level restore metadata (eliminate restore ambiguity)
- Streaming tokenization for very large datasets
- Selective column tokenization (`--tokenize-columns`, `--preserve-columns`)
