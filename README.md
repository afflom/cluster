# cluster

Three Supermicro nodes as a hypervisor substrate for OCI workloads: devcontainers,
GitHub Actions runners, and a small set of storage services. The host operating
system is an appliance, built with bootc, and the cluster's entire definition
lives here.

There is no configuration applied out of band, no node-local state that is not
either declared here or explicitly designated as data, and no path by which a
node comes to run software that did not pass the gate.

`SPEC.md` is the specification. `AGENTS.md` defines R1 through R6 and is the
brief for changing anything. `VERIFICATION.md` maps each gate to what it
discharges and records the defect planted to prove it can fail.

## The shape of it

| Node | Role | Runs | Updates |
| --- | --- | --- | --- |
| storage | `storage-ci` | registry, object store, NFS, observability, control plane, 2 CI runners | 3rd |
| compute | `compute` | devcontainers, the Remote-SSH target | 2nd |
| testbed | `bench` | one measurement job at a time, nothing else | 1st |

A direct-attached 10 GbE triangle joins them, `/31` per link, every mesh service
bound to a loopback. the testbed is first to update because a failure there costs a
measurement window rather than the pipeline; the storage node is last because it carries the
machinery needed to diagnose a bad update.

## R1 over infrastructure

The template this was cut from applies R1 to documentation: `CONFORMANCE.md` is
generated from `model/`, and a hand-edit is a gate failure. This repository
extends the same rule to every infrastructure artifact.

| Path | What it is |
| --- | --- |
| `model/` | the single source: nodes, network, images, policy, and the claim register |
| `generated/` | **rendered** from `model/`, committed, diff-gated |
| `features/suites/` | one Gherkin scenario per conformance ID |
| `crates/cluster-model` | the typed model and the renderers |
| `crates/cluster-health` | the health predicate, shipped in every image |
| `crates/cluster-updater` | the rollout ordering predicate, drain, and apply |
| `crates/cluster-ctl` | the control plane: sessions, rollout state, the API |
| `crates/cluster-web` | the Pages UI, a Leptos SPA on wasm32 |
| `crates/cluster-harness` | QEMU orchestration and the tier collector |
| `images/` | one Containerfile per variant |
| `xtask/` | the gates |

A hand-edited `.network` file is the same class of error as a hand-edited
`CONFORMANCE.md`, and `cargo xtask check-render` reports it the same way --- in
both directions: an artifact nothing asserts about fails, and a claim that
touches no artifact fails too.

## The gate

| Recipe | What it does |
| --- | --- |
| `just vv` | the whole acceptance gate --- this is T0 |
| `just render` | rewrite `generated/` from `model/` |
| `just model` | the repository gates: R1, R4, R5, and the wiring check |
| `just bdd` | R3 and the honesty meta-gate |
| `just deny` | advisories, bans, licences and sources (needs `cargo-deny`) |
| `just t1` / `just t2` / `just t3` | the guest and hardware tiers |

The tiers are separate recipes on purpose: T2 takes about thirty-five minutes and
T3 needs the real fleet, and a gate a developer will not run is a gate that gets
bypassed. Which tier discharges which claim is a model fact, not a convention ---
`model/ids.toml` carries a `tier` column, and a hardware claim registered below
T3 fails `check-model`.

## The register

61 claims across twelve classes, each discharged at exactly one tier.

| Suite | Claims | Tier |
| --- | --- | --- |
| `model` | 4 | T0 |
| `definition` | 11 | T0 |
| `image` | 4 | T0 |
| `boot` | 6 | T0, T1 |
| `network` | 3 | T2 |
| `storage` | 3 | T2 |
| `workload` | 2 | T2 |
| `update` | 10 | T0, T2 |
| `reclaim` | 5 | T0 |
| `control` | 5 | T0 |
| `lifecycle` | 4 | T0, T2 |
| `hardware` | 4 | T3 |

`CONFORMANCE.md` is generated from `model/ids.toml` and carries every statement.

## Claim discipline

Every claim carries one of three honesty levels, and the build fails if the two
registers are blurred:

| Level | Meaning |
| --- | --- |
| `some-true` | reproduced from an authority. **Not established here.** |
| `build` | constructed here and validated against its oracle. Evidence, not proof. |
| `open` | measured and reported, **never asserted**. |

`CONFORMANCE.md` is generated from `model/`, so a claim cannot exist in the
documentation without a register row, or in the register without appearing in the
documentation. `SPEC.md` §20 lists what is cited rather than constructed; §21
lists what this repository deliberately does not claim at all --- including that
the testbed yields stable measurements, which is the thing the whole measurement node
exists to make plausible and which no gate here can establish.

## Adding a capability

In this order, because the order is the discipline (R3):

1. A row in `model/ids.toml`, with its level and its tier.
2. A scenario in `features/suites/`, tagged with the ID.
3. A failing test whose name **ends in the ID**, lowercased with underscores.
4. The implementation.
5. `just vv`.

Every class `SPEC.md` §19.2 declares now has rows. A new one adds its rule to
`crates/model` in the commit that adds its first ID, which is what `registry.rs`
has said since the template --- the `OPEN-`, `CD-`, `CH-` and `CG-` rules were
each written that way.

## Licence

Dual-licensed under either of

- Apache License, Version 2.0 ([`LICENSE-APACHE`](LICENSE-APACHE))
- MIT license ([`LICENSE-MIT`](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this work by you, as defined in the Apache-2.0 licence, shall be
dual-licensed as above, without any additional terms or conditions.
