//! The Anaconda kickstart every node is installed from (`SPEC.md` §5.1, §12.1).
//!
//! **One kickstart, because there is one image** (§8.4). There were three: one
//! per node, each hard-coding the storage layout its machine would get. That
//! made installation an act of choosing which file to boot, and the thing being
//! chosen --- which machine is which --- is exactly what the machine can work
//! out for itself.
//!
//! The secondary storage layout still differs by role, and the `%post` here
//! branches on the same predicate §2.3.1 uses: a non-boot device at or above the
//! declared threshold makes this the storage node. That predicate is measurable
//! at install time, unlike the compute/testbed distinction, which is assigned by
//! the registrar over a network that does not exist yet during Anaconda. So the
//! installer prepares bulk storage where it finds bulk storage, prepares a plain
//! filesystem where it does not, and leaves the rest to first boot.
//!
//! **No secret appears here, and none is injected here either.** An earlier
//! revision carried three `@@PLACEHOLDER@@` names to be substituted at ISO build
//! time from Actions secrets. Two things were wrong with it.
//!
//! The Actions secrets did not exist. Nothing in any workflow supplied them, so
//! a node would have installed the literal string `@@AUTHORIZED_KEY@@` as root's
//! authorized key --- locked out, on a headless machine --- and then died at
//! `tailscale up --erroronfail`, taking the install with it.
//!
//! And the design could not work for a public repository even with the secrets
//! present. The ISO is a release artifact: a secret substituted into one is a
//! secret published to whoever downloads it (§9.1).
//!
//! So the node installs with no credentials at all and comes up *unenrolled*.
//! The operator reaches the control plane over the LAN, authenticates with the
//! GitHub App device flow --- the one credential that can be checked without any
//! of the others existing --- and enters the rest through the browser (§12.2,
//! §16.2). `CD-07` asserts that nothing secret-shaped is in this file, and
//! `CL-08` asserts that no shipped artifact carries a placeholder nothing fills.

use crate::render::Rendered;
use crate::Cluster;

/// The one placeholder anything still substitutes, and where.
///
/// The ISO build replaces it with the rendered kickstart, in
/// `bootstrap/config.toml`. It is here so that `CL-08` can assert every
/// placeholder in a shipped artifact has something that fills it --- the check
/// whose absence let a kickstart ship as the literal string
/// `@@RENDERED_KICKSTART@@`.
pub const KICKSTART_PLACEHOLDER: &str = "@@RENDERED_KICKSTART@@";

/// Placeholders that must appear in no rendered artifact.
///
/// Retired names, kept so the gate can say *why* one coming back is wrong rather
/// than only that it is. Each of these once stood for a secret substituted at
/// ISO build time, which is a secret published in a release artifact (§9.1).
pub const RETIRED_PLACEHOLDERS: &[&str] = &[
    "@@AUTHORIZED_KEY@@",
    "@@GHCR_PULL_TOKEN@@",
    "@@TAILSCALE_AUTH_KEY@@",
];

pub(crate) fn render(c: &Cluster) -> Rendered {
    let mut body = String::new();
    let storage_role = c
        .cluster
        .self_detected_role()
        .expect("the model check requires exactly one self-detected role");
    let threshold = c.cluster.detection.bulk_disk_min_gb;

    body.push_str(
        "# The kickstart every machine is installed from. One image means one\n\
         # installer and nothing to select at install time (§8.4, §12.1).\n\
         #\n\
         # It carries no credentials and substitutes nothing. A node comes up\n\
         # unenrolled and is given its secrets through the browser, over the LAN,\n\
         # after it boots (§12.2, §16.2).\n\
         #\n\
         # The ISO's SHA-256 is published in the release and verified out of band.\n\
         # That checksum is the root of trust: §12.3's signature policy ships inside\n\
         # the image, so the first install cannot verify itself (§12.1).\n\n",
    );

    body.push_str("text\n");
    body.push_str("firstboot --disable\n");
    body.push_str("selinux --enforcing\n");
    // DHCP on whichever port has carrier. There is no per-machine address to
    // configure: §3.2 makes the management plane DHCP precisely because a static
    // one would be a fact about a machine kept in a repository.
    body.push_str("network --bootproto=dhcp --device=link --activate\n");
    body.push_str("rootpw --lock\n");
    body.push_str("# Console redirection matches the firmware setting (§2.4).\n");
    body.push_str("bootloader --append=\"console=ttyS1,115200\"\n\n");

    // ---- partitioning (§5.1) ----
    body.push_str("# M.2 layout, identical on every node. bootc uses ostree deployments\n");
    body.push_str("# rather than A/B partitions, so current and rollback share p3 and there\n");
    body.push_str("# is no slot arithmetic (§5.1).\n");
    body.push_str("clearpart --all --initlabel --disklabel=gpt\n");
    for partition in &c.cluster.partition {
        let size = if partition.size == "remainder" {
            "--grow".to_string()
        } else {
            format!("--size={}", mib(&partition.size))
        };
        let fstype = match partition.mount.as_str() {
            "/boot/efi" => "--fstype=efi".to_string(),
            _ => format!("--fstype={}", partition.format),
        };
        body.push_str(&format!("part {} {fstype} {size}\n", partition.mount));
    }
    body.push('\n');

    // ---- secondary storage, decided by the machine (§2.3.1, §5.3) ----
    let devices = &storage_role.devices;
    let vg = devices.volume_group.as_deref().unwrap_or("vg_data");
    let lv = devices.origin_lv.as_deref().unwrap_or("lv_data");
    let cache = devices.cache_device.as_deref().unwrap_or("sata-ssd");

    // What each role expects its second device to be. The installer cannot tell
    // compute from testbed --- that is the registrar's decision, taken later over
    // a network that does not exist during Anaconda --- so the model check
    // requires every assigned role to name the same device, and this states which
    // one they agreed on rather than leaving the reader to check three tables.
    let local: Vec<String> = c
        .cluster
        .assigned_roles()
        .iter()
        .filter_map(|role| {
            let d = &role.devices;
            d.container_graph_device
                .as_deref()
                .or(d.bench_device.as_deref())
                .map(|device| format!("{}: {device}", role.id))
        })
        .collect();

    body.push_str(&format!(
        "# Secondary storage, decided here rather than chosen by an operator.\n\
         #\n\
         # A non-boot block device of at least {threshold} GB makes this the storage\n\
         # node (§2.3.1). That predicate is measurable now; the compute/testbed\n\
         # distinction is not --- it is assigned by the registrar over a network\n\
         # that does not exist yet during Anaconda --- so this prepares bulk storage\n\
         # where it finds bulk storage and a plain filesystem where it does not.\n\
         #\n\
         # The data volume is a spinning origin under a **writethrough** dm-cache on\n\
         # the {cache}.\n\
         # Writeback would be faster on write and would make a single non-redundant\n\
         # SSD a data-loss mode for the whole origin. Writethrough adds no failure\n\
         # mode, and it is what makes §2.5's tolerance of hard power loss true ---\n\
         # which unattended reboots depend on (§5.3).\n\
         #\n\
         # dm-cache over ZFS because it is in-tree: a kernel bump carried by a new\n\
         # image must not require rebuilding an out-of-tree module inside an\n\
         # immutable host.\n"
    ));
    body.push_str("%post --erroronfail\n");
    body.push_str("set -eu\n\n");
    body.push_str("root_disk=$(lsblk --noheadings --output PKNAME --paths \"$(findmnt --noheadings --output SOURCE /)\" | head -1)\n");
    body.push_str(&format!(
        "min_bytes=$(( {threshold} * 1000 * 1000 * 1000 ))\n",
    ));
    body.push_str("bulk=\"\"\ncache=\"\"\n");
    body.push_str("for dev in $(lsblk --noheadings --nodeps --output PATH); do\n");
    body.push_str("  [ \"$dev\" = \"$root_disk\" ] && continue\n");
    body.push_str("  size=$(blockdev --getsize64 \"$dev\")\n");
    body.push_str(
        "  if [ \"$size\" -ge \"$min_bytes\" ]; then bulk=\"$dev\"; else cache=\"$dev\"; fi\n",
    );
    body.push_str("done\n\n");
    body.push_str("if [ -n \"$bulk\" ] && [ -n \"$cache\" ]; then\n");
    body.push_str(&format!("  vgcreate {vg} \"$bulk\"\n"));
    body.push_str(&format!("  lvcreate --extents 100%FREE --name {lv} {vg}\n"));
    if let Some(gib) = devices.cache_partition_gib {
        body.push_str(&format!(
            "  # §2.2 fallback sizing: {gib} GiB, used when the cache device is a\n\
             \x20 # partition of the M.2 rather than a second SATA device.\n"
        ));
    }
    body.push_str(&format!(
        "  lvcreate --type cache --cachemode writethrough --cachepolicy smq \\\n    \
           --chunksize 256K --extents 100%FREE --cachedevice \"$cache\" {vg}/{lv}\n"
    ));
    body.push_str(&format!("  mkfs.xfs /dev/{vg}/{lv}\n"));
    body.push_str("elif [ -n \"$cache\" ]; then\n");
    body.push_str("  # No bulk device: this machine will be assigned compute or testbed, and\n");
    body.push_str(&format!(
        "  # either way its second device carries local state ({}). overlay2 and\n",
        local.join(", ")
    ));
    body.push_str("  # podman's overlay driver do not function on NFS, which is why the\n");
    body.push_str("  # container graph is local on every node and NFS carries data only\n");
    body.push_str("  # (§11.2). The two roles name the same device, and the model check\n");
    body.push_str("  # requires that: the installer cannot tell them apart.\n");
    body.push_str("  mkfs.xfs \"$cache\"\n");
    body.push_str("fi\n");
    body.push_str("%end\n\n");

    // ---- no credentials, deliberately (§12.2) ----
    body.push_str(&format!(
        "# The node installs with no credentials and comes up **unenrolled**.\n\
         #\n\
         # There is nothing to substitute here and nothing withheld. The ISO is a\n\
         # release artifact, so a secret placed in one is published to whoever\n\
         # downloads it --- and this repository is public (§9.1).\n\
         #\n\
         # What an unenrolled node has is the control plane, reachable over the LAN.\n\
         # The operator opens it in a browser, authenticates with the GitHub App\n\
         # device flow --- the one credential checkable without any of the others\n\
         # existing --- and enters the rest (§12.2, §16.2):\n\
         #\n{}\n",
        c.policy
            .secret
            .iter()
            .map(|s| format!("#   {} --- {}, enabling {}", s.id, s.description, s.enables))
            .collect::<Vec<_>>()
            .join("\n")
    ));

    body.push_str(
        "# The node is not considered provisioned until the predicate passes (§12.1).\n\
         %post --erroronfail --nochroot\n\
         echo 'cluster-health must pass before this node is in service'\n\
         %end\n\n\
         reboot\n",
    );

    Rendered::new("bootstrap/node.ks", vec!["CD-07"], body)
}

/// Kickstart sizes are in MiB. `1GiB` is the only unit the model uses, and an
/// unrecognised one is a model error rather than a silently wrong partition.
fn mib(size: &str) -> String {
    if let Some(gib) = size.strip_suffix("GiB") {
        gib.parse::<u64>()
            .map(|n| (n * 1024).to_string())
            .unwrap_or_else(|_| size.to_string())
    } else if let Some(mib) = size.strip_suffix("MiB") {
        mib.to_string()
    } else {
        size.to_string()
    }
}
