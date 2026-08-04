# AGENTS

The standing brief for anyone --- human or otherwise --- changing this
repository.

## What this repository is

The whole definition of a three-node bootc cluster: what a node is, what an
image is, and how one is built, published, booted, updated, drained, and
retired. `SPEC.md` specifies the structure; this file governs every change to
it.

Read `SPEC.md` for what is being built, `README.md` for the shape of the
repository, then `VERIFICATION.md` for which gate discharges what.

It was cut from a template that carried the gate machinery and none of the
content. Two things about that are worth knowing, because both changed here.
**R1 now covers infrastructure**, not only documentation: `generated/` is
rendered from `model/` and diff-gated, so a hand-edited `.network` file fails the
same way a hand-edited `CONFORMANCE.md` does. And **R5's allowlist is read from
the model** rather than hard-coded in a gate, because a gate whose allowlist is
edited to make it pass has stopped enforcing anything.

## The rules

Every gate names the rule it enforces when it fails, so a red gate says *which
promise* was broken rather than merely that something is wrong. These are those
rules; nothing else in the repository defines an `R`-number.

| Rule | What it says | Enforced by |
| --- | --- | --- |
| R1 | The model is the single source. Every claim lives in `model/*.toml`, and `CONFORMANCE.md` is generated from it. | `cargo xtask check-model` |
| R2 | Levels are load-bearing. A claim is `some-true`, `build`, or `open`, and the two registers are never blurred. | the honesty meta-gate |
| R3 | A capability begins as a register row, then a scenario, then a failing test. | `just bdd` |
| R4 | Nothing is deferred. No deferral marker, no stub, no capability behind a flag that turns it off. | `cargo xtask audit-deferral` |
| R5 | No unsanctioned error. Every error a caller can see is one the model sanctions --- and the list of those lives in `model/ids.toml`, naming the claim that sanctions each. | `cargo xtask audit-limits` |
| R6 | Nothing shipped depends on a dev-only crate, and no dependency arrives by wildcard. | `just deny` |

Expanded, in the order they are most often broken:

1. **The model is the single source (R1).** `CONFORMANCE.md` is *generated*;
   editing it is a mistake the gate catches. So is every file under
   `generated/` --- the `.network` units, the firewall, the Quadlets, the kernel
   arguments, the timer units, the kickstarts, `ssh_config`. Run
   `just model-write` and `just render` after changing the model.

   Each rendered file names the claims that assert over it, and `check-render`
   cross-references those against the register in both directions: an artifact
   nothing asserts about is a gap, and a `CD-` claim that touches no artifact is
   vacuous. Both fail.

2. **Nothing is deferred (R4).** If a change cannot be finished, it should not
   be started --- and `cargo xtask audit-deferral` will say so. It reads every
   crate and `xtask`, which includes itself.

3. **Levels are load-bearing (R2).** `some-true` is reproduced from an
   authority and is not established here. `build` is constructed here and
   validated against its oracle: evidence, not proof. `open` is measured and
   reported, never asserted. Writing "proves" about an `open` claim fails the
   meta-gate, and it should.

4. **A claim about a dependency belongs to that dependency.** Restating an
   imported library's guarantees here would give a claim two sources, which is
   what R1 forbids. Cite it, link it, demonstrate it --- do not re-register it.

## Adding a capability

In this order, because the order is the discipline (R3):

1. A row in `model/ids.toml`, with its level **and its tier**.
2. A scenario in `features/suites/`, tagged with the ID.
3. A failing test whose name **ends in the ID**, lowercased with underscores.
4. The implementation.
5. `just vv`.

The tier is a model fact rather than a convention, and `SPEC.md` §19.2's rule for
the `CH-` class depends on it: a hardware claim registered below `T3` fails
`check-model`, and `cluster-harness` refuses to hand one to a simulated run even
if it somehow got there. A `CH-` claim discharged by a QEMU guest would be a
false statement about a physical machine.

Every class §19.2 declares has rows. Adding a *new* class means adding its rule
in the commit that adds its first ID --- `registry.rs` has said so since the
template, and the `OPEN-`, `CD-`, `CH-` and `CG-` rules were each written that
way rather than left as a convention.

Steps 1--3 before step 4 is the whole of R3. The meta-gate enforces it: an ID
with no scenario, a scenario with no ID, or an ID with no test all fail
`just bdd`.

## Adding a crate

A crate is *shipped* when its manifest does not say `publish = false`, and the
gates read that rather than a list, so nothing needs to be registered anywhere.
A shipped crate is subject to R5; a dev-and-CI-only crate is not.

## Writing a gate

A gate that cannot fail is worse than no gate, because it reads as evidence.
Before adding one, plant the defect it exists to catch and confirm it fires,
then record that in `VERIFICATION.md`'s falsifiability table.

Gates in the repository this template was cut from were found vacuous
repeatedly, and in every flavour: a differential test comparing the reference
against itself, a claim discharged by a compile rather than a run, a job whose
crate list omitted the crate it was named for, a feature nothing ever built, and
examples in the `README` that nothing compiled. Assume yours is one until you
have watched it fail.

A skipped tier is the same failure wearing a different hat. `/dev/kvm` is
inconsistently available on hosted runners, so the harness probes for it and
reports absence as an explicit, non-zero skip --- never a silent fallback to
TCG. A tier that quietly emulated would be slower, differently timed, and would
look green.

Two habits follow from that:

- **Arm an anti-vacuity check on the register, do not assert it outright.**
  "There must be feature files" is false on an empty repository and true on a
  populated one. "There are registered IDs and no feature files" is the defect
  in both. Write the second.
- **A gate that reads source must survive reading its own.** `audit-deferral`
  spells its markers in halves for exactly this reason: a list of forbidden
  tokens written out in full matches itself, and the alternative --- exempting
  the file --- puts a hole precisely where a deferral parked in a gate would sit.

## Comments

Explain *why*, not *what*. The code says what it does. A comment earns its place
by recording the reason a decision went one way when it could plausibly have
gone another.
