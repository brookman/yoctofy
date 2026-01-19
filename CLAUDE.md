# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Yoctofy is a Rust CLI tool that generates Yocto/OpenEmbedded `.inc` files from Cargo dependency graphs. It separates build-time and runtime crate dependencies for use in Yocto recipes.

## Build Commands

```bash
# Build (debug)
cargo build

# Build (release)
cargo build --release

# Run directly
cargo run -- --package <pkg_name> [options]
```

## Usage

```bash
yoctofy --package <name> [--manifest-path <path>] [--features <features>] [--no-default-features] [--out-dir <dir>]
```

Example (for Vector project):
```bash
yoctofy --manifest-path /path/to/vector/Cargo.toml --package vector --out-dir .
```

This generates:
- `<package>-crates-runtime.inc` - runtime dependencies (crates.io + git)
- `<package>-crates-build.inc` - build-only dependencies (crates.io + git)

The runtime .inc file includes a `do_compile:prepend()` function that:
1. Removes auto-generated `[patch."..."]` blocks from cargo_home/config.toml that break workspace crates
2. Rewrites git dependencies in Cargo.toml to use local paths for offline builds

## Architecture

The tool is a single-file application (`src/main.rs`) that:

1. **Invokes `cargo metadata`** to get the resolved dependency graph with features applied
2. **Loads `Cargo.lock`** to extract checksums for crates.io packages
3. **Computes two dependency closures:**
   - Runtime closure: BFS from root following only normal (non-dev, non-build) dependencies
   - Build closure: BFS from build dependencies and proc-macros following normal+build deps
4. **Outputs Yocto `.inc` files** with `SRC_URI` entries, sha256sum checksums, and git SRCREV entries

Key dependency separation logic:
- Proc-macro crates are classified as build-time (they execute at compile time)
- Build dependencies of runtime crates become build-only
- Git dependencies include SRC_URI with git://, SRCREV entries, and sed commands to rewrite Cargo.toml paths
