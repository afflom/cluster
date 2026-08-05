# VERIFICATION

Which axis of the gate discharges which class of claim. `AGENTS.md` defines the
rules; `SPEC.md` defines the structure; this maps them onto the commands that
enforce them.

## The acceptance gate

| `just` recipe | Enforces | ID classes |
| --- | --- | --- |
| `just fmt-check` | the diff is reviewable | --- |
| `just model` | R1, R4, R5 --- and R1 over infrastructure | `CM-01`, `CD-` |
| `just lint` | clippy at `-D warnings` | --- |
| `just test` | the workspace suite | every `T0` claim |
| `just features` | every optional feature compiles, and so does the configuration with none | --- |
| `just bdd` | R3 and R2's behavioural half | `CM-02`, `CM-03` |
| `just deny` | R6, over the dependency graph | --- |

`just vv` runs all but `deny`, which needs a separately installed tool and is
therefore run alongside it rather than inside it.

## The tiers

`just vv` is T0. The tiers that need a machine are separate recipes, because a
gate a developer will not run is a gate that gets bypassed --- T2 takes about
thirty-five minutes and T3 needs the real fleet (`SPEC.md` §10.2).

| Recipe | Tier | Runs where | Discharges |
| --- | --- | --- | --- |
| `just vv` | T0 | anywhere | claims registered at `T0` |
| `just t1` | T1 | GitHub-hosted, KVM permitting | claims registered at `T1` |
| `just t2` | T2 | self-hosted on the storage node | claims registered at `T2` |
| `just t3` | T3 | the real fleet | claims registered at `T3` |

The tier a claim is discharged at is a **model fact**: `model/ids.toml` carries a
`tier` column, `CONFORMANCE.md` renders it, and `cluster-harness` collects by it.
That is what makes §19.2's rule for the `CH-` class enforceable rather than a
convention --- a hardware claim registered below `T3` fails `check-model`, and a
simulated tier refuses to collect one even if it somehow got there.

**A skipped tier is loud, and it runs nothing.** `/dev/kvm` is documented as
inconsistently available on hosted runners, so the harness probes for the whole
fixture --- accelerator, backing image, firmware, tools --- and reports a missing
one as an explicit skip with exit 3. It never falls back to TCG: a tier that
quietly emulated would be slower, differently timed, and would look green (§9.4).

The exit status is what stops the tier's assertions from running at all. This was
got wrong first: the tier tests skipped *themselves*, so `just t1` exited zero
reporting "5 passed" having booted nothing --- the vacuous gate in its most
convincing disguise. The decision now lives in one place, the driver, and no test
skips. `just t1` names the five claims it would have discharged and says nothing
was tested.

T1 may skip and the workflow records it as a skip. T2 may not: on the storage
node KVM is guaranteed, so its absence means the node is broken rather than the
harness being portable, and treating that as a skip would let a broken CI host
promote images.

## What changed when identity stopped being declared

`SPEC.md` §2.3 and §3.1 were rewritten so that a machine works out its own place
rather than being told it. Four things that had been true stopped being true, and
each of them was a test that had to be rewritten rather than repaired.

`CD-01` asserted that every rendered `.network` file matched on `MACAddress=`.
There are no MACs now; it asserts instead that **no** rendered artifact carries
one, and that the thresholds a machine sorts its own ports with are rendered.

`CD-06` asserted that `isolcpus=` appeared in exactly one node's `kargs.d`. One
image reaches all three machines, so an isolation argument there would isolate
the storage node's cores too; it now asserts that the base carries none and that
exactly one *role* does.

`CI-03` asserted that each variant copied its own node's tree and no other's. The
failure it guarded --- a variant carrying another node's addresses --- is
unreachable with one tree, so it asserts that it stayed unreachable: no build
references a per-machine tree, because there is no such thing to reference.

`CH-02` asserted that every declared MAC was present on the card the model named.
That is the one check this change genuinely gives up, and §21.12 records it. What
replaced it is what real hardware can still establish: the chassis presents the
port counts §2.1 declares, and every peer loopback is reachable --- which is only
true if each cable runs to the machine the addressing assumed.

## Every gate is falsifiable

A gate nobody has seen fail is indistinguishable from a gate that cannot. Before
adding one, plant the defect it exists to catch, confirm it fires, and add a row
here.

| Gate | Planted defect | Reported |
| --- | --- | --- |
| `check-model` (R1) | a `CONFORMANCE.md` that disagrees with the register | yes |
| `check-model` (R1) | a `spec` tag left at `template/1` | yes, naming the file |
| `check-render` (R1) | a hand-edited route metric in a rendered `.network` file | yes, naming the file |
| `check-render` (R1) | a file under `generated/` that the model does not render | yes, citing §17.2 |
| `check-render` (R1) | a `CD-` row that no rendered artifact names | yes, as vacuous |
| `audit-limits` (R5) | a shipped crate returning an unregistered error type | yes, ten call sites |
| `audit-deferral` (R4) | a deferral marker in a crate, and one in the gate's own source | yes, both |
| `check-render` (R1) | a signing identity with no workflow, and one naming only the repository | yes, both |
| `check-model` (§19.2) | a `CH-` claim registered below `T3` | yes |
| `CI-01` | a Containerfile floating its base tag | yes |
| `CI-03` | a build copying a per-machine tree | yes |
| `CL-03` | promotion copying to `:stable` without signing | yes |
| the honesty meta-gate (R2) | an ID with no test | yes |
| the meta-gate's `CG-` class rule | a reclamation scenario with no dirty-workspace case | yes, twice |
| the meta-gate's ID-in-test-name rule | a `CM-` prefix with unregistered rows | yes, unprompted |
| the tier driver (§9.4) | a tier with no fixture to boot | yes, exit 3, no assertions run |
| `check-wiring` (R1) | the signature policy rendered and copied into no image | yes, naming the file |
| `check-wiring` (R1) | a Quadlet mounting a configuration nothing renders | yes, citing §5.4 |
| `check-wiring` (R1) | an endpoint called by a component and served by no route | yes, listing the routes |
| `check-wiring` (R1) | a model field declared and read by nothing | yes, naming the line |
| `check-wiring` (R1) | a wildcard cross-origin, and a zero token-cache TTL | yes, both, citing §16.3 and §16.2 |
| `CC-01` | an allowlist that admits everyone when empty | yes |
| `CW-05` | the tunnel Feature omitting the supervisor or baking the server | yes |
| `CI-07` | a world-writable `policy.json` in the rendered tree | yes, naming the file and the mode |

**One plant did not fire, and that was the finding.** `CL-01` asserted the
rendered policy's certificate identity "contains the workflow" --- and
`contains("")` holds for every string, so a signing identity with an empty
workflow passed a gate whose entire purpose is to refuse exactly that. The
assertion is now on the whole identity rather than a substring of it, and the
model check refuses an identity that is not a workflow path at all, so the defect
cannot reach a render. A gate nobody has seen fail is indistinguishable from a
gate that cannot; this one had been the second kind.

Four more are worth the second column.

**`audit-limits` fired on its first real subject.** The template hard-coded an
allowlist of three error names from the repository it was cut from, which made
R5 a promise about a list nobody could change without editing a gate --- and a
gate whose allowlist is edited to make it pass has stopped enforcing anything.
The allowlist now comes from `model/ids.toml`, and the first shipped crate
(`cluster-health`) failed the gate at ten call sites until `ProbeError` was
registered against the claim that sanctions it. It fired again on `cluster-ctl`,
where a `collect::<Result<Vec<_>, _>>()` turbofish named an inferred driver error
in a signature; the fix was to wrap each row's failure where it happens.

**The `CG-` class rule caught a real gap, not a planted one.** `SPEC.md` §19.2
anticipates that the first `CG-` row should require a dirty-workspace case,
because retention tested only on clean workspaces has never been tested against
the failure that matters. Implementing the rule immediately failed `CG-02`, whose
scenario covered archiving on clean sessions only. The scenario and its test both
gained the dirty case.

**The meta-gate found the template's own gap.** `CM-01` through `CM-03` had tests
and no register rows. Nothing looked for them, because with an empty register no
ID's prefix was `CM` and the "every ID a test names is registered" check only
examines prefixes the register already uses. Adding `CM-04` made `CM` a known
prefix and the gap surfaced in the same run. The anti-vacuity design worked, a
little late --- which is the argument for keying guards to the register rather
than asserting them outright.

`audit-deferral` is worth the second column too, for the reason the template
gives: it reads every crate *and* `xtask`, so it reads its own source, and its
markers are spelled in halves. Both plants were run --- one in a crate, one in
the gate --- and both were caught.

## The gate that was missing

`check-render` proves the rendered tree equals the model. It says nothing about
whether anything *consumes* it --- and that was the gap that mattered. The
signature policy, which §12.3 calls the only thing standing between an unattended
node and an arbitrary image, rendered correctly, was diff-gated correctly, was
asserted over by a claim that passed, and was copied into no image. Every gate
was green and no node had a policy.

`cargo xtask check-wiring` reads the joins instead of the artifacts, in five
directions:

| Direction | What it caught on its first run |
| --- | --- |
| every rendered file is copied into an image | the signature policy and registry configuration on all three nodes, and the measurement node's exporter Quadlet |
| every path a unit invokes is produced | `runner-loop`, `zot-gc`, and the devcontainer agent |
| every image in this namespace is built | the control plane and the agent, both named as containers nothing built |
| every configuration a unit mounts read-only is rendered | Zot, Prometheus, and Alertmanager, all mounting empty directories |
| every endpoint a component calls is routed | `/api/nodes/:node/drain`, posted by the updater on the node it was about to reboot |

The fifth is the reason for the module. A caller and a router in different crates
are joined by a string no compiler checks, and the drain the whole of §14 depends
on was posted to nothing.

A sixth check runs the other way: **every field the model declares is read by
something**. A dangling reference fails at boot; a declared-and-unread field
fails more quietly than that --- the model says the journal is capped at 2G, the
register renders a document saying so, and nothing ever applies it. It found
thirty-seven, among them the SSH daemon policy, the SELinux mode, greenboot's
attempt count, Prometheus's retention, the snapshot tool, the deployment count,
and every hardware figure in §2.1's profile. Each is now applied or verified, and
the three that were hard-coded in the base Containerfile are rendered --- a build
that also declared them gave one decision two sources, and an `sshd` accepting
passwords would have satisfied a model saying it did not.

The name search excludes the type definitions that parse the model. A field
appears in the struct that reads it whether or not anything uses the value, so
counting that as "read" would make the check pass on a model nothing applies,
which is the whole condition it exists to find.

## Three gates that were caught being weak

**The wildcard-origin plant "passed" for the wrong reason.** Changing
`allowed_origin` to `*` failed `check-render` --- but only because the committed
tree had gone stale relative to the model, which is what *any* edit to
`model/policy.toml` does. Nothing was checking the value. The check is now on the
model, where a stale render cannot stand in for it, and the re-plant fails
naming the origin.

That is the second time in this repository a gate has looked green for a reason
unrelated to what it claims to check, and both times the tell was the same: the
failure message did not mention the thing being planted.

**The accelerator probe asked whether a file existed, not whether a guest could
be accelerated.** `/dev/kvm` can be present on a hosted runner and unusable ---
nested virtualisation is documented as unsupported there --- and QEMU meets that
with `Could not access KVM kernel module: Permission denied` and an immediate
exit. Testing for the path reported the fixture present, so the tier ran, and six
tests each spent five minutes failing to reach a guest that had never started.
The probe opens the device now, so an unusable one is an honest skip in a second.

That is the same shape as the skip notice that named `/dev/kvm` while the real
problem was a missing firmware image: a check answering the question next to the
one it claims. "Is there a file called /dev/kvm" and "can this machine
accelerate a guest" are different questions, and only the second is the tier's.

Two things kept that hidden. The harness reported QEMU "still running" by testing
for `/proc/<pid>` --- but a process that has exited and not been reaped is a
zombie, and a zombie still has one, so a dead QEMU read as a live guest. And the
guest's console was directed at one serial port while the image is told
`console=ttyS1,115200`, which is the *second* --- so every boot message went to a
port that did not exist. QEMU's own stderr was piped and never read at all.

**Three things had to be true before a guest could answer at all, and none of
them had ever been exercised.** T1 had never run, so every step past "the disk
exists" was unverified, and each one surfaced only when the one before it was
fixed:

1. `virtio_net` reports `Supported link modes: Not reported`. The classifier read
   only supported modes, so every guest sorted **zero** mesh ports and
   `cluster-init` refused the boot --- correctly by its own rule. It now falls
   back to the negotiated speed when a driver reports no supported modes at all,
   and `max_supported_mbps` states what that costs; the harness sets `speed=` on
   the guest NICs from the model's own class thresholds.
2. A bootc image locks root and installs no key. The tier authenticated as
   `root@127.0.0.1` against a guest that could never accept it. The disk build
   now makes an ephemeral key and installs its public half, and `Fixture::missing`
   reports the absence of one as a reason the tier cannot run.
3. `Connection refused` for five minutes says only that nothing is listening. It
   is equally true of a QEMU that exited immediately, an image that panicked in
   the initramfs, and a boot that failed a unit before `sshd`. The harness now
   reports which: whether the process is still alive, and the arguments it was
   started with.

**T1 booted the one node that cannot boot alone, and a test passed over an
empty loop while it did.** T1 took the first node in *rollout* order, which is
the testbed --- a machine holding no bulk disk, and therefore one with no ordinal
until the registrar answers (§2.3.2). With a single guest there is nothing on
either cable to answer, so it refused to come up, correctly, six times, after a
five-minute SSH timeout each. T1 now boots the storage node, which is the only
role that can come up alone.

`CB-06` moved to T2 with it, for the same reason and for a second one: the
isolation arguments are applied after the role is known now (§8.5), and there is
no role on a machine that never got an ordinal.

The `CB-06` test had also been passing while testing nothing. It looked its
variant up by node *name* when variants are keyed by *role*, matched none, hit
`continue` on every iteration and reported `ok` --- the vacuous gate again, this
time introduced by the very change that renamed the key. It now counts what it
checked and asserts the count is one.

**The tier driver and the tier's tests disagreed about where the disk was.**
`Fixture::from_environment` resolved `target/harness/base.qcow2` against the
working directory. `cargo run` runs from the workspace root and `cargo test` runs
from the package root, so `just t1` found the disk with its first command and
reported it missing with its second --- seven tests failing with a skip notice
about a file that was sitting there.

It had been latent for as long as T1 never actually ran, and it surfaced on the
first run that got far enough to boot something. Paths now resolve against the
repository root. This is the argument for a tier having to boot something before
it is believed: two commits earlier the same code passed, having tested nothing.

**The image shipped a world-writable signature policy, and nothing failed.**
`COPY` preserves the mode of the file on the build host. `just render` used
`std::fs::write`, which takes the umask, and a permissive one produced a `0666`
tree that the build copied straight through. systemd noticed --- "Configuration
file /usr/lib/systemd/system/cluster-init.service is marked world-writable.
Proceeding anyway" --- and proceeded, which is the whole problem: the warning was
in the build log of an image that then passed `bootc container lint` and every
gate here.

The file that matters is `policy.json`. §12.3 calls it the only thing between an
unattended node and an arbitrary image, and any local user could have rewritten
it. The renderer now sets `0644` explicitly and the build runs `chmod -R go-w`
over everything it copied; `CI-07` asserts both, and the plant fires naming the
file and the mode.

Found by looking at the built image rather than by any gate, which is the third
time in this repository that has been the finding. A gate reads what it was told
to read.

**A literal search read a doc comment as a value.** `CD-17` asserts that no
model fact is hard-coded in the binary that reads it, by searching
`cluster-init`'s source for the rendered thresholds. It fired on
`links.rs`, which explains in a doc comment that `ethtool` reports
`10000baseT/Full` --- documentation of a format, not a value anything uses. The
search now drops comment lines and `#[cfg(test)]` modules.

That is the third time an extractor here has read prose as code, and the tell has
been identical every time: what it objected to was a sentence *about* the thing
rather than the thing. It is recorded again because three occurrences is a
pattern rather than an accident --- any gate that greps source will meet it.

**A test hung the suite instead of failing it.** `generate_secret` read
`/dev/urandom` with `std::fs::read`, which reads to EOF, and `/dev/urandom` has
none. The suite did not fail; it was killed, and a `SIGTERM` with no assertion
attached is the least informative way a defect can present. `read_exact` of a
fixed length fixed it.

**The tier tests were compiled by nothing.** `lint` runs `clippy --workspace
--all-targets`, and `--all-targets` means every target whose manifest says
`test = true`. The three tier tests say `test = false` --- deliberately, so
`cargo test` cannot report `ok` for a claim it booted no guest to check --- and
that same flag took them out of `--all-targets`. They were run by `just t1`,
`t2`, `t3` and compiled by nothing else, so `tests/t1.rs` returned
`Some((c, guest))` from a function typed `(Cluster, Guest)` and the entire gate
passed. It surfaced on a runner that had just spent twenty minutes building a
disk image, which is the most expensive place in the system to learn about a
type error.

`lint` now names the three targets explicitly. They compile and they lint; they
still never run. Naming them found a second defect immediately --- an unused
import in `tests/t2.rs` --- and a planted `fn _plant() -> u32 { "not a u32" }`
in `tests/t3.rs` fails the gate and passes again when removed.

The tell was the same one as below: the skip that hid this said `/dev/kvm is
absent` on a runner where KVM was fine. A gate reporting a cause it never
measured is a gate reporting nothing.

**Two extractors read prose as code.** `check-wiring` read a doc comment
containing `` `GET /api/auth/config` `` as a call to an endpoint, and `CW-05`
read the tunnel Feature's own comment explaining why it does *not* pass
`--random-name` as though it passed it. Both now read instructions rather than
text, for the reason `audit-deferral` spells its markers in halves: a gate that
cannot be described in the file it inspects is a gate nobody can write around
honestly.

## What this suite does not establish

Anything about a dependency. bootc's transactional update, ostree's deployment
atomicity, greenboot's boot-counting rollback, the Dev Containers specification,
the Quadlet contract, `dm-cache`'s writethrough durability, Tailscale's identity
header --- each is gated in its own project. Restating any of them here would
give a claim two sources, which is what R1 forbids. `SPEC.md` §20 records what is
cited; §21 records what is deliberately not claimed at all.

What may be claimed here is what is built here, and `SPEC.md` §21 is the standing
list of what that excludes: that the testbed yields stable measurements, that the
hardware is as declared without a real node to ask, that a dependency behaves as
documented, that one copy of `lv_data` is enough, and that unattended update is
risk-free.
