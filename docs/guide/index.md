# Knit User Guide

Welcome to the **Knit** user guide — a comprehensive, task-oriented guide for
generating realistic synthetic datasets with Knit.

## Who This Guide Is For

This guide is for **data engineers, QA engineers, and developers** who need to
generate realistic test data, seed development databases, or stress-test data
pipelines. No prior experience with Knit is required.

## Prerequisites

- **Rust 1.75+** and Cargo (for building from source), _or_ a pre-built `knit` binary
- A terminal / command line
- A text editor for writing `.weave.toml` schema files

## Guide Contents

| Page | Description |
|------|-------------|
| [Getting Started](getting-started.md) | Install Knit, create your first schema, generate data |
| [Weave Schema Language](schema-language.md) | Practical tutorial on entities, fields, generators, relationships |
| [CLI Reference](cli-reference.md) | Every command, flag, and option with examples |
| [Examples Walkthrough](examples.md) | Guided tour of the five bundled example schemas |
| [Noise Injection](noise.md) | Add realistic data quality issues for pipeline testing |
| [Reverse Engineering](learn.md) | Infer schemas from existing datasets with `knit learn` |

## Other Resources

- [README](../../README.md) — Project overview and quick start
- [Weave Language Specification](../weave-spec.md) — Formal grammar and semantics
- [Architecture](../architecture.md) — Internal system design
- [Contributing](../../CONTRIBUTING.md) — Developer guide and PR process

## Quick Navigation

**"I want to…"**

- **…generate data for the first time** → [Getting Started](getting-started.md)
- **…understand how schemas work** → [Schema Language Tutorial](schema-language.md)
- **…look up a CLI flag** → [CLI Reference](cli-reference.md)
- **…see real-world examples** → [Examples Walkthrough](examples.md)
- **…inject noise into my data** → [Noise Injection Guide](noise.md)
- **…create a schema from existing data** → [Reverse Engineering Guide](learn.md)
- **…read the formal language spec** → [Weave Specification](../weave-spec.md)
