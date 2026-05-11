# Contributing to Knit

Thank you for your interest in contributing! This guide covers the development
workflow, conventions, and expectations for pull requests.

## Prerequisites

- **Rust 1.87+** (install via [rustup](https://rustup.rs))
- **Cargo** (bundled with Rust)
- Git

## Getting Started

```bash
git clone https://github.com/Yaming-Hub/knit.git
cd knit
cargo build --workspace
```

## Development Workflow

### Build

```bash
cargo build --workspace
```

### Test

```bash
cargo test --workspace
```

### Lint

```bash
cargo clippy --workspace -- -D warnings
```

### Format

```bash
cargo fmt --all
```

### Documentation

```bash
cargo doc --workspace --no-deps --open
```

## Code Conventions

- **Doc comments** — All public structs, enums, traits, and functions must have
  `///` doc comments. Module-level `//!` docs are encouraged.
- **Logging** — Use the `tracing` crate (`tracing::info!`, `tracing::warn!`,
  etc.) for all runtime logging. Do not use `println!` except in the CLI for
  user-facing output.
- **Error handling** — Use `thiserror` for library error types and `anyhow` in
  the CLI binary. Never call `.unwrap()` on user-provided data; use proper error
  propagation.
- **Determinism** — Generators must be deterministic for a given RNG state.
  Always derive per-field RNGs from the `RngTree` to ensure reproducibility.
- **Testing** — Write unit tests in `#[cfg(test)]` modules. Integration tests
  live in `crates/knit-integration-tests/`.

## Pull Request Guidelines

- **Branch naming** — Use `feat/<topic>`, `fix/<topic>`, or `refactor/<topic>`.
- **Size limit** — Keep PRs under 2000 lines where possible. Split large
  changes into stacked PRs.
- **Test coverage** — Every new public API should have at least one test.
  Bug-fix PRs should include a regression test.
- **CI must pass** — All tests, clippy, and formatting checks must pass before
  merge.
- **Commit messages** — Use conventional commit style
  (`feat:`, `fix:`, `docs:`, `refactor:`, `test:`).

## Architecture

Knit is organised as a Cargo workspace with focused crates:

```
knit-core → knit-blueprint → knit-plan → knit-gen → knit-noise → knit-bind
                                                                    ↑
                                                               knit-cli
```

Each crate has a single responsibility. Cross-crate dependencies flow left to
right — downstream crates never depend on upstream ones.

## Reporting Issues

- Use GitHub Issues for bug reports and feature requests.
- Include the output of `knit --version` and a minimal reproducing blueprint.
