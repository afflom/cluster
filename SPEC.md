# SPEC

The repository specification for `afflom/cluster`.

This document specifies the *structure, artifacts, and processes* of the
repository: what a node is, what an image is, how one is built, published,
booted, updated, drained, and retired.

It is **not** the claim register. `model/` is the single source of every claim
(R1), and `CONFORMANCE.md` is generated from it. Restating a registered claim
here would give that claim two sources, which is exactly what R1 forbids. Where
this document needs to refer to a capability it names the ID and stops.

`AGENTS.md` defines R1 through R6 and governs every change. `VERIFICATION.md`
maps each gate onto what it discharges.

---

## 1. Scope

Three Supermicro nodes act as a hypervisor substrate for OCI workloads. The host
operating system is an appliance: it exists to run devcontainers, GitHub Actions
runners, and a small set of storage services, and to do nothing else.

The cluster's entire definition lives in this repository. There is no
configuration applied out of band, no node-local state that is not either
declared here or explicitly designated as data, and no path by which a node comes
to run software that did not pass the gate.

**In scope.** Node hardware and firmware; network topology; storage tiering;
image composition; build, test, and publish; bootstrap; unattended rolling
update with drain and automatic rollback; devcontainer session lifecycle and
reclamation; the control plane and its web interface; client access;
observability; secrets; the conformance register that holds it all accountable.

**Out of scope.** The workloads themselves. A devcontainer definition belongs to
the repository it serves. A benchmark belongs to the project being measured.

### 1.1 Architectural ceilings

Two limits are structural rather than incidental, and are stated here so they are
discovered before money is spent on them.

**n ≤ 3.** The direct-attached mesh in §4 requires one 10 GbE port per peer.
Each node has two. A fourth node needs a switch and a different `topology` kind
in `model/network.toml`.

**One migration target.** §14 moves devcontainers off the compute node during its update. The
only node that can receive them is the storage node, because the testbed is reserved (§2.3) and
receiving work would void the guarantee it exists to provide. There is therefore
no drain path for the storage node's own storage services — they are bound to a disk that is
physically inside the storage node — and §14.2 states the unavailability window rather than
pretending it away.

---

## 2. Nodes

### 2.1 Uniform hardware

Every node is a Supermicro SYS-E300-8D (X10SDV-4C-TLN2F):

| Component | Specification |
| --- | --- |
| CPU | Intel Xeon D-1518 — 4C/8T Broadwell-DE, 2.2 GHz base and 2.2 GHz max, 35 W, AVX2, **no AVX-512** |
| Memory | 32 GB DDR4 ECC RDIMM (4 slots, 128 GB ceiling) |
| Boot storage | 256 GB M.2 NVMe (PCIe 3.0 ×4) |
| Secondary storage | 256 GB SATA SSD |
| Network | 2 × 10GBase-T (Intel X552), 2 × 1GbE, dedicated IPMI port |
| Management | IPMI 2.0, SOL, virtual media |

the storage node carries one additional 2 TB 2.5" SATA HDD.

The absence of AVX-512 bounds what any measurement generalizes to. The absence of
turbo headroom — base and max clock are the same figure — is why the testbed is usable
as a measurement host: a part with a flat clock has no boost algorithm to
introduce variance.

### 2.2 Physical drive mounting

The E300-8D chassis provides limited internal mounting and the storage node needs two SATA
devices plus the M.2. **Verified physically before the storage node is provisioned.**

- **Primary:** both SATA devices internal; the SSD caches the HDD (§5.3).
- **Fallback:** the HDD takes the bay and the cache device becomes a 64 GiB
  partition of the storage node's M.2. NVMe is the faster medium; the reason it is not the
  default is contention with the OS and `/var`, not performance.

Either outcome is one line in `model/cluster.toml`. Nothing downstream branches.

### 2.3 Roles

| Role | Runs | How the node comes to hold it | Update position |
| --- | --- | --- | --- |
| `storage` | registry, object store, NFS, observability, control plane, the registrar, 2 CI runners, T2 QEMU host | **self-detected** from its own disks | 3rd |
| `compute` | devcontainers, VS Code Remote-SSH target | **assigned** by the storage node, first to register | 2nd |
| `testbed` | one measurement job at a time, nothing else | **assigned** by the storage node, second to register | 1st |

The split is load-bearing. CI is bursty and I/O-heavy; interactive editing is
latency-sensitive; measurement requires an absence of both.

The `testbed` node mounts no network filesystem, joins no shared-storage
service, and **receives no migrated workload under any circumstance.** NFS
client activity, RPC timers, and interrupt handling inject jitter into exactly
the quantity being measured. Inputs are staged before a run; results are pushed
after.

Update position follows from the role and drives §13.2's rollout predicate. The
testbed is first because a failure there costs a measurement window rather than
the pipeline; storage is last because it carries the machinery needed to
diagnose a bad update.

**No node is told which role it holds.** One image is installed on all three
machines (§8.4); an installed machine differs from its neighbours only in the
hardware it contains and the order in which it was powered on. A role written
into an image, or into a per-machine config file, is a fact in two places —
the machine and the model — and the two disagree the first time a chassis is
swapped.

#### 2.3.1 Storage is self-detected

Exactly one machine carries bulk disk. `model/cluster.toml` declares a capacity
threshold; a node holding a non-boot block device at or above it is the storage
node. The predicate is a model fact, the measurement is taken on the machine.

Detection is not a vote and does not need one: the threshold sits far above the
container-graph SSDs and far below the bulk device, so the predicate is true on
exactly one machine of a conforming fleet. A fleet where it is true on two, or
on none, is misassembled, and the registrar refuses to proceed rather than
picking one — §21.11 records why that refusal is the honest outcome.

#### 2.3.2 Compute and testbed are assigned, in provisioning order

The storage node runs a **registrar**. A node that is not the storage node
discovers it across the mesh (§3.3), presents its machine ID, and is assigned
the next free ordinal and the role that goes with it: the first to register
becomes `compute`, the second `testbed`.

The assignment is keyed on the machine ID and persisted on the storage node, so
it is made once and survives every subsequent boot in any order. Re-registering
returns the existing assignment rather than consuming a new one; a machine that
must be re-provisioned into a different role is released explicitly (§17.1).

Provisioning order is the only tie-break available. The two machines are
identical (§2.1), so nothing else distinguishes them — and inventing a
distinction, by MAC or by serial, would make the fleet's behaviour depend on a
number nobody chose.

### 2.4 Firmware settings

Configuration this pipeline cannot reach. Applied manually at bootstrap,
re-verified by `CH-` on every hardware smoke run via `ipmitool` and
`/proc/cpuinfo`.

| Setting | Value | Why |
| --- | --- | --- |
| Intel VT-x | Enabled | the storage node runs T2 guests; without it QEMU falls back to TCG |
| Intel VT-d | Enabled | required for device assignment; harmless otherwise |
| Hyper-Threading | Enabled | the testbed disables SMT via `nosmt` so isolation is expressed in one place |
| C-States | Enabled | the testbed constrains them via kernel cmdline; BIOS stays permissive so control lives in the model |
| Restore on AC Power Loss | Power On | headless cluster, no operator after an outage |
| Boot order | M.2 first, then virtual media | virtual media is the recovery path and must not be default |
| Console Redirection | Enabled, COM2/SOL, 115200 8N1 | matches `console=ttyS1,115200` |
| Watchdog | Enabled, reset action | a hung node recovers without an operator, which unattended update requires |
| BMC firmware | Latest | old firmware is Java-only iKVM; HTML5 matters when the client is a Chromebook |

### 2.5 Power

No UPS integration, no coordinated shutdown on power loss. A deliberate v1
limitation, recorded rather than hidden: the cluster tolerates hard loss because
root is read-only, `/var` is journalled XFS, and the only write-heavy device is
under a **writethrough** cache (§5.3) with no dirty-data window.

Nodes power on after AC restore. the storage node carries a 30-second BMC power-on delay so
the storage node is not competing for inrush, and so the compute node and the testbed find their
registry and NFS already answering.

---

## 3. Network identity

### 3.1 Interfaces are classified by link speed, never by name and never by MAC

**MAC addresses are arbitrary and appear nowhere in the model.** They are
assigned by whoever made the card, they change when a mainboard is replaced, and
a fleet that records them makes replacing hardware an edit to a file in a
repository rather than an operation on a machine.

An earlier revision of this document recorded four MAC addresses per node and
matched every `.network` file on `MACAddress=`. That made interface identity a
model fact at the cost of making every chassis swap a code change, and it is
withdrawn. §21.12 records what was given up with it.

What distinguishes the ports is what they are wired to, and that is a physical
property of the machine:

| Class | How it is recognised | MTU | Wired to |
| --- | --- | --- | --- |
| `mesh` | supports 10GBase-T | 9000 | another node, directly |
| `lan` | does not, and has carrier | 1500 | the 8-port switch |

The classifier reads each interface's **supported link modes** from its driver,
not its negotiated speed: a 10GbE port with no cable in it reports no speed at
all, and a mesh port is down precisely when the peer it is waiting for has not
booted yet. Supported modes are a property of the card and are readable whether
or not anything is plugged in.

A conforming machine presents exactly two mesh ports and at least one LAN port
(§2.1). One that does not is misassembled, and the classifier fails the boot
rather than guessing — a node that quietly configured one mesh port would join
the cluster with no redundancy and nothing would say so.

The second 1GbE port is unconfigured — a spare for physical fault diagnosis. It
is recognised as a LAN-class port and deliberately left without an address;
`lan` is the class, and the first such port with carrier is the one that
carries the management plane.

### 3.2 Planes

| Plane | Interface class | MTU | Addressing | Reachability |
| --- | --- | --- | --- | --- |
| Management | `lan` (1GbE) → 8-port switch | 1500 | DHCP | LAN, SSH, Tailscale |
| Mesh | `mesh` ×2 (10GBase-T, direct) | 9000 | derived (§4.1) | node-to-node only |
| BMC | dedicated IPMI port | 1500 | out-of-band | isolated VLAN, **never routed to WAN** |

Management is **DHCP**. A static address is a per-machine fact, and this
revision has none: the machine that answers to a given address is whichever one
the switch and the DHCP server agreed on. Nothing in the cluster reaches a node
by its management address — §4.3's names resolve to mesh loopbacks — so the
management plane needs only to be reachable, not predictable.

The BMC is out-of-band. It is not a host interface, the host cannot see it, and
it is not classified: it is configured in firmware (§2.4) and reached from the
LAN. BMC isolation is not optional. X10-generation BMCs have a poor CVE history
and are the one component this pipeline cannot update.

### 3.3 Peer discovery

A node knows it has two mesh ports. It does not know which peer is on the far
end of either, and it cannot be told: that is a property of how somebody ran the
cables.

Each mesh port is brought up with IPv6 link-local addressing only — always
available on an Ethernet link, requiring no allocation and no agreement — and
the node announces itself to the link-local all-nodes multicast address on that
port alone. A direct-attached segment has exactly two endpoints, so the only
listener is the peer.

The announcement carries the machine ID and, once known, the ordinal and role.
What comes back identifies the machine at the other end of *that specific cable*,
which is the fact the addressing in §4.1 needs and the only fact a node cannot
derive on its own.

**An unpeered port is not the same failure for every machine.** The storage node
knows its ordinal from its own disks (§2.3.1), so a cable with nothing on the far
end costs it that one link's addresses and nothing else: it comes up, serves the
registrar, and the link takes its addresses when the machine on the other end
registers. Any other machine has no ordinal without an answer, so the same
silence is fatal to it — there is no address it could take and no role it could
start.

That asymmetry is what makes §12.1's promise true. The first machine powered on
is necessarily alone, and a system where being first was an error could not be
brought up at all.

Discovery is not authentication. A rogue device spliced into a direct-attached
cable inside the chassis' own rack is outside this threat model, as §4.4 already
states for the mesh as a whole. §21.13 records this rather than leaving it to be
inferred.

---

## 4. Mesh

### 4.1 Topology

```
            node1
             /  \
      /31   /    \   /31
           /      \
       node2 ──── node3
              /31
```

Addresses are **derived from ordinals**, and ordinals are assigned by the
registrar (§2.3.2). The model declares two bases and the arithmetic; no address
is written down against a machine.

| Quantity | Derivation | Ordinal 1 | 2 | 3 |
| --- | --- | --- | --- | --- |
| Loopback | `loopback_base + ordinal` | `10.10.255.1/32` | `10.10.255.2/32` | `10.10.255.3/32` |

| Link | Derivation | Prefix | lower | higher |
| --- | --- | --- | --- | --- |
| `1 ↔ 2` | `link_base + 2·index` | `10.10.0.0/31` | `.0` | `.1` |
| `1 ↔ 3` | " | `10.10.0.2/31` | `.2` | `.3` |
| `2 ↔ 3` | " | `10.10.0.4/31` | `.4` | `.5` |

The link index is the position of the unordered ordinal pair in ascending order,
so both ends compute the same prefix from the same two numbers without
exchanging it. **The lower ordinal takes the even address.** A `/31` carries no
network or broadcast address (RFC 3021), so its two hosts are exactly its two
endpoints, and the parity rule is what makes the assignment agree without
negotiation.

Which physical port carries which link is discovered (§3.3) and never assumed.
A node learns the peer's ordinal across a cable and only then knows what address
to put on the port that cable is in — which is why the addressing here is stated
as arithmetic over ordinals rather than as a table of interfaces.

MTU 9000. Every mesh service binds its node's loopback, which decouples service
addressing from which link carries a flow and makes reachability assertions
meaningful.

### 4.2 Routing and single-link failure

A triangle with only direct routes is not resilient: one failed link partitions
two nodes that can physically reach each other through the third. Each node holds
two routes to each peer loopback:

| Route | Metric |
| --- | --- |
| direct link to that peer | 100 |
| via the remaining peer (transit) | 200 |

`net.ipv4.ip_forward=1` on all three. `systemd-networkd` withdraws routes on
carrier loss, so failover needs no daemon: the 200-metric route takes over at the
cost of one hop.

The route table is **derived** from `model/network.toml`, never written.
Failover is a `CN-` capability, tested in T2 by detaching a guest netdev.

### 4.3 Name resolution

No DNS service. `/etc/hosts` is written from the registry, on every node, once
the ordinals are known. Three nodes do not justify a resolver, and a hosts file
has no failure mode of its own.

| Name | Resolves to |
| --- | --- |
| `devcluster` | the storage node's loopback |
| `node1.devcluster` | ordinal 1's loopback |
| `node2.devcluster` | ordinal 2's loopback |
| `node3.devcluster` | ordinal 3's loopback |

The cluster domain is `devcluster` and the bare name is the cluster's entry
point: it is where the control plane answers, and a client that knows only
`devcluster` can find everything else from there.

Names are on **ordinals, not roles**. `node2.devcluster` is the second machine
to have registered and stays that machine; it does not move if a role is
reassigned. A name that tracked a role would change hosts under a client that
was using it, and the whole point of a stable name is that it does not.

Management addresses have no names. They are handed out by DHCP (§3.2), so
there is nothing stable to name, and nothing in the cluster needs one.

**This file is rendered, and it is the same on every machine.** Nothing in it
depends on which chassis is reading it: every name maps to an ordinal, every
ordinal derives its own loopback (§4.1), and `devcluster` maps to ordinal 1
because the storage role pins that ordinal (§2.3.2). A machine does not need to
know which node it is in order to know where all three are.

That is worth stating because the neighbouring artifacts are the opposite. The
`.network` files cannot be rendered — a machine's own address depends on the
ordinal it was assigned, and which port carries which link depends on how the
cables were run (§3.3). The rule is not "runtime because identity is
discovered"; it is that a file is rendered when its content is a fact about the
*fleet* and written at boot when it is a fact about *this machine*.

### 4.4 Firewall

`nftables`, rendered from `model/network.toml`. Default `drop` on input.

| Plane | Accepted |
| --- | --- |
| `mgmt` | 22/tcp and 9100/tcp from the LAN prefix; ICMP echo |
| `tailscale0` | 22/tcp; 443/tcp to the control plane on the storage node; established |
| mesh | all traffic between the six link addresses and three loopbacks |
| lo | all |

The mesh is trusted in full because it is a physically isolated L2 with exactly
two endpoints per segment. That trust is a property of §4.1's topology and is why
§1.1's ceiling matters: a switched mesh would invalidate it.

### 4.5 Off-LAN access

Tailscale on all three nodes, tagged `tag:cluster`, the storage node advertising the
management subnet. Auth keys are ephemeral single-use, delivered at install
(§12.2). ACLs permit only the operator's tagged devices; the mesh is never
advertised.

---

## 5. Storage

### 5.1 Disk layout

M.2 NVMe, GPT, identical on all nodes:

| Partition | Size | Format | Mount |
| --- | --- | --- | --- |
| `p1` | 1 GiB | vfat | `/boot/efi` |
| `p2` | 1 GiB | xfs | `/boot` |
| `p3` | remainder | xfs | `/` (ostree deployments and `/var`) |

bootc uses ostree deployments, not A/B partitions: two deployments — current and
rollback — share `p3`. No slot arithmetic.

SATA SSD, per role:

| Node | Use |
| --- | --- |
| storage | LVM PV, cache pool for `vg_data` |
| compute | xfs at `/var/lib/containers` and `/var/lib/docker` |
| testbed | xfs at `/var/bench` |

the storage node HDD: LVM PV, origin LV `lv_data` in `vg_data`, cached by the SSD.

### 5.2 The bootc filesystem contract

`/etc` and `/var` are writable and persist across updates; everything else is
read-only. Container graph storage, runner state, session records, and
measurement output live under `/var` and survive image transitions with no
explicit persistence declaration. `/etc` is three-way merged on update, which is
why nothing this repository owns is written to `/etc` at runtime.

### 5.3 The cache, and why writethrough

The 2 TB device is a spinning disk: ~100–120 random IOPS, ~100 MB/s sequential.
It is never a hot tier.

| Parameter | Value |
| --- | --- |
| Mode | **writethrough** |
| Policy | `smq` |
| Chunk | 256 KiB |
| Cache device | 256 GB SATA SSD (or 64 GiB M.2 partition, §2.2) |

Writeback would be faster on write and would make a single non-redundant SSD a
data-loss mode for the entire 2 TB origin. Writethrough gives the read
acceleration this workload needs and adds no failure mode — and it is what makes
§2.5's tolerance of hard power loss true, which unattended reboots (§13) depend
on.

`dm-cache` over ZFS because it is in-tree: a kernel bump carried by a new image
must not require rebuilding an out-of-tree module inside an immutable host.

### 5.4 Services on `lv_data`

| Service | Software | Purpose |
| --- | --- | --- |
| Registry | **Zot** | hosts this repository's images, mirrored from GHCR; pull-through caches `docker.io` and `ghcr.io` |
| Object store | **Garage** | S3-compatible; `sccache` backend, replaces `actions/cache` WAN round-trips |
| NFS | `nfs-utils` | devcontainer durable volumes and home directories, exported to the compute node only |
| Control plane | `cluster-ctl` | session registry, rollout state, API (§16) |

Zot syncs `ghcr.io/afflom/cluster/*` every 5 minutes. Nodes poll Zot for update
targets (§13.1) and fall back to GHCR directly when Zot is unreachable — which it
is, by design, during the storage node's own reboot.

NFS is exported to the compute node's loopback alone with `sec=sys`, acceptable only because
§4.4 makes the mesh a closed segment.

### 5.5 Retention

| Data | Policy |
| --- | --- |
| ostree deployments | 2 (current + rollback), enforced by `bootc` |
| Container images | weekly `podman system prune --filter until=168h` |
| Registry blobs | weekly Zot GC; untagged manifests older than 30 days removed |
| Journald | `SystemMaxUse=2G` |
| Prometheus | 30 days |
| Devcontainer sessions | §15.3 |
| Measurement output | never pruned; §5.6 |

### 5.6 Backup, stated honestly

`lv_data` is a **single copy**, with no on-cluster replica and no off-site target.
Tolerable only because of what is allowed to live there:

| Data | Recoverable? | How |
| --- | --- | --- |
| Registry blobs | Yes | re-pullable from GHCR and upstream |
| Build caches, `sccache` | Yes | regenerated |
| Devcontainer volumes and archives | Yes | `restic` snapshots on `lv_data`; the source of truth is the git repository the container serves |
| Session records | Yes, lossily | reconstructible from running containers |
| Prometheus history | Yes, lossily | accepted loss |
| **Measurement output** | **No** | committed to this repository under `results/` |

Measurement output is the only irreplaceable artifact the cluster produces and it
is small. It is committed, which makes git the backup and makes provenance a
property of the same history that produced the image it was measured on.

If anything else irreplaceable comes to live on `lv_data`, this section is what
has to change first.

---

## 6. Repository layout

```
afflom/cluster
├── AGENTS.md  SPEC.md  VERIFICATION.md  CONFORMANCE.md (generated)
├── Cargo.toml Justfile deny.toml clippy.toml rust-toolchain.toml
│
├── model/
│   ├── ids.toml  ledger.toml  authorities.toml
│   ├── cluster.toml           nodes, roles, MACs, update positions, storage tiers
│   ├── network.toml           planes, links, routes, firewall
│   ├── images.toml            variants, base digest, runtime, packages, units, kargs
│   └── policy.toml            rollout timings, drain budgets, GC thresholds
│
├── features/suites/
│   definition  image  boot  network  storage  workload
│   update  reclaim  control  lifecycle  hardware        (.feature)
│
├── crates/
│   ├── model/                 template: parses model/*.toml, generates CONFORMANCE.md
│   ├── conformance/           template: BDD runner and honesty meta-gate
│   ├── cluster-model/         typed model; renderers
│   ├── cluster-health/        the health predicate (§10.1); ships in every image
│   ├── cluster-updater/       rollout predicate, drain, apply (§13, §14)
│   ├── cluster-ctl/           control plane: sessions, rollout state, API (§16)
│   ├── cluster-web/           Leptos SPA, wasm32; published to Pages (§16.3)
│   └── cluster-harness/       QEMU orchestration; SSH assertions over an inventory
│
├── xtask/
├── images/{base,n1,n2,n3}/Containerfile
├── generated/                 RENDERED from model/, committed, diff-gated
├── bootstrap/{config.toml,kickstart.ks}
├── results/                   measurement output, committed
└── .github/workflows/
    ci.yml  honesty.yml  images.yml  promote.yml  pages.yml  smoke.yml
```

---

## 7. The model, and R1 over infrastructure

### 7.1 New model files

Four files beyond the template's three. Hardware uniform across nodes is declared
once as a profile and referenced.

`model/policy.toml` carries every tunable that governs unattended behaviour —
poll interval, jitter, drain budgets per workload class, greenboot deadline, GC
thresholds. These are the numbers most likely to be tuned in anger at 2am, and
they belong in the model rather than scattered through unit files.

The `spec` tag moves from `template/1` to `cluster/1`. **The template parses
`spec` into a `String` and never checks it** — a version marker that cannot catch
a version skew. `check-model` gains that assertion in the same commit.

### 7.2 Rendering

The template applies R1 to documentation. **This repository extends it to every
infrastructure artifact.**

| Rendered artifact | Source |
| --- | --- |
| `systemd-network/*.network` (MAC-matched, routes, metrics) | `network.toml`, `cluster.toml` |
| `nftables.conf`, `hosts` | `network.toml`, `cluster.toml` |
| `containers-systemd/*.{container,volume,network}` | `images.toml` |
| `kargs.d/*.toml` | `images.toml` |
| `bootstrap/kickstart.ks`, `ssh_config` | `cluster.toml` |
| updater and GC timer units and their env | `policy.toml` |

The tree is committed and diff-gated. `just render` writes it; `cargo xtask
check-render` fails when it disagrees with the model. A hand-edited `.network`
file is the same class of error as a hand-edited `CONFORMANCE.md`.

Every rendered file carries a generated header naming the IDs that assert over
it, mirroring `CONFORMANCE.md`'s `@generated` marker; `check-render`
cross-references those against the register, so rendering an artifact nothing
asserts about is a failure rather than a silent gap.

Two consequences: the Rust gates become load-bearing rather than passing green
over content they never read, and the gate is falsifiable — plant a hand-edit,
watch it fire, record it.

---

## 8. Images

### 8.1 Base, pinned by digest

`quay.io/centos-bootc/centos-bootc:stream10`, recorded in `model/images.toml` as
a `sha256:` digest, **never as the floating tag**. A repository this careful
about digest-pinning downstream cannot float its upstream. A scheduled workflow
opens a PR bumping the digest weekly; the bump passes the full gate.

CentOS Stream 10 over Fedora bootc for kernel stability: a measurement
environment that moves underneath a longitudinal series silently invalidates
comparisons across it.

Base adds: OpenSSH (key-only), podman, chrony, `node_exporter`, Tailscale,
`nftables`, **greenboot**, `cluster-health`, `cluster-updater`, the rendered
networkd and hosts files, and `console=ttyS1,115200`.

### 8.2 Container runtime — a model row, not a bet

Docker CE packages for EL10 may or may not exist at any given build. The runtime
is declared per variant:

| Value | Meaning |
| --- | --- |
| `docker` | Docker CE from the official EL10 repository |
| `podman-compat` | podman with `podman.socket`, `podman-docker` shim, `DOCKER_HOST` set |

The build **fails loudly** if the declared runtime's packages are unavailable; it
never silently substitutes. `CI-` asserts the declared runtime is present and its
socket answers a Docker API version ping; `CW-` asserts `devcontainer up`
succeeds against it. Either value passes both.

`podman-compat` is the default until EL10 Docker packages are confirmed.
Infrastructure services use podman with Quadlet regardless, because Quadlet files
ship inside the image under `/usr/share/containers/systemd/` and materialize as
units at boot — the bootc grain exactly.

### 8.3 SELinux

`enforcing`, targeted, and it stays that way.

- Every Quadlet volume mount carries `:Z` or `:z`, declared in `model/images.toml`
  and rendered, never hand-written.
- `container-selinux` in base.
- Custom policy is compiled at **image build time** and shipped in
  `/usr/share/selinux/`, loaded by a `oneshot` unit ordered before workloads.
  Nothing compiles policy at runtime on a read-only root.
- `CB-` asserts `getenforce` is `Enforcing` and the audit log holds no AVC
  denials after boot settles. A denial is a build failure, not a warning.

### 8.4 One image, three roles

**There is one image.** It is installed unmodified on all three machines, and
what a machine does with it is decided by the role it discovers at boot (§2.3).

Three images differing only by which units they enable was three artifacts to
build, sign, promote, scan and roll back, for a difference a `ConditionPathExists=`
expresses in one line. It also made installation an act of choosing, and the
thing being chosen — which machine is which — is exactly what the machine can
work out for itself.

So the image carries the union of what the three roles need:

| Role | What it enables | Gated on |
| --- | --- | --- |
| `storage` | `lvm2`, `nfs-utils`, `restic`, Quadlets for Zot/Garage/NFS/Prometheus/Grafana/Alertmanager/`cluster-ctl`, the registrar, 2 CI runner Quadlets, GC timer, QEMU + libvirt for T2 | `/run/cluster/role.storage` |
| `compute` | devcontainer CLI prerequisites, NFS client, devcontainer agent, the tunnel Feature (§11.1) added to every session | `/run/cluster/role.compute` |
| `testbed` | isolation kargs (§8.5), 1 bench runner Quadlet, **no NFS client, no Tailscale subnet routing, no migration receiver** | `/run/cluster/role.testbed` |

`cluster-init` writes exactly one `/run/cluster/role.*` marker once the role is
known, and every role-specific unit carries `ConditionPathExists=` naming it. A
unit whose condition is unmet is skipped, not failed — which is what lets the
same image boot cleanly into any of the three.

The markers are under `/run` deliberately. A role is re-derived on every boot
from the machine's own hardware and the registrar's answer; persisting the
marker would let a stale file outvote the machine it describes.

The cost is a larger image: every machine carries QEMU and `lvm2` whether or not
it runs them. That is paid once in bytes on an M.2 and recovered in every
promotion, rollback and scan that now has one artifact to reason about instead
of three. §21.14 records it rather than pretending the trade is free.

### 8.5 Testbed kernel arguments

```
isolcpus=2,3 nohz_full=2,3 rcu_nocbs=2,3 nosmt
intel_idle.max_cstate=1 processor.max_cstate=1
```

with the governor pinned to `performance` and IRQ affinity steered away from the
isolated set by a `oneshot` unit.

These cannot ship in `/usr/lib/bootc/kargs.d/`: that directory is part of the
image, the image is now shared by all three roles, and isolating two cores on
the storage node would cost half its CPU to no purpose.

So they are applied **after the role is known**, with
`bootc loader-entries set-options-for-source --source cluster-role`. The
arguments become a tracked source in the BLS entry, merged with the image's own
and re-merged on every upgrade, and a node that is no longer the testbed drops
them by setting the same source empty. One reboot follows the first assignment,
and the marker that records it means it does not repeat.

That these are present and that `/sys/devices/system/cpu/isolated` reflects them
is constructible and testable. That the environment yields stable measurements is
neither, and §21 records why.

---

## 9. Build, validate, publish

### 9.1 Visibility, and what follows from it

**This section's premise does not currently hold.** The repository is *public*.
§21.10 records that, and the two consequences below are written as they were
derived --- from a private repository --- because the derivation is what has to
change, not the wording. Until it does, the workflows guard the exposed half by
refusing to schedule a self-hosted job for a pull request from a fork.

**The repository is private, and its GHCR packages are private.** Two things
follow:

1. **Fork pull requests do not exist**, so self-hosted runners are not exposed to
   untrusted code. This is the precondition that makes T2 on the storage node acceptable.
   Going public would require moving T2 to hosted runners.
2. **Nodes authenticate to pull.** A fine-grained PAT with `read:packages` only,
   delivered at install into `/etc/containers/auth.json` (§12.2).

Secrets never go into an image — not because images are public, but because an
image is readable by everything holding pull access.

### 9.2 The invariant

**Build once, validate that digest, promote that digest.** Never rebuild between
validation and publication.

```
podman build ──► ghcr.io/afflom/cluster/<node>:ci-<sha>
                          │  capture sha256:…
                          ▼
                  T0 ─► T1 ─► T2      all against the digest
                          │
                  (git tag promote/<date>)
                          ▼
        cosign sign ─► crane copy sha256:… ──► :stable
                          │
                          ▼
                  GitHub Release published  ──►  §13 takes over
```

### 9.3 Promotion is tag-triggered; the release is the authorization

Promotion is deliberate and human-initiated; everything after it is not.

An operator pushes a tag `promote/<date>` onto a commit whose T0, T1, and T2 are
green. `promote.yml` resolves the tag to that commit, signs the three digests
built from it, copies each to `:stable`, and **publishes a GitHub Release** whose
body records the three digests and the source commit.

The release is the operator's authorization and the human-readable record. The
`:stable` digest moving is the machine-readable event, and §13 is what consumes
it. CI never commits to the repository; a bot commit would trigger CI on itself
and complicate branch protection for no gain.

`concurrency: { group: promote, cancel-in-progress: false }` serialises
promotions.

### 9.4 Where the work runs

| Stage | Runner | Why |
| --- | --- | --- |
| build | GitHub-hosted | faster than a D-1518; keeps the cluster out of its own critical path |
| T0, T1 | GitHub-hosted | static and cheap; T1 needs KVM (below) |
| T2 | self-hosted on the storage node | real KVM, 2 TB of disk, no emulation tax |
| promote | GitHub-hosted | the publish path never depends on the cluster |
| Pages build | GitHub-hosted | §16.3 |

T2 on the storage node is not circular: the storage node runs the *previously promoted* image while
validating the candidate in guests.

**KVM.** GitHub documents nested virtualization on hosted runners as technically
possible but not officially supported, with no guarantee of stability,
performance, or compatibility, and `/dev/kvm` availability is reported as
inconsistent on free runners. The harness probes for `/dev/kvm` and reports its
absence as an **explicit skip**, never a silent TCG fallback. T1 may skip; T2
runs where KVM is guaranteed, so nothing is promoted on a skipped tier.

**Disk.** Hosted runners provide roughly 14 GB free, which does not hold three
bootc disk images. The harness uses one base qcow2 with per-node copy-on-write
overlays, and the workflow reclaims space by removing preinstalled toolchains.

### 9.5 Runner fleet

| Node | Count | Labels | Mode |
| --- | --- | --- | --- |
| storage | 2 | `self-hosted,linux,x64,cluster,ci` | `--ephemeral`, Quadlet-managed, re-registering |
| compute | 0 | — | the interactive node stays uncontended |
| testbed | 1 | `self-hosted,linux,x64,cluster,bench` | `--ephemeral`, systemd concurrency lock of 1 |

---

## 10. Validation

### 10.1 The health predicate

`assert healthy` appears throughout this document, in the rollout predicate, in
greenboot, and in three test tiers. It is defined once and shipped as
`/usr/bin/cluster-health` in the base image. Non-zero exit on any failure, JSON
on stdout:

1. `systemctl is-system-running` returns `running`, not `degraded`
2. `systemctl --failed` is empty
3. `bootc status --json` reports the expected image digest
4. every declared mesh peer loopback answers, and `ping -M do -s 8972` succeeds
   on each path
5. every Quadlet declared for this node's role is `active`
6. `/usr` is read-only and `/var` writable
7. `getenforce` is `Enforcing`, no AVC denials since boot
8. chrony synchronised, offset < 100 ms

**Five consumers, one predicate:** T1, T2, T3, greenboot's required check
(§13.3), and the rollout precondition (§13.2). It is also served over HTTP on the
mesh loopback at `:9101/health`, which is how nodes observe each other without a
lock (§13.2) and how §18 alerts on drift.

### 10.2 Tiers

| Tier | When | Duration | What |
| --- | --- | --- | --- |
| T0 | every PR | ~2 min | render diff, `bootc container lint`, `systemd-analyze verify`, runtime and package assertions inside the built container |
| T1 | every PR | ~8 min | one node boots under OVMF; `cluster-health` passes |
| T2 | every PR **and** nightly | ~35 min | three nodes, mesh wired, failover, cross-node features, a full simulated rollout with drain and rollback (§13.6) |
| T3 | after each promotion | ~5 min | the same suite against real hardware; plus firmware verification (§2.4) |

T2 runs on every pull request, not only nightly: §9.3 requires it green on the
promoted commit, and a nightly-only tier could not discharge that.

### 10.3 QEMU topology

QEMU socket netdevs give point-to-point links with no bridges, taps, or
privilege:

```
n1:  -netdev socket,id=l12,listen=:11200
n2:  -netdev socket,id=l12,connect=127.0.0.1:11200
```

Three socket pairs reproduce the `/31` triangle; a user-mode netdev per guest
provides management and outbound; OVMF supplies UEFI. This is a faithful test of
the routing configuration — including §4.2's failover, exercised by detaching a
netdev mid-run.

### 10.4 The transition is the test

A fresh boot proves an image is buildable. What is done to hardware is an
*upgrade*, and that is where breakage lives: SELinux relabels, `/etc` three-way
merge conflicts, storage migrations.

```
boot :stable ─► bootc upgrade ─► candidate ─► reboot ─► cluster-health
                              ─► bootc rollback ─► reboot ─► cluster-health
```

Both directions. An untested rollback is not a recovery path — and under §13 it
is a path taken without an operator present.

---

## 11. Client access

The primary client is a Chromebook, which cannot build images and should not be
assumed to run anything but a browser and an SSH client. **The developer's
working environment is a devcontainer on the compute node, not the Chromebook.**

### 11.1 Paths

**The primary path is a VS Code Remote Tunnel running inside the container.**

One correction has to be stated because the obvious reading is wrong:
**`vscode.dev` cannot run the Dev Containers extension.** "Open vscode.dev and
reopen in container" is not a path. The tunnel runs *inside* the devcontainer,
not on the compute node, and what the browser connects to is an editor already in the
workspace.

| Path | Use |
| --- | --- |
| **Primary** — `code tunnel` inside the container; browser connects via vscode.dev | no local install beyond a browser; works off-LAN with no VPN |
| **Recovery** — `ssh dc-<session-id>` | retained deliberately; see below |
| **Web** — the Pages UI (§16) | create, start, stop, migrate, and reclaim sessions |
| **Off-LAN** — Tailscale | the management plane and the control plane; the storage node advertises the management subnet |

The tunnel is named `dc-<session-id>`, the same identifier as the SSH alias, so
one name addresses a session on both paths. The URL is the documented form:

```
https://vscode.dev/tunnel/dc-<session-id>/<folder>
```

The Pages UI's "Open" control is an anchor to that URL. `cluster-web` contains no
editor code, embeds nothing, and proxies nothing.

**The tunnel is injected without touching the workload repository.** §1 places
`devcontainer.json` out of scope — it belongs to the repository it serves —
so `cluster-ctl` never modifies it. Instead this repository publishes a
devcontainer Feature, `ghcr.io/afflom/cluster/features/tunnel:1`, and
`cluster-ctl` adds it at creation with `devcontainer up --additional-features`.
The tunnel becomes a property of how *this cluster* runs containers rather than
of any project, which is what keeps §1's boundary intact.

**SSH is retained, and not as a formality.** The VS Code CLI's stored token is a
single point of failure for every container at once: if refresh ever fails hard,
all tunnel access is lost until an interactive device-code login. SSH is the
recovery, and it costs nothing to keep working. Host aliases are
`dc-<session-id>`, rendered into `generated/ssh_config` with a `ProxyCommand`
that resolves the session's **current** host from the control plane, so a session
that has migrated (§14.3) is reachable at the same alias without the client
knowing it moved.

**Forwarded ports default to private.** Dev tunnels can forward a container port
and produce a `*.devtunnels.ms` URL reachable from the Chromebook. Visibility is
per-port, and the default here is private: a development server bound inside a
container is not a thing to publish by accident.

### 11.2 Devcontainer storage

Workspaces live on the compute node's local SATA SSD; the NFS export from the storage node holds durable
volumes, home directories, and workspace mirrors (§14.3). Git runs on local disk.
`overlay2` and podman's `overlay` driver do not function on NFS, which is why the
container graph is local on every node and NFS carries data only.

---

## 12. Bootstrap and secrets

### 12.1 Bootstrap

Once per node:

1. Apply §2.4 firmware settings; update BMC firmware.
2. `bootc-image-builder` produces an installer ISO from the promoted image. Its
   SHA-256 is published in the release and verified out-of-band. **This is the
   root of trust:** §12.3's signature policy ships inside the image, so the first
   install cannot verify itself and is anchored by the checksum instead.
3. Mount via IPMI virtual media; the rendered kickstart partitions per §5.1 and
   injects **nothing**. It carries no credentials: the ISO is a release artifact
   and this repository is public (§9.1, §12.2).
4. Reboot. The node comes up unenrolled — no SSH key, no registry token, no
   tailnet.
5. Open the storage node's control plane in a browser over the LAN, authenticate
   with the GitHub App device flow, and enter the secrets §12.2 declares. This
   is done once for the cluster, not once per machine: the values are stored on
   the storage node and the others receive what they need over the mesh.
6. `cluster-health` must pass before the node is considered provisioned.

**The same ISO is used for every machine, and the order matters.** There is one
image (§8.4), so there is one installer and nothing to select at install time.
What the operator does choose is the order: the machine carrying bulk disk is
brought up first, because it is the registrar and the other two cannot obtain an
ordinal until it answers. Of the remaining two, the first powered on becomes
`compute` and the second `testbed` (§2.3.2).

A machine booted before the registrar answers does not fail. It retries with
backoff, holding no ordinal and starting no role-gated unit, and joins when the
registrar appears — so an operator who powers all three on at once gets a
working cluster, with the compute/testbed assignment decided by whichever won the
race. Order is how you *choose*; it is not a precondition.

From that point IPMI is used only for power control and post-mortems.

### 12.2 Secrets

**Secrets reach a cluster through the browser, after it boots.** Not through
the ISO, and not through an Actions secret substituted into one.

That was the previous design and it could not have worked. The Actions secrets
it named did not exist — `secrets.GITHUB_TOKEN` is the only secret any workflow
has ever referenced — so a node would have installed the literal string
`@@AUTHORIZED_KEY@@` as root's authorized key, on a headless machine with no
console, and then died at `tailscale up --erroronfail`, taking the install with
it. And the shape was wrong even with the secrets present: an ISO is a release
artifact, this repository is public (§9.1), and a secret substituted into a
release is a secret published to whoever downloads it.

So a node installs **unenrolled**. It has the control plane and nothing else.
The operator reaches it over the LAN, authenticates with the GitHub App device
flow — the one credential that can be checked without any of the others
existing — and enters the rest. §16.2's authorization is what makes this a
bootstrap path rather than an open door.

`model/policy.toml` declares each secret by **destination**, never by value:
where it lands, at what mode, what applying it does, and in what **format** it
is written. `CD-20` asserts the rendering, `CD-21` asserts the format, `CL-08`
asserts no artifact carries a placeholder nothing fills, and `CC-09` asserts the
control plane hands none of them back.

Format is a separate question from destination, and conflating the two shipped a
credential that could not have worked. An operator enters a *credential*; most
destinations want exactly that credential, and one wants a document built around
it. `/etc/containers/auth.json` is parsed by podman as JSON, so the bare token
written there failed every pull — unattended, at the next update, three layers
from its cause.

| Format | What is written |
| --- | --- |
| `raw` | the entered value, and a newline |
| `docker-auth` | a containers-auth document keyed by the declared registry, whose username is the login the device flow authenticated |

The username being the authenticated login is not a convenience. The operator
entering a GHCR token is, by construction, authenticated as the GitHub account
that token belongs to (§16.2) — so the pair ghcr.io wants is already in hand and
is never a second thing to enter.

| Secret | Lives in | Reaches a node | Rotation |
| --- | --- | --- | --- |
| SSH authorized key | operator's device | browser, at enrolment | on device change |
| GHCR read PAT | operator's GitHub account | browser, at enrolment | quarterly |
| GitHub App private key | Actions secret | never reaches a node | annually |
| Tailscale auth key | operator's tailnet | browser, ephemeral single-use, spent on entry | per install |
| Runner registration token | Actions secret | Quadlet env file, per registration | per registration |
| Garage access keys | generated on the storage node at first boot | never leave it except to CI as an Actions secret | annually |
| Cluster join secret | generated on the storage node at first boot | over the mesh, at registration (§2.3.2) | on `cluster-ctl secret rotate` |
| `cluster-ctl` session DB | `lv_data` | — | — |
| cosign | none — keyless (§12.3) | — | — |

Rotation is documented and manual, and rarely needed: a secret entered through
the browser stays on the cluster, so rotation is a decision about the credential
rather than a consequence of how it got there.

**Nothing is read back.** The control plane reports which secrets are set and
which are missing; there is no route that returns one. An operator who has lost
a token issues a new one. A control plane that would hand a credential back is
one bearer token away from handing it to somebody else.

**Enrolment is reachable on the LAN, and has to be.** A machine that has just
installed has no tailnet — the auth key is one of the things not yet given —
and no client is on the mesh. §4.4 opens the control-plane port to the LAN
prefix for the storage role, the same prefix that already reaches SSH on every
node. Binding is not the boundary and never was: the packet filter is, it
defaults to drop, and it is a model fact.

**The join secret is generated, never declared.** It authenticates a node's
registration request and the tunnel sessions that follow (§11.1). It is created
by the registrar on its first boot from the kernel's random source, stored
`0600` on `lv_data`, and handed to each node when it registers. It appears in no
model file, no image, no rendered artifact and no repository — a shared secret
committed to a repository is a shared secret with everyone who can read the
repository, and this one is public (§9.1).

`check-render` enforces the absence rather than trusting it: no rendered
artifact may carry a value that looks like a key or a token, which is the same
gate `CD-07` already applies to the kickstart, applied to the whole tree.

**Two things are deliberately absent from this table.**

The GitHub App's **client ID** is not here, because it is not a secret: the
device flow uses a public client ID with no client secret, which is what lets a
static page start an authorization at all (§16.2). It lives in
`model/cluster.toml`. A table of secrets that contains a non-secret teaches its
reader to skim.

There is **no repository-scoped token on the compute node**. §16.2's browser token requests
`read:user` and cannot reach code; a container clones with credentials that
arrive over the tunnel through the browser's GitHub auth provider and die with
the connection. The credential that reaches source is per-session and never at
rest on a node, which is a stronger position than rotating a PAT quarterly.

### 12.3 Signing and verification

Images are signed in `promote.yml` with **keyless cosign** — GitHub OIDC, Fulcio,
Rekor — so there is no long-lived key to custody.

Nodes verify before staging. `/etc/containers/policy.json`, shipped in the image,
requires a sigstore signature whose OIDC issuer is GitHub and whose identity is
this repository's `promote.yml` workflow. An image signed by anything else does
not stage, including one signed by a different workflow in the same repository.

This is what makes §13's unattended update safe to run: the node applies whatever
`:stable` points at, and the policy is the only thing standing between it and an
arbitrary image. §12.1's checksum breaks the resulting chicken-and-egg at
install.

**The policy governs the registry path, and says so explicitly.** Its default is
reject, one repository is admitted over the `docker` transport under the identity
above, and the node's own `containers-storage` is accepted. The last is not a
loophole: what is already in local storage arrived either through that strict
path or from the installer, whose medium is anchored by §12.1's checksum. Without
it `bootc install` cannot read the image it was told to install --- the installer
works from local storage and the local copy carries no signature --- and the
deployment is refused outright.

---

## 13. Unattended rolling update

Nodes update themselves when a release is published. No operator is present, so
every step is either safe to take unattended or is a halt.

### 13.1 Trigger

Each node polls for the digest `:stable` resolves to, every 10 minutes with 0–120
seconds of jitter, from the storage node's Zot first and GHCR directly on failure. There is
no webhook: nodes have no inbound reachability from GitHub, and polling adds no
attack surface.

The registry is polled rather than the Releases API because the registry is what
the node would actually pull, is already authenticated, and stays available if
the API does not.

### 13.2 Ordering without a lock

A distributed lock would need a service that survives the reboot of the node
holding it — which means either consensus across three nodes or a single point of
failure that is itself one of the three. Both are more control plane than this
cluster has.

Instead, ordering is a **pure function of observable state**. Each node knows its
position from `model/cluster.toml` (§2.3) and reads every peer's
`:9101/health` (§10.1). A node at position `i` applies an update only when all of:

- `target ≠ booted` — a new digest exists
- `target` is not quarantined (§13.4)
- for every `j < i`: peer `j` reports `booted == target` **and** healthy
- for every `j > i`: peer `j` reports healthy
- no peer reports state `draining` or `updating`

By construction this is true for exactly one node in any consistent state. The
predicate is re-evaluated within 30 seconds of committing to the upgrade, and the
poll jitter makes simultaneous stale reads unlikely rather than merely improbable
in theory.

the testbed has no predecessors, so it moves first on its own. the storage node moves only when both
peers are already on the target and healthy.

### 13.3 Applying, and automatic rollback

The node publishes state `draining`, runs §14, publishes `updating`, then:

```
bootc upgrade → reboot
```

**greenboot** owns what happens next. `cluster-health` is installed as
`/usr/lib/greenboot/check/required.d/50-cluster-health.sh`, so the boot is
declared successful only if the predicate passes within the deadline in
`model/policy.toml` (default 10 minutes). On failure greenboot rolls back to the
previous ostree deployment automatically and the node reboots into it.

This is not a mechanism this repository invents. It is cited (§20.1), configured,
and tested — and it is the single reason unattended update is acceptable: a bad
image costs one reboot cycle on one node rather than a cluster.

### 13.4 Quarantine

A node that rolls back POSTs the failed digest to `cluster-ctl` as
**quarantined**. Quarantine is a precondition in §13.2, so no other node attempts
it. An alert fires immediately.

Because the testbed moves first, a bad image is normally caught by the node whose
failure costs least, and the compute node and the storage node never see it.

The exception is worth naming: if the storage node — last in the sequence — fails and rolls
back, the cluster is left split-version, with the compute node and the testbed ahead of it. That is
a legitimate, alerted state requiring a human decision (roll the others back, or
fix forward). It is not silently reconciled.

### 13.5 Halt conditions

The rollout halts and does not proceed to the next node when:

- any peer is unhealthy at the start of a stage
- a drain budget is exceeded (§14.4)
- the target digest is quarantined
- signature verification fails

A halted rollout is a recoverable state. A cluster updated on top of an unnoticed
fault is not. Halts alert at 6 hours (§18).

### 13.6 Split-version tolerance

A rollout leaves the cluster on mixed digests for tens of minutes. Everything
that crosses the mesh must work across one version boundary.

**A change that breaks interop between adjacent versions must ship as two
releases.** Changing the mesh addressing scheme, the `:9101/health` schema, the
NFS export path, or the control plane API in a single release would partition the
cluster mid-rollout. Phase one adds the new form and accepts both; phase two
removes the old. `CU-` covers this by running T2's rollout simulation from the
previous `:stable` rather than from the candidate to itself.

---

## 14. Draining

### 14.1 What can move, and what cannot

| Workload | Node | Drain strategy |
| --- | --- | --- |
| Bench job | testbed | **Wait.** Migrating a measurement invalidates it. Stop re-registering the ephemeral runner; let the in-flight job finish. |
| CI runners | storage | **Wait.** `--ephemeral` runners exit after one job; stop re-registering. |
| Devcontainers | compute | **Migrate** to the storage node (§14.3). |
| Registry, object store, NFS, control plane | storage | **Cannot move.** They are bound to `lv_data`, which is a disk physically inside the storage node. §14.2 states the window. |

Nothing migrates to the testbed, ever. Receiving work would void the isolation guarantee
it exists to provide (§2.3).

### 14.2 the storage node's unavailability window

the storage node's reboot takes its services with it, for roughly two to three minutes. This
is a stated limit, not a solved problem — solving it would require a second node
with a copy of the 2 TB disk, which does not exist.

| Impact | Mitigation |
| --- | --- |
| Image pulls fail | `registries.conf` lists local Zot first with `ghcr.io` and `docker.io` as fallbacks; pulls continue over WAN |
| NFS stalls on the compute node | hard mounts stall and recover cleanly; workspaces are on local disk (§11.2) so only durable volumes are affected |
| `sccache` misses | jobs compile without cache; correctness unaffected |
| Metrics gap | accepted; §5.6 already records Prometheus as lossy |
| Web UI unavailable | §16.5 |

By the time the storage node updates, the compute node and the testbed are already on the target and are not
pulling. The window is therefore mostly felt by whatever a developer is doing at
that moment, which is why §18 alerts before rather than after.

### 14.3 Devcontainer migration

A devcontainer's durable state is the git worktree, its declared volumes, and the
`devcontainer.json` that built it — **not** its process state.

```
quiesce         stop accepting new starts on n2; notify attached sessions
sync            rsync workspace → its NFS home on n1
stop            podman stop, with the container's declared grace period
recreate        start on n1 from the same image digest, workspace from NFS
record          update the session's current host in cluster-ctl
notify          attached clients are told to reconnect
```

**Attached editor sessions drop.** There is no way around this short of CRIU
checkpoint/restore, which is not reliable for the processes involved — VS Code
server, open TTYs, live SSH sockets. Rather than a fragile mechanism that fails
unpredictably, the spec chooses a predictable one that fails visibly.

**The tunnel URL does not change.** This is the property the tunnel path was
chosen for. A tunnel name is host-independent: the process dies with the
container on the compute node and re-registers on the storage node under the same `dc-<id>`, so
`https://vscode.dev/tunnel/dc-<id>/<folder>` still addresses the session. The
user reloads the tab.

That is strictly better than the SSH path, which needs `ProxyCommand` resolution
against the control plane to discover where the container went — and which
therefore degrades during the storage node's own window (§16.5). A `CW-` ID asserts the URL
survives migration: it is the entire benefit of this choice, and it should fail
the build if it stops holding.

The `dc-<id>` SSH alias remains the recovery path and resolves to the new host on
reconnect, so reconnection there is one command and not a lookup.

**Capacity budget.** the storage node has 4 cores and 32 GB and is already running the
storage services and two CI runners. `model/policy.toml` caps migrated
devcontainers at 12 GiB of declared memory. Beyond the cap, the excess is
**stopped with notice** rather than migrated — the session survives, the process
does not. T2 asserts the cap is enforced rather than exceeded silently.

### 14.4 Budgets

| Class | Budget | On exceeding |
| --- | --- | --- |
| Bench job | 4 h | halt rollout, alert; never kill a measurement |
| CI job | 1 h | halt rollout, alert |
| Devcontainer migration | 10 min per container, 30 min total | stop remaining with notice, continue |
| Total drain | 6 h | halt rollout, alert |

A budget is never met by force. Exceeding one halts the rollout and asks for a
human, because the alternative — killing a four-hour benchmark to install a patch
release — is worse than staying on the old image.

---

## 15. Devcontainer lifecycle

### 15.1 Session records

`cluster-ctl` holds one record per devcontainer in SQLite on `lv_data`:

| Field | Notes |
| --- | --- |
| `id` | short stable identifier; the `dc-<id>` SSH alias |
| `owner` | Tailscale login (§16.2) |
| `repo`, `ref`, `config_path` | what to rebuild it from |
| `image_digest` | what it was built from |
| `host` | current node; updated by §14.3 |
| `state` | `creating`, `running`, `stopped`, `migrating`, `archived`, `purged` |
| `created_at`, `last_attached_at` | `last_attached_at` drives §15.3 |
| `dirty` | recomputed on every stop and before every reclaim step |

**`id` is constrained, because four things consume it.** It becomes a directory
under the workspace root, a path segment in the URL the agent is asked for, a
`podman exec` container name, and the `dc-<id>` SSH alias. Lowercase letters,
digits and hyphens, not beginning or ending with a hyphen, at most 60 characters
— the intersection of four grammars, and 60 is what a hostname label leaves once
`dc-` is prepended. `CC-10` asserts it, at the control plane and again at the
agent: an identifier arrives there as a URL segment from another machine, and
trusting it because something else checked it is how a `..` reaches a path.

The constraint is what makes the rest of §15 safe to state. An unconstrained
identifier put `..` into `workspace_of`, and — because the agent built its
answers by concatenation and the control plane read them by substring — let a
crafted one make a dirty workspace report clean to the step that deletes
archives. The agent serialises its answers now and the control plane parses
them; an answer that cannot be understood is dirty (§15.2).

**`last_attached_at` has two sources, and the better one is the tunnel.**

A tunnel gives a signal that needs no log parsing and no heuristic: the tunnel
*process* runs continuously whether or not anyone is looking, but the VS Code
**server** process spawns only when a client actually connects. Its presence is
a direct statement that somebody is attached. That is the primary signal, and
the agent on the compute node samples it.

The `sshrc` hook remains for the SSH path (§11.1), which has no server process
to observe.

This field carries more weight than its size suggests: §15.3's entire retention
policy is computed from it, and a session somebody is using that looks idle is a
session archived out from under them. A `CW-` ID therefore asserts the
correlation between a connected client and a running server process rather than
assuming it.

### 15.2 Dirty is the thing that stops deletion

A workspace is `dirty` when any of: uncommitted tracked changes, unpushed
commits on any branch, or untracked non-ignored files.

Dirty is recomputed immediately before any destructive step, never read from
cache. It is the one flag that overrides the retention policy.

### 15.3 Reclamation

Idle is measured from `last_attached_at`. Thresholds live in
`model/policy.toml`; the defaults are:

| Age | Action | Reversible? |
| --- | --- | --- |
| 14 days | notify owner; session marked `idle` in the UI | — |
| 30 days | stop container; `code tunnel unregister`; `restic` snapshot of workspace and volumes to `lv_data`; remove container, volumes, and unreferenced image layers; state → `archived` | yes — restore rebuilds from the snapshot and `devcontainer.json` |
| 90 days | second notice, then delete the archive; state → `purged` | **no** |
| 90 days, `dirty` | **not purged.** Held indefinitely, listed in the UI as requiring acknowledgement. | — |

The dirty exemption is the point of the whole policy. Reclaiming resources from
an abandoned container is housekeeping; deleting someone's uncommitted work
because a timer expired is a betrayal, and a system that does it once is never
trusted again. The cost of holding a dirty archive forever is a few gigabytes on
a 2 TB disk.

**Unregistering the tunnel is part of archiving, not an afterthought.** Tunnel
names are globally unique per account, so an archive that left `dc-<id>`
registered would collide with any session later recreated under the same
identifier — and the collision would appear as an editor that will not connect,
which is a long way from its cause.

Reclamation runs as a systemd timer on the storage node, daily, and emits per-session metrics
so §18 can alert on unexpected volume.

### 15.4 Reclamation is not drain

Reclamation and drain are separate mechanisms with separate triggers, and
reclamation never runs during a rollout. A session archived because it was idle
must not be confused with one stopped because its host was updating, and
`state` distinguishes them.

---

## 16. Control plane and web interface

### 16.1 `cluster-ctl`

An axum service on the storage node, backed by SQLite on `lv_data`. It is the session
registry (§15.1), the rollout state store (§13.4), and the API the UI speaks to.

| Endpoint | Purpose |
| --- | --- |
| `GET /api/nodes` | health, booted digest, target digest, rollout state |
| `GET /api/sessions` | list, with idle age and dirty flag |
| `POST /api/sessions` | create from repo, ref, and `devcontainer.json` path |
| `POST /api/sessions/:id/{start,stop,migrate,restore}` | lifecycle |
| `DELETE /api/sessions/:id` | archive now, or purge if already archived |
| `GET /api/sessions/:id/connect` | the `ssh dc-<id>` command and tunnel URL |
| `GET /api/rollout` | current stage, budgets consumed, quarantined digests |
| `POST /api/rollout/quarantine` | called by a node after greenboot rollback |

It is a *shipped* crate under the template's definition and is therefore subject
to R5: every error a caller can see is one `model/ids.toml` sanctions. An HTTP
API is precisely the surface R5 exists for.

### 16.2 Exposure and authentication

**Identity is GitHub's, by device flow, and this repository now contains
authentication code.**

An earlier version of this section authenticated with the Tailscale identity
header and justified it on the grounds that *"there is no auth code in this
repository"*. That justification no longer holds and is not left standing:
roughly fifty lines of authentication now exist, and `CC-` claims them.

#### Authentication is not ambient

The tempting design is to lean on the operator already being signed in to
github.com. It does not work, and the reason is worth stating so nobody
reconstructs it: a github.com session is a cookie scoped to github.com. No other
origin can read it and it is never sent to the cluster. Neither the Pages site
nor `cluster-ctl` can observe that a browser is signed in.

One explicit authorization step is therefore required. What follows is
one-time-per-browser and thereafter indistinguishable from ambient {D} but the
mechanism is an authorization, not an observation.

#### A GitHub App, and the device flow

**A GitHub App, not an OAuth App.** App user-to-server tokens expire in eight
hours and carry a refresh token. OAuth App tokens do not expire, and a
non-expiring token sitting in browser storage is a permanent liability.

**The device flow, not the web flow.** Device flow uses a public client ID with
no client secret and no callback URL, which is exactly what a static page can
do. The web flow needs a secret an SPA cannot hold and a callback URL GitHub
cannot reach while `cluster-ctl` is behind Tailscale.

The client ID is public by design. It lives in `model/cluster.toml` with
everything else that decides who may drive the cluster {D} and deliberately
*not* in §12.2's table, because it is not a secret and a table of secrets that
contains a non-secret teaches the reader to skim.

#### `afflom` is a user account, not an organization

This bounds the authorization primitive and the bound is stated rather than
discovered later. A personal namespace has no membership API: there is no
`GET /orgs/afflom/members` to call. Authorization reduces to a login compared
against `authorized_logins` in `model/cluster.toml`.

Extending access to a second person requires either converting the account to an
organization, or checking collaborator status against a named repository.
Neither is in scope, and both would be foreclosed by silence if this paragraph
did not exist.

#### The browser token is identity-only

It requests **`read:user` and nothing else.**

In particular it does not request `repo` in order to populate a repository
picker. The UI takes a typed repository reference instead, and the **container**
clones using credentials that arrive through the browser's GitHub auth provider
over the tunnel {D} the same mechanism that already serves `git push` from
inside the container.

Two things follow. The long-lived browser token cannot reach code, and
repository access is a per-session credential that dies with the connection.
§12.2 reflects the second: the compute node needs no repo-scoped PAT.

#### Validation

`cluster-ctl` validates a bearer token by calling `GET /user` and caching the
token-to-login mapping. The TTL is in `model/policy.toml`; revocation lag is
bounded by it and the call volume is nowhere near a rate limit. This requires
outbound WAN from the storage node to `api.github.com`, which is a dependency worth naming
because §14.2 already lists what stops working when the storage node cannot reach the world.

#### Exposure: Serve, not Funnel

`tailscale serve` publishes `cluster-ctl` at `https://n1.<tailnet>.ts.net`, with
a real certificate from the tailnet's CA and no inbound port opened on the
management plane.

**Funnel was considered and rejected**, and the rejection is recorded because
the case for it was good. Funnel would put the control plane on the open
internet with identity alone as the barrier, which §2's GitHub App now genuinely
provides, and it would match "the client is only a browser" completely. It was
rejected because the *editor* path {D} the one that actually matters day to day
{D} already works with no tailnet: the dev tunnels relay is outbound from the
container (§11.1). Funnel's remaining gain is the management UI without a
tailnet, and the price is the control plane on the public internet. §16.3's the storage node
mirror covers the same ground without moving that line.

So the barrier is the tailnet **and** an identity, not either alone.

#### What this collapses

One identity provider across four surfaces: tunnel host registration, the
`vscode.dev` client, git operations inside the container, and the control plane.
No second account, no invented bearer secret, and no dependency on the transport
for identity.

### 16.3 The Pages UI

`crates/cluster-web` is a Leptos SPA compiled to `wasm32-unknown-unknown`,
published to GitHub Pages by `pages.yml` on every push to `main`.

Rust rather than TypeScript for one practical reason beyond uniformity:
`cargo deny` (R6) already covers the dependency graph, so the UI's supply chain
is gated by the same rule as everything else. A `package.json` would be a second
dependency graph under no rule at all.

The SPA is entirely static. Its only build-time configuration is the API base
URL, injected from a repository variable. All state comes from §16.1 at runtime.

**The same bundle is also served from the storage node.** `pages.yml` publishes to Pages and
pushes the identical artifact to the control plane, which serves it at the origin
the API is on. Pages stays canonical and stays the versioned artifact; the the storage node
copy is the path that always works, because same-origin has no CORS preflight and
no browser policy standing between a page and the API it was built for.

That mirror exists because of a constraint discovered late. Chrome 142 shipped
Local Network Access on 28 October 2025: a request from a public origin to a
private network address is gated behind a permission prompt, and denial fails
*silently*. Edge implements the same model. A page served from `afflom.github.io`
calling into the cluster is exactly the pattern it targets.

**Whether it applies here is not yet established.** Chrome's enumerated local
ranges are RFC 1918, `169.254.0.0/16`, `fc00::/7`, `fe80::/10`, and loopback.
Tailscale addresses are `100.64.0.0/10` — RFC 6598 shared address space —
which is not on that list, so Serve may be unaffected. That has not been
measured, and §21.6 records it as open rather than asserting either way. The
mirror makes the answer not matter: it retires the whole risk class, including
the case where Chrome later reclassifies CGNAT or an enterprise policy blocks the
prompt.

**CORS is required regardless**, for the Pages copy: an exact-origin
`Access-Control-Allow-Origin: https://afflom.github.io` rather than a wildcard,
and preflight `OPTIONS` handling.

**When the API is unreachable** — the browser is not on the tailnet, or the storage node is
rebooting — the UI renders an explicit disconnected state naming which causes it
cannot distinguish, rather than an empty list that looks like "you have no
devcontainers."

### 16.4 Why not drive it through the GitHub API

An alternative design has the static page dispatch `workflow_dispatch` events
that a self-hosted runner on the storage node picks up, requiring no inbound reachability at
all. It is rejected: every action becomes a workflow run, which means 10–30
seconds of latency to start a container, an Actions log full of UI clicks, and a
status view that cannot poll. The Tailscale path is direct, and Tailscale is
outbound-initiated, so it opens nothing.

### 16.5 Availability

The UI is unavailable during the storage node's update window (§14.2) and during any period
the storage node is unhealthy. It is a management surface, not a dependency: devcontainers
already running continue to run, their tunnel URLs are unaffected because the
tunnel is registered by the container and not by the control plane (§11.1), and
`ssh dc-<id>` continues to work from `generated/ssh_config` without the control
plane, resolving to the last known host. Only migration-aware resolution
degrades.

**SSH to the storage node is the lockout escape, and it is retained for that.** §16.2 makes
authorization depend on GitHub being reachable, on the App being configured, and
on a login being spelled correctly in `model/cluster.toml`. Any of those can be
wrong at a moment when the UI is the thing that would have fixed it. SSH is the
way back in, and it is the reason none of §16.2's machinery is allowed to become
the only door.

---

## 17. Node replacement and retirement

### 17.1 Replacement

**Replacing a machine is not a change to this repository.** Under the withdrawn
§3.1 it was: MAC addresses were model facts, so a new mainboard meant an edit, a
gate run and a promotion before the fleet could be whole again. Nothing about a
replacement mainboard is a fact about the cluster's design, and it no longer
touches the model.

The procedure is: install the promoted image (§12.1), power the machine on, and
release the old machine's registration:

```
cluster-ctl registration release <machine-id>
```

The replacement discovers its own hardware, finds the registrar across the mesh,
and takes the freed ordinal — the same one, so its names, addresses and update
position are the ones the fleet already expects.

Release is explicit and never automatic. A node that is merely off — powered
down for maintenance, or midway through a reboot — is indistinguishable from
one that is gone, and a registrar that reclaimed ordinals on silence would hand
a live node's identity to its replacement while the original was still booting.

The storage node is the exception in two ways: it self-detects rather than
registering, so there is no ordinal to free, and §5.6 governs what is and is not
recoverable on it. Replacing it is a restore, not a bootstrap.

### 17.1.1 Re-roling

Compute and testbed are assigned in provisioning order (§2.3.2), and provisioning
happened once. To swap them, release both registrations and boot them in the
order wanted. There is no command to set a role directly: a role that could be
set by hand would be a fact in two places again, and the whole of §2.3 exists to
stop that.

### 17.2 Retirement

A variant is retired by deleting its row from `model/images.toml` and its
Containerfile. The gates then fail on any ID, scenario, or rendered artifact that
referenced it — intended, since R1 makes a dangling reference a build failure
rather than a stale file. Retired IDs are never reused.

---

## 18. Observability

Prometheus, Grafana, and Alertmanager on the storage node as Quadlets, scraping over the
mesh. 30-day retention, accepted as lossy in §5.6.

| Alert | Condition |
| --- | --- |
| Node down | scrape failure > 5 min |
| Failed units | `node_systemd_unit_state{state="failed"} > 0` |
| Digest drift | booted digest ≠ current `:stable`, sustained > 24 h |
| Rollout stalled | rollout state unchanged > 6 h with a target pending |
| Drain budget exceeded | any budget in §14.4 breached |
| Digest quarantined | any node posted a quarantine (§13.4) |
| Split version | nodes on differing digests > 2 h |
| Root filesystem writable | `/usr` not read-only — an immutability violation |
| Cache pool pressure | dm-cache occupancy > 90% |
| Disk health | SMART failure, HDD prioritised |
| Clock | chrony unsynchronised or offset > 100 ms |
| Bench contention | any process on the testbed's isolated CPUs that is not the measurement job |
| Reclaim volume | more than 5 sessions archived in one run — a policy or clock bug looks like this |
| Dirty archives held | count of §15.3 held archives, informational |

Alertmanager delivers to a Tailscale-reachable webhook. There is no paging
integration; this is a three-node lab cluster and an alert that wakes someone is
worse than a dashboard they check.

The digest-drift, rollout-stalled, and split-version alerts are what make §13
observable. Unattended automation whose failures are invisible is worse than
manual updates.

---

## 19. The conformance register

### 19.1 What this document may say about it

Nothing that `model/ids.toml` says. This section defines the *namespace* and the
*classes*; the rows and their statements live in the register.

### 19.2 ID classes

| Prefix | Class | Suite |
| --- | --- | --- |
| `CM-` | Model — the register is internally consistent | *(template)* |
| `CD-` | Definition — the model renders the declared artifacts | `definition` |
| `CI-` | Image — properties of the built container, before boot | `image` |
| `CB-` | Boot — properties of a single booted node | `boot` |
| `CN-` | Network — mesh behaviour and failover | `network` |
| `CS-` | Storage — registry, object store, NFS, cache | `storage` |
| `CW-` | Workload — devcontainers and runners | `workload` |
| `CU-` | Update — rollout ordering, drain, greenboot rollback, quarantine, split-version interop | `update` |
| `CG-` | Reclamation — retention, archive, dirty protection | `reclaim` |
| `CC-` | Control plane — API, authorization, UI build | `control` |
| `CL-` | Lifecycle — signature policy, promotion, provenance | `lifecycle` |
| `CH-` | Hardware — what only real nodes can establish | `hardware` |

Two digits, `01`–`99` per class, never reused after retirement.

`registry.rs` notes that class rules are added *in the commit that adds the first
ID in that class*. Four are anticipated:

- The first `OPEN-` row: an `open` claim must carry `sample_size` and `seed`.
- The first `CD-` row: every file under `generated/` names at least one
  registered `CD-` ID in its header (§7.2).
- The first `CH-` row: a `CH-` scenario is excluded from T0–T2 collection, so a
  hardware claim can never be discharged by a simulated run.
- The first `CG-` row: a `CG-` scenario must include a dirty-workspace case.
  Retention that is only tested on clean workspaces is retention that has never
  been tested against the failure that matters.

### 19.3 Levels, applied here

**`build`** — constructed here and validated against its oracle. Mesh
reachability and failover; a cross-mesh registry pull; `devcontainer up` followed
by an exec; a runner registering and completing a job; a rollout in which exactly
one guest updates at a time; a drain that migrates a container and preserves its
worktree; a greenboot failure that rolls back and quarantines; a rollout that
halts when a peer is unhealthy; an idle session archived at threshold; a dirty
session **not** purged at threshold; an unsigned image failing to stage.

**`some-true`** — reproduced from an authority, not established here. bootc's
transactional update guarantee; ostree's deployment atomicity; greenboot's
boot-counting rollback; the Dev Containers specification; the Quadlet contract;
`dm-cache`'s writethrough durability; Tailscale's identity header semantics.
`AGENTS.md` is explicit that a claim about a dependency belongs to that
dependency: *"the image updates atomically"* and *"greenboot rolls back a failed
boot"* are **not** `build` claims here. They are authority rows, and what is
registered is what this repository constructs *around* them — that the upgrade is
triggered by the right event, that the rollback is detected, reported, and
quarantined, and that the boot which follows is asserted healthy by a predicate
this repository owns.

**`open`** — measured and reported, never asserted. Every quantity this
repository can observe but cannot construct an oracle for: the wall-clock length
of the storage node's unavailability window (§14.2), the duration of a devcontainer drain
(§14.3), the dm-cache hit ratio under a real CI load (§5.3), and the
run-to-run dispersion of a measurement on the testbed (§8.5). Each carries the
`sample_size` and `seed` its class rule requires, and no document is permitted to
say that any of them is proven, guaranteed, or established — the meta-gate reads
the prose and fails the build if one does.

The boundary between `build` and `open` is the existence of an oracle, not the
difficulty of the measurement. "Exactly one guest updates at a time" has an
oracle — count them — so it is `build`. "The drain takes about ninety seconds"
has none, because there is no independent statement of what it should take, so it
is `open` and stays a number with a sample size next to it.

---

## 20. Authorities

### 20.1 What is cited rather than constructed

Every row here is a fact this repository *depends on* and does not establish. A
`some-true` claim names one of them, `model/authorities.toml` records it well
enough for a third party to find, and `CM-03` fails the build if a claim cites an
authority that has no row.

| Authority | What it says | Where this repository leans on it |
| --- | --- | --- |
| bootc | A bootc host transitions between container images transactionally; the running system is never a partially applied image. | §10.4, §13.3 |
| ostree | A deployment is written and then made current atomically, and the previous deployment remains bootable. | §5.1, §13.3 |
| greenboot | Boot success is determined by required health checks, and a failed boot is rolled back to the previous deployment by boot counting. | §13.3 |
| Dev Containers specification | `devcontainer.json` fully determines how a workspace container is built and started. | §11, §14.3, §15.1 |
| Quadlet | A `.container` file under a systemd unit search path materializes as a service unit at boot. | §8.2, §8.4 |
| `dm-cache` | In writethrough mode every write reaches the origin device before completion, so cache-device loss is not origin data loss. | §5.3, §2.5 |
| Tailscale | `tailscale serve` terminates TLS with a tailnet CA certificate and presents the authenticated identity in a request header. | §16.2 |
| RFC 3021 | A /31 prefix on a point-to-point link addresses exactly two hosts, with no network or broadcast address. | §4.1 |
| Sigstore / cosign | A keyless signature binds an artifact digest to an OIDC identity, verifiable against a transparency log without a long-lived key. | §12.3 |
| VS Code Remote Tunnels | `code tunnel` registers a named tunnel and the Microsoft dev tunnels service relays a browser client to it; the URL is derived from the tunnel name. | §11.1, §14.3 |
| GitHub Apps device flow | A public client ID with no client secret authorizes a device; the resulting user-to-server token expires in eight hours and carries a refresh token. | §16.2 |
| Chrome Local Network Access | A request from a public origin to a private network address is gated behind a permission prompt, and denial is silent. | §16.3 |
| VS Code Server licensing | The server behind this model is licensed for a single user, and hosting it as a service is not permitted. | §21.7 |

**What citing these does not license.** `AGENTS.md` is explicit that a claim
about a dependency belongs to that dependency, and two temptations here are worth
naming. *"The tunnel connection is secure"* and *"device flow requires no client
secret"* are **not** `build` claims in this repository. They are the rows above.
What is registered is only what this repository constructs on top of them: that
the Feature installs a supervisor where it will actually be run, that the URL a
session is addressed by survives a migration, and that an unlisted login is
rejected.

### 20.2 Checksums, and where there are none

`model/authorities.toml` carries a checksum over the committed artifact when
there is one. For the rows above there generally is not: each cites living
upstream documentation rather than a file vendored into this repository, and a
checksum over a URL fetched at an unrecorded time is worse than no checksum
because it looks like provenance. `CM-03` therefore accepts `checksum = "none"`
only with a stated reason, and each row carries one.

A future authority that *is* vendored — a specification document committed under
this repository — carries a real checksum and loses the exemption.

### 20.3 What citing does not license

Citing an authority does not import its guarantee into this repository's register.
A cited fact is available to reason with; it is never evidence that this
repository realized it. What closes that gap is a `build` row: `realized_by` on
an authority names the conformance IDs that demonstrate the fact is actually in
force here, and an authority with an empty `realized_by` is a dependency this
repository has taken on without demonstrating.

---

## 21. What this repository does not claim

A specification is judged as much by what it refuses to assert as by what it
establishes. These are the things that are structurally out of reach, recorded
here so that no future reader mistakes their absence for an oversight.

### 21.1 That the testbed yields stable measurements

§8.5 configures CPU isolation, disables SMT, constrains C-states, pins the
governor, and steers interrupts. Each of those is a *constructible* fact: the
kernel argument is present, `/sys/devices/system/cpu/isolated` reflects it, no
foreign process is scheduled on the isolated set. Those are `build` claims and
`CB-` carries them.

Stability is not among them. It is a property of the workload, the compiler, the
microarchitecture's undocumented behaviour, and the thermal environment of the
rack — none of which this repository controls or can construct an oracle for.
What §18's bench-contention alert establishes is that nothing *known* is
competing, which is a necessary condition and not a sufficient one. The dispersion
itself is an `open` row: measured, reported with its sample size, never asserted.

### 21.2 That the hardware is correct

`CH-` claims are discharged only on real nodes, and T3 is the only tier that can
run them. A simulated run cannot establish that VT-x is enabled in firmware, that
a MAC belongs to the card the model says it does, or that a SATA device is
physically mounted where §2.2 requires. The class rule enforces the exclusion
rather than trusting the tier to remember it, because a `CH-` claim discharged by
a QEMU guest would be a false statement about a physical machine.

### 21.3 That any dependency behaves as documented

§20's authorities are cited, not verified. This repository does not test that
ostree deployments are atomic, that greenboot counts boots correctly, or that
`dm-cache` in writethrough mode survives a power cut — those belong to the
projects that make the claims, and re-registering them here would give each claim
two sources, which R1 forbids. What is tested is that this repository *uses* them
in the way the citation permits, and that failure of any of them is observable
rather than silent.

### 21.4 That a single copy of `lv_data` is sufficient

§5.6 states plainly that there is no replica and no off-site target. That is a
recorded limitation, not a design this repository defends. The mitigation is
restricting what is allowed to live there, and the mitigation's correctness rests
entirely on that restriction continuing to hold — which no gate can check,
because "someone put something irreplaceable on the data volume" is a fact about
intent and not about bytes.

### 21.5 That the tunnel path has been exercised end to end

Every component of §11.1 is standard and documented. The *combination* — a
tunnel inside a devcontainer, a shared authentication volume, supervised against
process death, surviving a migration — has not been run, and nothing in this
repository should be read as saying it has.

Four failure modes are anticipated and handled in the Feature rather than in
`cluster-ctl`, and each is an anticipation rather than an observation:

- **UID alignment.** The CLI writes to `~/.vscode-cli/`. Devcontainer images do
  not agree on the container user's UID — most are 1000, some are not — and a
  mismatch fails authentication silently, on that one container.
- **Supervision.** Devcontainers have no init by default, so a `code tunnel`
  started from `postStartCommand` has nothing to restart it if it dies
  mid-session.
- **Cold start.** The CLI is baked into the Feature layer; the server payload is
  not, because `vscode.dev` pins it by commit and ships weekly, so a baked server
  goes stale and is re-downloaded anyway. Roughly thirty seconds on first connect
  after each VS Code release is accepted.
- **Name collision.** Tunnel names are globally unique per account, which is why
  §15.3 unregisters on archive.

Until the spike in §22 has run, no `CW-` row asserts any of the behaviours above.
The register is the place that says what has been established, and it currently
says nothing about them.

### 21.6 That Local Network Access does not gate the tailnet

§16.3 states the constraint and states that its applicability here is unmeasured.
Chrome's enumerated local ranges do not include `100.64.0.0/10`, which suggests
Serve is unaffected — but "suggests" is the whole of the evidence, and a
permission denial under this feature is *silent*, which is the worst shape a
wrong guess could take.

The the storage node mirror in §16.3 exists so the answer does not gate anything. If the
measurement is ever taken and comes back the other way, the disconnected state in
§16.3 gains a third cause it cannot distinguish from the other two, and that is
the only thing that changes.

### 21.7 That more than one person can use this

Two limits point the same way and neither is a matter of effort.

The VS Code Server behind §11.1 is licensed for a single user, and hosting it as
a service is not permitted. One operator on owned hardware is within that.
§15.1's `owner` field is multi-user-shaped, and if it ever means more than one
person, this constraint binds and the design has to change.

§16.2's authorization is a login compared against a list, because `afflom` is a
user account with no membership API. A second person needs an organization or a
collaborator check against a named repository.

### 21.8 That the browser path is independent of a third party

Traffic routes browser → Microsoft dev tunnels relay → the container. Two things
follow that are not claimed away.

**Editor latency is not LAN latency**, and it is noticeable in the integrated
terminal. **Availability depends on a relay this repository does not run**: if
the dev tunnels service is down, the browser path is down. §11.1's SSH alias is
the mitigation, and that is the reason it is retained rather than a courtesy to
old habits.

The **concurrent tunnel quota per account is unmeasured**. If it turns out lower
than the migration memory budget in §14.3, it is the binding limit on how many
devcontainers this cluster can host, and it belongs in §1.1's ceilings rather
than here. It is here because nobody has counted.

### 21.10 That §9.1's precondition holds

It does not. §9.1 states that the repository is private and derives from that
the claim that fork pull requests do not exist --- which is *"the precondition
that makes T2 on the storage node acceptable"*. The repository is public, so that
precondition is false, and the section itself says what follows: *"Going public
would require moving T2 to hosted runners."*

What exists today is a guard rather than a fix: every self-hosted job refuses to
run for a pull request whose head repository is not this one, so a fork cannot
schedule its code on the node that holds the registry, the object store, and
every devcontainer. `CL-07` asserts the guard.

A guard is not the derivation. Two things would restore it, and both are the
operator's to choose:

- make the repository private again, which is what §9.1 assumes throughout; or
- move T2 to hosted runners and accept the emulation tax §9.4 rejected, which
  means rewriting §9.4's runner table and §10.2's tier durations.

Recorded here rather than resolved because it is a decision about what this
cluster is for, not a defect in what it does.

### 21.9 That unattended update is risk-free

§13 is engineered so that the common failure is cheap: the testbed moves first, greenboot
rolls back, the digest is quarantined, an alert fires. What is not claimed is that
every failure is caught. A change that passes `cluster-health` and is still wrong
— slow, subtly misconfigured, correct on boot and broken an hour later — proceeds
through all three nodes exactly as a good one does. The health predicate is the
oracle, and the rollout is no better than it.

### 21.11 That a misassembled fleet can be provisioned anyway

§2.3.1's storage predicate is true on exactly one machine of a conforming fleet.
On a fleet with two bulk devices, or none, it is true twice or never, and there
is no answer this system can compute: which machine *should* hold the data is a
decision about the hardware, taken by whoever assembled it.

So the registrar refuses. It does not pick the larger disk, the lower machine
ID, or the first to boot. A cluster that silently chose would put the object
store on a 256 GB SSD and report itself healthy, and the operator would find out
when it filled.

What is claimed is that the refusal is loud and names the count it measured.
What is not claimed is that the fleet can be brought up without correcting the
hardware.

### 21.12 That a mis-cabled mesh is detected as such

Matching interfaces by MAC (the withdrawn §3.1) made one class of error visible
that classification by speed does not: a cable moved from one port to another
changed which declared MAC carried which role, and the boot failed.

Discovery has no opinion about which port a cable is in. A mesh cable moved
between the two 10GbE ports of the same machine is not an error and is not
detected — both ends re-discover their peer and re-derive the same addresses,
which is the point. But it also means the *only* mis-cabling this system
detects is one that changes the topology: a link to the wrong machine, or a link
missing. A physically odd but topologically identical arrangement passes, and no
gate here will say it looked strange.

What replaced the MAC check is §3.1's port count and §3.3's peer identity: two
mesh-class ports must be present, and each must find a peer that the registrar
agrees exists. That catches an absent link and a link to a stranger. It does not
catch tidiness.

### 21.13 That mesh discovery is authenticated

§3.3's announcement is an unauthenticated datagram on a link-local multicast
address. Anything electrically present on that segment can answer it and claim
to be a peer.

This is the same trust §4.4 already extends to the mesh as a whole, and it rests
on the same physical property: a direct-attached cable between two chassis in
one rack has exactly two endpoints and no third party without physical access.
Discovery does not weaken the boundary; it depends on it.

What follows the discovery *is* authenticated: the registration request carries
the join secret (§12.2), so learning a peer's identity is not sufficient to
obtain an ordinal, a role, or an address. An attacker with physical access to
the mesh cabling has, however, already got physical access to the machines, and
this repository claims nothing against that.

### 21.14 That one image costs nothing

§8.4 collapsed three images into one. The image now carries QEMU, `lvm2`,
`nfs-utils` and `restic` onto machines that will never run them, and the
measurement node — the one whose whole purpose is an absence of activity —
carries the largest set of packages it has ever carried.

Nothing here claims that is free. It costs bytes on an M.2 and it widens the
package surface every node presents to a CVE scan. What is claimed is that the
packages are *present*, not running: role-gated units are skipped by their
`ConditionPathExists=`, and `CB-` asserts on the testbed that no storage-role
unit is active.

The trade is one artifact to build, sign, promote, scan and roll back instead of
three, and an installation that requires no operator to know which machine they
are standing in front of. Whether that is worth the bytes is a judgement, and it
is recorded here as one.

---

## 22. Build order

> **Reconstructed.** The specification handed to the implementer was truncated
> partway through §19.3; §20, §21 and this section were rebuilt from the
> cross-references pointing at them. The tranche numbering below is chosen so
> that step 7 is `CW-` and step 8 is `CC-`, which is what the amendment note
> assumes. Correct it against the original.

The order is a dependency order, not a preference. Each tranche can be built
because the ones before it exist, and a tranche's claims are registered only when
its capability is built (R3).

| Step | Tranche | Depends on |
| --- | --- | --- |
| 1 | `CM-` — the register is internally consistent | the template |
| 2 | `CD-` — the model renders the declared artifacts | 1 |
| 3 | `CI-` — properties of the built container | 2 |
| 4 | `CL-` — signature policy, promotion, provenance | 3 |
| 5 | `CB-` — properties of a booted node | 3, and the guest fixture |
| 6 | `CN-`, `CS-` — mesh and storage on booted nodes | 5 |
| 7 | `CW-` — devcontainers and runners | 6, and the tunnel spike below |
| 8 | `CC-` — the control plane and its authorization | 7 |
| 9 | `CU-` — rollout ordering, drain, rollback | 8 |
| 10 | `CG-` — retention, archive, dirty protection | 8 |
| 11 | `CH-` — what only real nodes establish | the fleet |
| 12 | `CC-` UI — the web interface | 8 |

Three things about this order are load-bearing rather than incidental.

**Step 7 is gated on a spike, not on code.** The tunnel Feature (§11.1) must be
published before any `CW-` scenario can exercise it, and the combination in
§21.5 must be *run* before any `CW-` row asserts it. Publishing the Feature is
part of this tranche; registering the behavioural claims is not, until the spike
has happened. The spike is:

```
devcontainer up --workspace-folder <repo> \
  --additional-features '{"ghcr.io/afflom/cluster/features/tunnel:1":{}}'
```

then: open the URL; stop and start the container and confirm the same URL lands;
kill the tunnel process and confirm the supervisor restores it; run a container
whose user is not UID 1000 and confirm authentication still works. Separately,
run several sessions concurrently to establish the per-account concurrent tunnel
quota (§21.8).

**Step 8 precedes step 12, and the exposure decision precedes step 8.** Serve or
Funnel (§16.2) changes what the UI can reach and therefore what its disconnected
state must say. Deciding it during the UI tranche would mean building the UI
twice.

**Step 12 ships the the storage node mirror with the Pages deployment, not after it.** §16.3
makes the mirror the path that always works; a UI published to Pages alone,
pending a mirror to follow, is a UI whose one guaranteed route does not exist
yet.

Nothing is registered until it is built. R4 is unaffected by an amendment to this
document: an amendment changes what will be built, and a stub changes what is in
the tree. Only the second is a deferral.
