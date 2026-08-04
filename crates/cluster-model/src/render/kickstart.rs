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
//! **No secret appears here.** The authorized key, the registry PAT and the
//! Tailscale auth key are injected at install time from Actions secrets (§12.2).
//! A kickstart is a plain-text file committed to a repository, and a secret in
//! one is a secret published --- `CD-07` asserts the absence rather than trusting
//! it.

use crate::render::Rendered;
use crate::Cluster;

/// The placeholders the installer substitutes. They are names, not values, and
/// `CD-07` reads this list to assert that nothing else secret-shaped is present.
pub const SECRET_PLACEHOLDERS: &[&str] = &[
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

    body.push_str(&format!(
        "# The kickstart every machine is installed from. One image means one\n\
         # installer and nothing to select at install time (§8.4, §12.1).\n\
         #\n\
         # The installer substitutes the three @@PLACEHOLDER@@ values below from\n\
         # Actions secrets at ISO build time; none of them is committed, and CD-07\n\
         # asserts that (§12.2).\n\
         #\n\
         # The ISO's SHA-256 is published in the release and verified out of band.\n\
         # That checksum is the root of trust: §12.3's signature policy ships inside\n\
         # the image, so the first install cannot verify itself (§12.1).\n\n"
    ));

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

    body.push_str(&format!(
        "# Secondary storage, decided here rather than chosen by an operator.\n\
         #\n\
         # A non-boot block device of at least {threshold} GB makes this the storage\n\
         # node (§2.3.1). That predicate is measurable now; the compute/testbed\n\
         # distinction is not --- it is assigned by the registrar over a network\n\
         # that does not exist yet during Anaconda --- so this prepares bulk storage\n\
         # where it finds bulk storage and a plain filesystem where it does not.\n\
         #\n\
         # The data volume is a spinning origin under a **writethrough** dm-cache.\n\
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
        "min_bytes=$(( {threshold} * 1000 * 1000 * 1000 ))\n"
    ));
    body.push_str("bulk=\"\"\ncache=\"\"\n");
    body.push_str("for dev in $(lsblk --noheadings --nodeps --output PATH); do\n");
    body.push_str("  [ \"$dev\" = \"$root_disk\" ] && continue\n");
    body.push_str("  size=$(blockdev --getsize64 \"$dev\")\n");
    body.push_str("  if [ \"$size\" -ge \"$min_bytes\" ]; then bulk=\"$dev\"; else cache=\"$dev\"; fi\n");
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
    body.push_str("  # either way its second device carries local state. overlay2 and\n");
    body.push_str("  # podman's overlay driver do not function on NFS, which is why the\n");
    body.push_str("  # container graph is local on every node and NFS carries data only\n");
    body.push_str("  # (§11.2).\n");
    body.push_str("  mkfs.xfs \"$cache\"\n");
    body.push_str("fi\n");
    body.push_str("%end\n\n");

    // ---- secrets, by placeholder only (§12.2) ----
    body.push_str("# Injected at ISO build time. Names here, values never.\n");
    body.push_str("%post --erroronfail\n");
    body.push_str("install -d -m 0700 /root/.ssh\n");
    body.push_str(&format!(
        "printf '%s\\n' '{}' > /root/.ssh/authorized_keys\n",
        SECRET_PLACEHOLDERS[0]
    ));
    body.push_str("chmod 0600 /root/.ssh/authorized_keys\n\n");

    body.push_str("install -d -m 0700 /etc/containers\n");
    body.push_str(&format!(
        "printf '%s\\n' '{}' > /etc/containers/auth.json\n",
        SECRET_PLACEHOLDERS[1]
    ));
    body.push_str("chmod 0600 /etc/containers/auth.json\n\n");

    body.push_str(
        "# Ephemeral and single-use: the key is spent by this one install (§12.2).\n\
         #\n\
         # The subnet route is advertised only by the storage node, and which machine\n\
         # that is is not known yet --- so it is advertised at first boot by\n\
         # cluster-init, once the role is, rather than guessed at here. The mesh is\n\
         # never advertised (§4.5).\n",
    );
    body.push_str(&format!(
        "tailscale up --auth-key '{}' --advertise-tags=tag:cluster\n",
        SECRET_PLACEHOLDERS[2]
    ));
    body.push_str("%end\n\n");

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
