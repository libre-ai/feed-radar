#!/usr/bin/env bash
# Fails when the JWT crypto provider is missing from the PRODUCTION build graph.
#
# `jsonwebtoken` 10.x ships no crypto backend by default. `sign` and `verify`
# both resolve one through `CryptoProvider::get_default()`, which falls back to
# `CryptoProvider::from_crate_features()` and panics when the feature set does
# not name exactly one backend. Nothing about that is visible at compile time:
# `cargo check`, `cargo clippy` and `cargo build` all succeed, and the process
# aborts on the first token it signs or verifies.
#
# Two distinct ways to get this wrong, both silent, both gated here:
#
#   A. No provider at all — the feature is simply absent.
#   B. A provider that never reaches production — declared under
#      `[dev-dependencies]`. Under resolver "2" those features do not flow into
#      `cargo build`, so the test suite passes while the shipped binary keeps
#      panicking. This is why the check reads `--edges normal`: it deliberately
#      ignores dev-only edges, which is exactly what a `cargo test` run cannot
#      do for itself.
#   C. Both providers at once — `from_crate_features` matches on
#      `all(feature = "rust_crypto", not(feature = "aws_lc_rs"))` and its mirror,
#      so enabling both falls through to the same panic as enabling neither.
#
# This gate reads the dependency graph and never compiles the workspace. The
# companion proof that the provider actually works is the round-trip test in
# `crates/api/src/routes/auth.rs`, which signs and verifies a real token.

set -euo pipefail

echo "== JWT crypto provider =="

# Direct, normal-edge dependencies of `jsonwebtoken` as cargo resolves them for
# a production build of the workspace. `--edges normal` excludes dev and build
# edges; `--depth 1` keeps only the crate's own dependencies.
tree_output=$(cargo tree --package jsonwebtoken --edges normal --depth 1)

# `aws-lc-rs` is the sole marker of the `aws_lc_rs` feature. `hmac` is optional
# in `jsonwebtoken` and pulled only by `rust_crypto`, so it marks that feature
# without depending on which algorithms the application happens to use.
has_aws_lc_rs=0
has_rust_crypto=0
grep -qE '(^|[^a-z-])aws-lc-rs v' <<<"$tree_output" && has_aws_lc_rs=1
grep -qE '(^|[^a-z-])hmac v' <<<"$tree_output" && has_rust_crypto=1

providers=$((has_aws_lc_rs + has_rust_crypto))

if [ "$providers" -eq 0 ]; then
  echo
  echo "FAIL: jsonwebtoken carries no crypto provider in the production graph."
  echo
  echo "Every sign and verify will panic at runtime with"
  echo "\"Could not automatically determine the process-level CryptoProvider\"."
  echo "A provider declared under [dev-dependencies] does NOT count: resolver"
  echo "\"2\" keeps it out of \`cargo build\`."
  echo
  echo "Enable exactly one of 'rust_crypto' or 'aws_lc_rs' on the workspace"
  echo "\`jsonwebtoken\` dependency in Cargo.toml. See docs/adr/0005."
  echo
  echo "Direct normal-edge dependencies seen:"
  echo "$tree_output"
  exit 1
fi

if [ "$providers" -gt 1 ]; then
  echo
  echo "FAIL: jsonwebtoken carries more than one crypto provider."
  echo
  echo "'rust_crypto' and 'aws_lc_rs' are mutually exclusive: the crate selects"
  echo "a backend with \`all(feature = \"rust_crypto\", not(feature = \"aws_lc_rs\"))\`"
  echo "and its mirror, so enabling both resolves to neither and panics exactly"
  echo "as enabling none would."
  echo
  echo "Direct normal-edge dependencies seen:"
  echo "$tree_output"
  exit 1
fi

if [ "$has_rust_crypto" -eq 1 ]; then
  echo "   provider: rust_crypto (RustCrypto backend)"
  echo "   note: this is NOT the backend retained in docs/adr/0005. It pulls"
  echo "         'rsa' and with it RUSTSEC-2023-0071, which has no upstream fix,"
  echo "         so it needs a deny.toml waiver that 'aws_lc_rs' does not."
else
  echo "   provider: aws_lc_rs (aws-lc-rs backend, already built for rustls)"
fi

echo
echo "PASS: exactly one JWT crypto provider reaches the production build."
