# Yoctofy

Yoctofy generates Yocto Linux / BitBake recipe fragments from a Rust **binary crate**.
It analyzes a Cargo project and produces dependency and license metadata suitable for
integration into a Yocto build.

## Examples

### Basic usage

```bash
cargo run -r -- \
  --package whatever \
  --manifest-path /path/to/whatever/Cargo.toml
```

This command generates the following files in the current working directory, assuming
default Cargo features and the host target triple:

* `whatever-crates-runtime.inc`
  Lists all **runtime dependencies** (crates from crates.io and Git repositories).
  Git dependencies include the necessary patch logic.
  **This file must be included in the BitBake recipe.**

* `whatever-crates-other.inc`
  Lists all **non-runtime dependencies** (e.g. build-time-only crates).
  These dependencies are still required for the build to succeed and
  **must also be included in the BitBake recipe.**

* `whatever-licenses.json`
  Optional license mapping file used for SPDX / license fixups.

### Advanced usage

```bash
cargo run -r -- \
  --package whatever \
  --manifest-path /path/to/whatever/Cargo.toml \
  --out-dir /a/specific/output/directory/  \
  --no-default-features \
  --features feature-a,feature-b \
  --target aarch64-unknown-linux-gnu
```

This generates the same set of output files, but for a custom feature set and target
triple. Restricting features and specifying the target will typically reduce the number
of runtime dependencies.
