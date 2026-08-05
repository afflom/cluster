# cluster

Three Supermicro nodes as a hypervisor substrate for OCI workloads: devcontainers,
GitHub Actions runners, and a small set of storage services. The host operating
system is an appliance, built with bootc, and the cluster's entire definition
lives here.

There is no configuration applied out of band, no node-local state that is not
either declared here or explicitly designated as data, and no path by which a
node comes to run software that did not pass the gate.

| Document | What it is |
| --- | --- |
| [`INSTALL.md`](INSTALL.md) | bare machines to a working cluster, in nine steps |
| [`SPEC.md`](SPEC.md) | the specification, and the reasoning behind each decision |
| [`AGENTS.md`](AGENTS.md) | R1 through R6 — the brief for changing anything |
| [`CONFORMANCE.md`](CONFORMANCE.md) | the claim register, generated from `model/ids.toml` |
| [`VERIFICATION.md`](VERIFICATION.md) | what each gate discharges, and the defect planted to prove it can fail |

## The shape of it

| Role | Detected by | Runs | Updates |
| --- | --- | --- | --- |
| `storage` | its own bulk disk | registry, object store, NFS, observability, control plane, registrar, CI runners, T2 host | 3rd |
| `compute` | assigned, first to register | devcontainers, the Remote-SSH target | 2nd |
| `testbed` | assigned, second to register | one measurement job at a time, nothing else | 1st |

**A machine is not told what it is.** One image is installed on all three
(§8.4); each works out its own role at boot from the hardware it holds and the
order it registered in (§2.3). The machine carrying a disk at or above the
declared threshold is the storage node and the registrar; the other two are
assigned in the order they ask. Nothing in this repository says which chassis is
which, and replacing a mainboard is not an edit to a file here.

A direct-attached 10GbE triangle joins them — no switch, a `/31` per link, every
mesh service bound to a loopback — and each machine also has a 1GbE link to the
LAN. Addresses are derived from ordinals rather than declared (§4.1).

The testbed updates first because a failure there costs a measurement window
rather than the pipeline; the storage node updates last because it carries the
machinery needed to diagnose a bad update.

## R1 over infrastructure

The template this was cut from applies R1 to documentation: `CONFORMANCE.md` is
generated from `model/`, and a hand-edit is a gate failure. This repository
extends the same rule to every infrastructure artifact.

| Path | What it is |
| --- | --- |
| `model/` | the single source: fleet, network, images, policy, and the claim register |
| `generated/` | **rendered** from `model/`, committed, diff-gated |
| `features/suites/` | one Gherkin scenario per conformance ID |
| `images/node/` | the one Containerfile (§8.4) |
| `bootstrap/` | the installer configuration the ISO is built from |
| `crates/cluster-model` | the typed model and the renderers |
| `crates/cluster-init` | what a machine works out about itself at boot |
| `crates/cluster-health` | the health predicate, shipped in every image |
| `crates/cluster-updater` | the rollout ordering predicate, drain, and apply |
| `crates/cluster-ctl` | the control plane: sessions, rollout state, enrolment, the API |
| `crates/devcontainer-agent` | the node-local agent: dirty, attachment, migration |
| `crates/cluster-web` | the browser client, a Leptos SPA on wasm32 |
| `crates/cluster-harness` | QEMU orchestration and the tier collector |
| `crates/conformance`, `crates/model` | the register and the honesty meta-gate |
| `xtask/` | the gates |

A hand-edited `.network` file is the same class of error as a hand-edited
`CONFORMANCE.md`, and `cargo xtask check-render` reports it the same way — in
both directions: an artifact nothing asserts about fails, and a claim that
touches no artifact fails too. `check-wiring` adds the joins the compiler cannot
see: a rendered file nothing copies into an image, a unit invoking an executable
nothing builds, an endpoint a component calls and no route serves.

## The gate

| Recipe | What it does |
| --- | --- |
| `just vv` | the whole acceptance gate — this is T0 |
| `just render` | rewrite `generated/` from `model/` |
| `just model` | the repository gates: R1, R4, R5, and the wiring check |
| `just bdd` | R3 and the honesty meta-gate |
| `just installer` | build the installer ISO locally (`INSTALL.md` step 3) |
| `just deny` | advisories, bans, licences and sources (needs `cargo-deny`) |
| `just t1` / `just t2` / `just t3` | the guest and hardware tiers |

The tiers are separate recipes on purpose: T2 takes about thirty-five minutes and
T3 needs the real fleet, and a gate a developer will not run is a gate that gets
bypassed. Which tier discharges which claim is a model fact, not a convention —
`model/ids.toml` carries a `tier` column, and a hardware claim registered below
T3 fails `check-model`.

A tier that cannot run exits 3 and reports a skip. It never reports a pass:
"did not run" is not "success", and `promote.yml` refuses a commit whose tier
was skipped for exactly that reason.

## The register

[`CONFORMANCE.md`](CONFORMANCE.md) carries every claim, its honesty level, the
tier that discharges it, and a per-suite summary. The counts are generated from
`model/ids.toml`; they are deliberately not restated here, because a number in
two places is two sources for it — and the copy that used to live in this file
was wrong within a few commits.

Every claim carries one of three honesty levels, and the build fails if the two
registers are blurred:

| Level | Meaning |
| --- | --- |
| `some-true` | reproduced from an authority. **Not established here.** |
| `build` | constructed here and validated against its oracle. Evidence, not proof. |
| `open` | measured and reported, **never asserted**. |

A claim cannot exist in the documentation without a register row, or in the
register without appearing in the documentation. `SPEC.md` §20 lists what is
cited rather than constructed; §21 lists what this repository deliberately does
not claim at all — including that the testbed yields stable measurements, which
is the thing the whole measurement node exists to make plausible and which no
gate here can establish.

## Adding a capability

In this order, because the order is the discipline (R3):

1. A row in `model/ids.toml`, with its level and its tier.
2. A scenario in `features/suites/`, tagged with the ID.
3. A failing test whose name **ends in the ID**, lowercased with underscores.
4. The implementation.
5. `just vv`.

Every class `SPEC.md` §19.2 declares now has rows. A new one adds its rule to
`crates/model` in the commit that adds its first ID, which is what `registry.rs`
has said since the template.

Before adding a gate, plant the defect it exists to catch and confirm it fires.
[`VERIFICATION.md`](VERIFICATION.md) records every plant, including the two that
correctly did **not** fire and why.

## Licence

Dual-licensed under either of

- Apache License, Version 2.0 ([`LICENSE-APACHE`](LICENSE-APACHE))
- MIT license ([`LICENSE-MIT`](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this work by you, as defined in the Apache-2.0 licence, shall be
dual-licensed as above, without any additional terms or conditions.
