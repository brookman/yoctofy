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


## How to structure you recipe

```
SUMMARY = "Whatever"
DESCRIPTION = "A tool, whatever"
HOMEPAGE = "https://whatever.dev"
LICENSE = "MPL-2.0"
LIC_FILES_CHKSUM = "file://LICENSE;md5=65d26fcc2f35ea6a181ac777e42db1ea"

inherit cargo cargo-update-recipe-crates

SRC_URI = "git://github.com/whatever/whatever.git;protocol=https;nobranch=1"
SRCREV  = "44c8f1c3d64bdedb924041c946f639d09890fc51"

S = "${WORKDIR}/git"

# must be the same as used to generate the .inc files
CARGO_BUILD_FLAGS += " --no-default-features --features feature-a,feature-b"

# We can't use --frozen because we patch Cargo.toml, but --offline is fine
CARGO_BUILD_FLAGS:remove = " --frozen" 
CARGO_BUILD_FLAGS += " --offline"

# SPDX fixup: filter to runtime crates and add license info
require spdx-fixup.inc

...
```

## Gotchas
- The `spdx-fixup.inc` part is optional
- The .inc files try to patch git imports which are messed up from cargo-yocto. It's not perfect and sometimes needs some manual tweaking.