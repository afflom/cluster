# Installing a cluster

From bare machines to a working cluster. Every step was read off the artifacts it
describes rather than remembered; where something is not established, it says so
rather than sounding confident.

`SPEC.md` is the specification and carries the reasoning. This is the sequence.

## What you need

**Three machines.** `model/cluster.toml`'s `[profile]` declares the fleet this
repository was built for: Supermicro SYS-E300-8D, Xeon D-1518, 32 GB, two 10GbE
and two 1GbE ports each. Different hardware works — `[profile]` is a description,
and `CH-` verifies a real node against it — but three things are structural, and
changing them is a model change rather than a configuration change:

| Requirement | Why | Declared in |
| --- | --- | --- |
| Two mesh-class ports per machine | each machine has a direct link to each peer, and two ports is what makes three the fleet size | `[[class]]`, `model/network.toml` |
| At least one LAN-class port | the management plane, and the only route in before a tailnet exists | same |
| Exactly one machine with a non-boot disk at or above 1000 GB | this is how the storage node is detected; on any other count the boot refuses rather than guessing | `[detection]`, `model/cluster.toml` |

**Cabling.** The 10GbE ports are direct interconnects, machine to machine, with
no switch: each machine's two mesh ports go to its two peers. The 1GbE port goes
to your LAN switch. Which cable lands on which port does not matter — a machine
sorts its own ports by the speed their driver reports and discovers which peer is
on the far end (§3.1, §3.3).

**Firmware.** `[[firmware]]` in `model/cluster.toml` lists eight settings and why
each is needed. Apply them by hand — this pipeline cannot reach BIOS — and
`CH-01` re-verifies them against the model on every hardware run. Two will cost
you an afternoon if missed: **Restore on AC Power Loss: Power On** (headless
cluster, no operator after an outage) and **Console Redirection: COM2/SOL,
115200 8N1**, which matches `console=ttyS1,115200` in the image and is how you
watch a boot.

**A build host** with podman, a Rust toolchain, and roughly 20 GB free. It need
not be one of the three machines.

## 1. Make the model yours

The model is the single source and nothing is configured out of band. A fork that
changes nothing builds an image pinned to *this* repository's identity. These are
the fields that are about you rather than about the design:

| File | Field | What it is |
| --- | --- | --- |
| `model/cluster.toml` | `domain` | the cluster's DNS-style domain, e.g. `devcluster` |
| | `tailnet` | your tailnet's name |
| | `magic_dns_suffix` | Tailscale's suffix, normally `ts.net` |
| | `authorized_logins` | the GitHub logins that may drive the cluster |
| | `[github_app] client_id` | the App from step 2 |
| | `[detection] bulk_disk_min_gb` | above your cache SSD, below your bulk disk |
| | `[profile]`, `[[disk]]` | your hardware, if it differs |
| `model/network.toml` | `lan_prefix` | your LAN, e.g. `192.168.20.0/24` |
| `model/images.toml` | `[signing] repository` | `<owner>/<repo>`: where runners register, and the only repository whose workflow may sign an image your nodes will stage |
| `model/policy.toml` | `rollout.image_repository` | where your images live, e.g. `ghcr.io/<owner>/<repo>` |
| | `rollout.registries` | pull order, local mirror first |
| | `auth.allowed_origin` | your Pages origin, exactly — never a wildcard |

Then:

```sh
just render     # rewrite generated/ from model/
just vv         # the acceptance gate
```

`just vv` is not advisory. It refuses a model that is internally inconsistent: a
migration target that is not a role, an allowed origin carrying a wildcard,
reclamation thresholds that do not increase, a role that hosts a runner with no
runner installed. If it passes, what is in `generated/` is what your fleet runs.

## 2. Create the GitHub App

The browser client authenticates by **device flow**, which is what a static page
can actually do: a public client ID, no client secret, no callback URL. Create a
GitHub App with:

- **Device flow enabled.** This is the setting that gets missed; without it the
  browser fails at the first step.
- **No extra permissions.** The App is identity only — `read:user`. It
  deliberately does not request `repo`.

Put its **Client ID** in `[github_app] client_id`, re-render, re-run the gate.
The client ID is public by design and is deliberately not in §12.2's table of
secrets: a table of secrets containing a non-secret teaches its reader to skim.

Authorization is then a comparison against `authorized_logins`. There is no
membership API in play — a user account has none — and §16.2 states that limit
rather than leaving you to discover it.

## 3. Build the installer

```sh
just installer
```

This builds the node image locally, fills `bootstrap/config.toml` with the
rendered kickstart, runs bootc-image-builder, and leaves `iso/node.iso` with its
checksum beside it.

**Why locally, for a first install.** `promote.yml` is the normal way to get an
ISO and it is not available yet: promotion refuses a commit whose T2 did not run,
T2 runs on a self-hosted runner, and that runner runs on the cluster you are
installing. A first install cannot wait on a cluster that does not exist yet.
`just installer` uses the same substitution and the same builder as the release
path, so this is the same kind of artifact — unsigned and unreleased.

**One ISO for all three machines.** There is one image (§8.4). What a machine
does is decided at boot from its own hardware and from the registrar, not by
which installer you chose, so there is nothing to select.

## 4. Install the machines

Write `iso/node.iso` to a USB stick, or mount it as virtual media over IPMI.

**Power on the machine with the bulk disk first.** It detects itself, takes
ordinal 1 and becomes the registrar. Then the other two, one at a time: the first
to register becomes `compute`, the second `testbed` (§2.3.2).

Order decides only *which* of two identical machines gets which role. A machine
powered on before the registrar waits, bounded by the discovery timeout, so a
fleet can be powered on in any order without anything breaking — §12.1 requires
that, and splitting `cluster-init` from `cluster-peers` is what makes it true.

The install is unattended, and `rootpw --lock` means there is no console login;
watch it over IPMI serial-over-LAN if you want to see it happen. Each machine
reboots into the installed system when Anaconda finishes.

**A node comes up unenrolled** — no SSH key, no registry token, no tailnet. That
is deliberate: an ISO is a release artifact, and a secret put into one is
published to whoever downloads it. What an unenrolled node has is the control
plane, reachable over the LAN (§12.2).

## 5. Find the control plane

The storage node serves it on port 8080, opened to `lan_prefix` for exactly this
step. Its address comes from DHCP, so there is no address in this repository to
give you. Two ways to find it:

- **Your DHCP server's lease table.** Each machine sets its hostname from its
  ordinal at first boot, so the storage node appears as `node1` — or whatever
  your `name_template` derives. Whether that name is *resolvable* depends on your
  router registering DHCP hostnames; many do, some do not, and this repository
  cannot establish which yours does.
- **IPMI.** The serial console shows the boot, and `ip addr` shows the lease.

Then open `http://<address>:8080/` from a browser on the same LAN.

That URL serves the browser client from the node itself. The same bundle goes to
GitHub Pages and Pages is canonical, but the node's own copy is the one that
always works — same-origin has no preflight and no browser policy between the
page and the API it was built for (§16.3). For a first enrolment it is also the
only one that exists, because Pages has not been told where your API is yet.

## 6. Enrol the secrets

Sign in with the device flow — the page shows a code, you enter it at GitHub —
then enter four values. None of them is in this repository, in the image, or in
the ISO.

| Secret | What to use | Without it |
| --- | --- | --- |
| `ssh_authorized_key` | your SSH **public** key | SSH stays closed, and §16.5 keeps SSH as the way back in when the control plane is what is wrong |
| `registry_pull_token` | a GHCR token with `read:packages` and nothing else | unattended updates cannot pull |
| `runner_registration_pat` | a GitHub token with `administration: write` on the repository | no self-hosted runner registers, so T2 and the Pages mirror never run |
| `tailnet_auth_key` | a Tailscale auth key, ephemeral and single-use | no off-LAN access, and the control plane is not published on the tailnet |

The control plane reports which secrets it has and **never returns one**. There
is no route that will; an operator who has lost a token issues a new one.

You enter credentials, not files. Two of these are shaped on the way in and you
do not need to know how: the registry token becomes a containers-auth document
keyed by your registry, with your authenticated GitHub login as the username, and
the Tailscale key is spent by `tailscale up` and deliberately not kept.

Enrol each machine. The SSH key and registry token are per-node; the runner
credential is wanted on the storage node (two CI runners) and the testbed (the
bench runner); a Tailscale key is single-use, so issue one per machine.

## 7. Check it came up

Copy `generated/ssh_config` into your `~/.ssh/config`, or include it. It
addresses nodes by their tailnet names, because management addresses come from
DHCP and there is no address to write down.

```sh
ssh node1
cluster-health check     # the predicate, as JSON; non-zero if unhealthy
```

`cluster-health check` evaluates eight checks and is the same predicate greenboot
runs to decide whether a boot succeeded. A node is not in service until it
passes.

## 8. Turn on the rest of the pipeline

Once the fleet is up and its runners have registered:

| Setting | Where | Value |
| --- | --- | --- |
| `CLUSTER_API_BASE` | repository **variable** | your control plane's tailnet URL, e.g. `https://node1.<tailnet>.ts.net` |
| `CLUSTER_FLEET_ONLINE` | repository **variable** | `true`, once runners are registered |
| Pages | Settings → Pages | source: GitHub Actions |

There are **no repository secrets to set.** `secrets.GITHUB_TOKEN` is the only
secret any workflow references, and Actions provides it.

`CLUSTER_FLEET_ONLINE` gates every job that schedules onto your runners. It is a
variable rather than something inferred because a job queued against a runner
that does not exist blocks the workflow for ever, and this repository would
rather skip honestly than hang.

With that set, `images.yml` runs T2 on the real fleet, `pages.yml` mirrors the
browser client onto the storage node, and `promote.yml` will accept a tag.

## 9. Release

```sh
git tag promote/v1 && git push origin promote/v1
```

**This is the first point at which promotion can succeed, and step 8 is why.**
The workflow refuses a commit whose T2 was skipped, T2 runs on a self-hosted
runner, and the runner only exists once the fleet is up and
`CLUSTER_FLEET_ONLINE` is `true`. Pushing the tag earlier fails at that check,
which is the correct outcome and not a bug to work around.

Until the first promotion, `:stable` does not exist. Nodes say so plainly ---
"nothing to follow --- no image is tagged `stable` yet" --- and stay healthy;
an unpromoted cluster is not a broken one. What they cannot do is update
themselves, so the first release is what turns §13 on.

Promotion is deliberate and human-initiated; everything after it is not. The
workflow refuses a commit whose `build`, `t1` and `t2` jobs are not green —
including one where a tier was *skipped*, because absence is not consent. It
signs the digest, moves `:stable`, builds the ISO, and publishes the ISO and its
SHA-256 as a release.

That checksum is the root of trust for a *later* install: §12.3's signature
policy ships inside the image, so a first install cannot verify itself and is
anchored by the checksum instead. Verify it out of band before mounting.

From then on, nodes follow `:stable` unattended — one at a time, testbed first
and storage last, with greenboot rolling back a boot that does not come up
healthy.

## What this guide does not establish

- **That an install has been performed end to end from this document.** Nothing
  has yet booted from an ISO built by `just installer`. T1 boots a disk image in
  QEMU and T2 boots guests on real hardware; neither exercises the installer
  path. The first real install is what tests it.
- **That your LAN resolves DHCP hostnames.** Step 5 gives a route that does not
  depend on it.
- **That the fleet performs as intended.** `SPEC.md` §21 is the standing list of
  what this repository deliberately does not claim, including that the testbed
  yields stable measurements.
