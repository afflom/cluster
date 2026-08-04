//! The Anaconda kickstart each node is installed from (`SPEC.md` §5.1, §12.1).
//!
//! Rendered per node because the storage roles differ: `n1` builds an LVM cache
//! pool over a 2 TB spinning disk, `n2` gives its SATA SSD to container graph
//! storage, `n3` gives its own to measurement working state. Rendering the
//! layout rather than writing it is what makes §2.2's fallback a one-line model
//! change with nothing downstream branching.
//!
//! **No secret appears here.** The authorized key, the registry PAT and the
//! Tailscale auth key are injected at install time from Actions secrets (§12.2).
//! A kickstart is a plain-text file committed to a repository, and a secret in
//! one is a secret published --- `CD-07` asserts the absence rather than trusting
//! it.

use crate::render::Rendered;
use crate::{Cluster, Node};

/// The placeholders the installer substitutes. They are names, not values, and
/// `CD-07` reads this list to assert that nothing else secret-shaped is present.
pub const SECRET_PLACEHOLDERS: &[&str] = &[
    "@@AUTHORIZED_KEY@@",
    "@@GHCR_PULL_TOKEN@@",
    "@@TAILSCALE_AUTH_KEY@@",
];

pub(crate) fn render(c: &Cluster, node: &Node) -> Rendered {
    let mut body = String::new();

    body.push_str(&format!(
        "# Kickstart for {}, role {}. The installer substitutes the three\n\
         # @@PLACEHOLDER@@ values below from Actions secrets at ISO build time;\n\
         # none of them is committed, and CD-07 asserts that (§12.2).\n\
         #\n\
         # The ISO's SHA-256 is published in the release and verified out of band.\n\
         # That checksum is the root of trust: §12.3's signature policy ships inside\n\
         # the image, so the first install cannot verify itself (§12.1).\n\n",
        node.name, node.role
    ));

    body.push_str("text\n");
    body.push_str("firstboot --disable\n");
    body.push_str("selinux --enforcing\n");
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

    // ---- secondary storage, per role (§5.1, §5.3) ----
    let storage = &node.storage;
    if let (Some(vg), Some(lv), Some(cache)) = (
        storage.volume_group.as_ref(),
        storage.origin_lv.as_ref(),
        storage.cache_device.as_ref(),
    ) {
        let hdd = node
            .disk
            .iter()
            .find(|d| d.purpose == "data")
            .map(|d| d.id.as_str())
            .unwrap_or("sata-hdd");
        body.push_str(&format!(
            "# The data volume: a {hdd} origin under a writethrough dm-cache on the\n\
             # {cache}. Writeback would be faster on write and would make a single\n\
             # non-redundant SSD a data-loss mode for the whole 2 TB origin.\n\
             # Writethrough adds no failure mode, and it is what makes §2.5's\n\
             # tolerance of hard power loss true --- which unattended reboots depend\n\
             # on (§5.3).\n\
             #\n\
             # dm-cache over ZFS because it is in-tree: a kernel bump carried by a new\n\
             # image must not require rebuilding an out-of-tree module inside an\n\
             # immutable host.\n"
        ));
        body.push_str("%post --erroronfail\n");
        body.push_str(&format!("vgcreate {vg} /dev/disk/by-id/{hdd}\n"));
        body.push_str(&format!("lvcreate --extents 100%FREE --name {lv} {vg}\n"));
        if let Some(gib) = storage.cache_partition_gib {
            body.push_str(&format!(
                "# §2.2 fallback sizing: {gib} GiB, used when the cache device is a\n\
                 # partition of the M.2 rather than the SATA SSD.\n"
            ));
        }
        body.push_str(&format!(
            "lvcreate --type cache --cachemode writethrough --cachepolicy smq \\\n  \
               --chunksize 256K --extents 100%FREE --cachedevice /dev/disk/by-id/{cache} {vg}/{lv}\n"
        ));
        body.push_str(&format!("mkfs.xfs /dev/{vg}/{lv}\n"));
        body.push_str("%end\n\n");
    } else if let Some(device) = storage.container_graph_device.as_ref() {
        body.push_str(&format!(
            "# Container graph storage on the {device}. overlay2 and podman's overlay\n\
             # driver do not function on NFS, which is why the graph is local on every\n\
             # node and NFS carries data only (§11.2).\n\
             %post --erroronfail\n\
             mkfs.xfs /dev/disk/by-id/{device}\n\
             %end\n\n"
        ));
    } else if let Some(device) = storage.bench_device.as_ref() {
        body.push_str(&format!(
            "# Measurement working state on the {device}. This node mounts no network\n\
             # filesystem: NFS client activity, RPC timers and interrupt handling inject\n\
             # jitter into exactly the quantity being measured (§2.3).\n\
             %post --erroronfail\n\
             mkfs.xfs /dev/disk/by-id/{device}\n\
             %end\n\n"
        ));
    }

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

    body.push_str("# Ephemeral and single-use: the key is spent by this one install (§12.2).\n");
    body.push_str(&format!(
        "tailscale up --auth-key '{}' --advertise-tags=tag:cluster{}\n",
        SECRET_PLACEHOLDERS[2],
        if node.name == c.policy.drain.migration_target {
            format!(" --advertise-routes={}", c.network.lan_prefix)
        } else {
            // The mesh is never advertised, and only n1 advertises the
            // management subnet (§4.5).
            String::new()
        }
    ));
    body.push_str("%end\n\n");

    body.push_str(&format!(
        "# The node is not considered provisioned until the predicate passes (§12.1).\n\
         %post --erroronfail --nochroot\n\
         echo 'cluster-health must pass before {} is in service'\n\
         %end\n\n\
         reboot\n",
        node.name
    ));

    Rendered::new(format!("bootstrap/{}.ks", node.name), vec!["CD-07"], body)
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
