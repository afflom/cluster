//! The typed shape of `model/images.toml` (`SPEC.md` §8).

use serde::Deserialize;

/// `model/images.toml`.
#[derive(Debug, Clone, Deserialize)]
pub struct ImagesFile {
    /// The schema tag.
    pub spec: String,
    /// The upstream base, pinned by digest (§8.1).
    pub base: Base,
    /// What a node's signature policy will accept (§12.3).
    pub signing: Signing,
    /// Where a node pulls from, in order (§13.1, §14.2).
    pub registries: Registries,
    /// The legal container runtimes (§8.2).
    pub runtime: Vec<Runtime>,
    /// The runtime a variant takes when it does not say otherwise.
    pub default_runtime: String,
    /// One per node image.
    pub variant: Vec<Variant>,
}

/// The upstream base image and everything every node has (§8.1).
#[derive(Debug, Clone, Deserialize)]
pub struct Base {
    /// Repository, without a tag or digest.
    pub image: String,
    /// Recorded for provenance and for the weekly bump PR to diff against.
    /// Nothing pulls it.
    pub tag: String,
    /// The multi-arch index digest. This is what a Containerfile writes after
    /// `FROM`, because a repository this careful about digest-pinning
    /// downstream cannot float its upstream.
    pub digest: String,
    /// The amd64 manifest inside that index.
    pub amd64_digest: String,
    /// The fleet's uniform architecture (§2.1).
    pub architecture: String,
    /// When the digest above was resolved from the upstream registry.
    pub resolved_on: String,
    /// Why this base rather than another.
    pub reason: String,
    /// Packages, binaries and kernel arguments every node gets.
    pub content: BaseContent,
    /// SSH daemon policy.
    pub sshd: Sshd,
    /// SELinux policy (§8.3).
    pub selinux: Selinux,
    /// Quadlets every node runs.
    #[serde(default)]
    pub quadlet: Vec<Quadlet>,
}

/// What the base image adds on top of the upstream (§8.1).
#[derive(Debug, Clone, Deserialize)]
pub struct BaseContent {
    /// Packages installed from a repository.
    pub packages: Vec<String>,
    /// Binaries this repository builds and copies in. They are the two things
    /// every node must have before it can be trusted to update itself
    /// unattended (§10.1, §13).
    pub binaries: Vec<String>,
    /// Kernel arguments every node carries.
    pub kargs: Vec<String>,
}

/// SSH daemon policy. A password-accepting `sshd` on a node that reboots
/// unattended is an invitation, and there is no operator present to notice.
#[derive(Debug, Clone, Deserialize)]
pub struct Sshd {
    /// Whether passwords are accepted. They are not.
    pub password_authentication: bool,
    /// `PermitRootLogin`.
    pub permit_root_login: String,
    /// Whether keyboard-interactive is accepted. It is not.
    pub kbd_interactive_authentication: bool,
}

/// SELinux configuration (§8.3).
#[derive(Debug, Clone, Deserialize)]
pub struct Selinux {
    /// `enforcing`, and it stays that way.
    pub mode: String,
    /// `targeted`.
    pub policy_type: String,
    /// The custom policy module's name.
    pub module: String,
    /// `build`. Nothing compiles policy at runtime on a read-only root.
    pub compile_at: String,
}

/// What a node's signature policy will accept (§12.3).
///
/// The identity is a *workflow* reference and not merely a repository, because
/// §12.3 is explicit that an image signed by a different workflow in the same
/// repository must not stage either. A policy keyed on the repository alone
/// would admit anything any workflow in it ever signed.
#[derive(Debug, Clone, Deserialize)]
pub struct Signing {
    /// The OIDC issuer. GitHub's, for keyless cosign.
    pub issuer: String,
    /// The repository whose workflow may sign.
    pub repository: String,
    /// The workflow file that may sign, relative to the repository root.
    pub workflow: String,
    /// The ref it must run on. `ref` is a Rust keyword, so the field carries
    /// the trailing underscore and serde carries the model's spelling.
    #[serde(rename = "ref")]
    pub ref_: String,
    /// The transparency log a signature is recorded in.
    pub transparency_log: String,
}

impl Signing {
    /// The exact certificate identity a signature must carry.
    ///
    /// Built here rather than declared, so the policy and the workflow cannot
    /// disagree about which of them is authoritative.
    pub fn certificate_identity(&self) -> String {
        format!(
            "https://github.com/{}/{}@{}",
            self.repository, self.workflow, self.ref_
        )
    }
}

/// Where a node pulls from, in order (§13.1, §14.2).
#[derive(Debug, Clone, Deserialize)]
pub struct Registries {
    /// The port the local registry serves on. Its host is derived from the node
    /// that runs it, never written down twice.
    pub port: u16,
    /// Tried in order when the local registry does not answer --- which it does
    /// not, by design, during its own node's reboot.
    pub fallbacks: Vec<String>,
}

/// One legal container runtime (§8.2).
#[derive(Debug, Clone, Deserialize)]
pub struct Runtime {
    /// `docker` or `podman-compat`.
    pub id: String,
    /// The packages the build installs for it. The build fails loudly when they
    /// are unavailable; it never silently substitutes the other runtime.
    pub packages: Vec<String>,
    /// The socket unit whose Docker API ping `CI-03` asserts.
    pub socket_unit: String,
    /// What `DOCKER_HOST` is set to.
    pub docker_host: String,
    /// What it is.
    pub description: String,
}

/// One node image (§8.4).
#[derive(Debug, Clone, Deserialize)]
pub struct Variant {
    /// Variant identifier, matching the node it is built for.
    pub id: String,
    /// The node in `model/cluster.toml` this variant is built for.
    pub node: String,
    /// Which [`Runtime`] this variant declares.
    pub runtime: String,
    /// Packages beyond the base.
    pub packages: Vec<String>,
    /// Native systemd units enabled on this variant.
    pub services: Vec<String>,
    /// Kernel arguments beyond the base.
    pub kargs: Vec<String>,
    /// Quadlets beyond the base.
    #[serde(default)]
    pub quadlet: Vec<Quadlet>,
    /// Actions runners this node hosts (§9.5).
    #[serde(default)]
    pub runner: Vec<Runner>,
    /// Devcontainer Features added to every session this node starts (§11.1).
    ///
    /// Added with `--additional-features` rather than written into any
    /// `devcontainer.json`: §1 puts that file out of scope, and the tunnel is a
    /// property of how this cluster runs containers, not of any project.
    #[serde(default)]
    pub features: Vec<String>,
    /// Network filesystems mounted. `n3` declares none, and the model check
    /// enforces that rather than trusting this file to stay that way (§2.3).
    #[serde(default)]
    pub mount: Vec<Mount>,
    /// CPU isolation, on the variant that has any (§8.5).
    #[serde(default)]
    pub isolation: Option<Isolation>,
}

/// A Quadlet unit shipped inside the image under
/// `/usr/share/containers/systemd/`, which materializes as a service unit at
/// boot --- the bootc grain exactly (§8.2).
#[derive(Debug, Clone, Deserialize)]
pub struct Quadlet {
    /// Unit name without its extension.
    pub name: String,
    /// `container`, `volume`, or `network`.
    pub kind: String,
    /// The image, pinned as tightly as its upstream allows.
    pub image: String,
    /// `PublishPort=` entries.
    #[serde(default)]
    pub publish: Vec<String>,
    /// `Network=`, when the unit needs one.
    #[serde(default)]
    pub network: Option<String>,
    /// What the unit is for.
    pub description: String,
    /// Arguments the container is started with.
    ///
    /// Some settings a service takes are command-line only --- Prometheus's
    /// retention is the case that forced this field. Declaring the retention in
    /// `model/policy.toml` and having no way to pass it would be a threshold
    /// that renders into a document and never reaches the process.
    #[serde(default)]
    pub exec: Vec<String>,
    /// Volume mounts. Every one carries a relabel flag (§8.3).
    #[serde(default)]
    pub mount: Vec<QuadletMount>,
}

/// One volume mount on a Quadlet.
#[derive(Debug, Clone, Deserialize)]
pub struct QuadletMount {
    /// Host path.
    pub source: String,
    /// Path inside the container.
    pub target: String,
    /// Mount options other than the relabel flag.
    pub options: String,
    /// `Z` for a private label, `z` for a shared one. Declared here and
    /// rendered, never hand-written, because a missing relabel is an AVC denial
    /// at boot and `CB-` treats a denial as a build failure (§8.3).
    pub relabel: String,
}

/// An Actions runner (§9.5).
#[derive(Debug, Clone, Deserialize)]
pub struct Runner {
    /// Runner name.
    pub name: String,
    /// Labels a workflow targets it by.
    pub labels: Vec<String>,
    /// `--ephemeral` runners exit after one job, which is what makes drain a
    /// matter of not re-registering rather than of killing work (§14.1).
    pub ephemeral: bool,
    /// A systemd concurrency lock, on the node that needs one (§9.5).
    #[serde(default)]
    pub concurrency: Option<u32>,
}

/// A network filesystem mount (§11.2).
#[derive(Debug, Clone, Deserialize)]
pub struct Mount {
    /// What is mounted.
    pub what: String,
    /// Where it is mounted. `where` is a Rust keyword, so the field carries the
    /// trailing underscore and serde carries the model's spelling.
    #[serde(rename = "where")]
    pub where_: String,
    /// Filesystem type.
    #[serde(rename = "type")]
    pub fstype: String,
    /// Mount options.
    pub options: String,
}

/// CPU isolation on the measurement node (§8.5).
#[derive(Debug, Clone, Deserialize)]
pub struct Isolation {
    /// The isolated set, as the kernel spells it.
    pub isolated_cpus: String,
    /// The scaling governor pinned on every CPU.
    pub governor: String,
    /// Where interrupts are steered, which is away from the isolated set.
    pub irq_affinity: String,
}

impl ImagesFile {
    /// Look up a variant by its node name.
    pub fn variant_for(&self, node: &str) -> Option<&Variant> {
        self.variant.iter().find(|v| v.node == node)
    }

    /// Look up a runtime by id.
    pub fn runtime(&self, id: &str) -> Option<&Runtime> {
        self.runtime.iter().find(|r| r.id == id)
    }

    /// The runtime a variant actually builds against.
    pub fn runtime_of(&self, variant: &Variant) -> Option<&Runtime> {
        self.runtime(&variant.runtime)
    }
}

impl Variant {
    /// Every Quadlet this variant runs: the base's, then its own.
    pub fn all_quadlets<'a>(&'a self, base: &'a Base) -> Vec<&'a Quadlet> {
        base.quadlet.iter().chain(self.quadlet.iter()).collect()
    }

    /// Every kernel argument this variant boots with.
    pub fn all_kargs(&self, base: &Base) -> Vec<String> {
        base.content
            .kargs
            .iter()
            .chain(self.kargs.iter())
            .cloned()
            .collect()
    }
}

impl QuadletMount {
    /// The `Volume=` value a Quadlet takes, relabel flag included.
    pub fn volume_line(&self) -> String {
        format!(
            "{}:{}:{},{}",
            self.source, self.target, self.options, self.relabel
        )
    }
}
