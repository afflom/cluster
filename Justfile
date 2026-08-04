# `just vv` is the normative acceptance gate. Everything else is a slice of it.

default: vv

# The whole gate.
vv: fmt-check model lint test features bdd
    @echo "vv: the acceptance gate passed"

# R1, R4, R5 --- the repository gates, each falsifiable. `validate` runs
# check-model, check-render, audit-limits and audit-deferral in that order.
model:
    cargo run -q -p xtask -- validate

# Regenerate everything the model owns: CONFORMANCE.md.
model-write:
    cargo run -q -p xtask -- check-model --write

# R1 over infrastructure: rewrite generated/ from model/. The tree is committed
# and diff-gated, so a hand-edited `.network` file is the same class of error as
# a hand-edited CONFORMANCE.md --- and `cargo xtask check-render` says so.
render:
    cargo run -q -p xtask -- render

fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all -- --check

lint:
    cargo clippy --workspace --all-targets -- -D warnings
    # `--all-targets` means every target with `test = true`, so the three tier
    # tests --- marked `test = false` precisely so `cargo test` cannot pretend
    # to have booted a guest --- were compiled by nothing. A type error in
    # `tests/t1.rs` passed the whole gate and surfaced only when a runner tried
    # to boot something, which is the most expensive place to learn it. Naming
    # them compiles and lints them; it still never runs them.
    cargo clippy -p cluster-harness --test t1 --test t2 --test t3 -- -D warnings

test:
    cargo test --workspace

# A feature only its author has built is a feature that does not work: nothing
# else in the gate compiles a crate at anything but its default features, so a
# rename upstream of an optional dependency fails nowhere until someone turns
# the flag on. `--all-targets` because the tests behind a flag are code too.
#
# Every optional feature compiles, with its tests --- and so does the
# configuration with none of them. `--all-features` only ever tests the union,
# so `cluster-ctl` without `server` --- the shape `cluster-web` links for wasm32
# --- would compile nowhere in the gate and break silently the first time a
# server type leaked into the wire module.
features:
    cargo check --workspace --all-features --all-targets
    cargo check -p cluster-ctl --no-default-features
    cargo check -p cluster-web --target wasm32-unknown-unknown

# R3: every capability begins as a Gherkin scenario, and every scenario has a
# test whose name ends in its ID.
bdd:
    cargo test -p repo-conformance

# The guest and hardware tiers (SPEC.md §10.2). Deliberately not part of `vv`:
# T2 takes ~35 minutes and T3 needs the real fleet, and a gate a developer will
# not run is a gate that gets bypassed. The workflows run them; VERIFICATION.md
# records which discharges what.
#
# A tier that cannot boot a guest exits non-zero with an explicit skip rather
# than falling back to TCG (§9.4).
#
# The driver decides whether a tier can run and exits 3 if it cannot. That exit
# is what stops the assertions from running at all --- a tier whose tests
# reported `ok` having booted nothing would be the vacuous gate in its most
# convincing disguise.
t1:
    @just _tier t1

t2:
    @just _tier t2

t3:
    @just _tier t3

# Run one tier: probe the fixture, then assert. Exit 3 means the tier did not
# run and nothing was tested; the workflow reports that as a skip, never a pass.
_tier tier:
    #!/usr/bin/env bash
    set -uo pipefail
    cargo run -q -p cluster-harness -- {{tier}}
    code=$?
    if [ "$code" -eq 3 ]; then
      echo "{{tier}}: did not run. Nothing was tested." >&2
      exit 3
    fi
    [ "$code" -eq 0 ] || exit "$code"
    cargo test -p cluster-harness --test {{tier}} -- --test-threads=1 --nocapture

# R6: nothing shipped depends on a dev-only crate, no wildcard version
# requirement, no advisory against anything in the tree. Needs
# `cargo install cargo-deny`, which is why it is not in `just vv`.
#
# Advisories, bans, licences and sources, over the dependency graph.
deny:
    cargo deny --all-features check
