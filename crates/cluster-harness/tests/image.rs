//! The `image` and `lifecycle` suites, at T0 (`SPEC.md` §8, §9.2, §12.3).
//!
//! Everything about an image that can be established before it is built or
//! booted: that its Containerfile agrees with the model about the base digest,
//! the runtime, the packages and whose rendered tree it carries, and that the
//! publication path cannot rebuild between validation and promotion.
//!
//! These run in `just vv`. The claims that need a running node --- whether the
//! runtime's socket answers, whether an unsigned image is actually refused ---
//! are registered at their tiers and live in `tests/t1.rs` and `tests/t2.rs`.

use std::path::PathBuf;

use cluster_harness::image::{
    base_references, containerfiles, copied_trees, installed_packages, workflows, BaseReference,
};
use cluster_model::Cluster;

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/cluster-harness is two below the root")
        .to_path_buf()
}

fn model() -> Cluster {
    let c = Cluster::load(&root().join("model")).expect("the cluster model loads");
    c.check().expect("the cluster model is consistent");
    c
}

/// `CI-01`: the base is pinned by the digest the model declares, never floated.
#[test]
fn the_base_is_pinned_by_the_declared_digest_ci_01() {
    let c = model();
    let files = containerfiles(&root());
    assert!(!files.is_empty(), "no Containerfiles were read");

    let mut pinned = 0usize;
    for file in &files {
        let references = base_references(file);
        assert!(
            !references.is_empty(),
            "{}: a Containerfile with no FROM",
            file.name
        );
        for reference in references {
            match reference {
                BaseReference::Digest { repository, digest } => {
                    assert_eq!(
                        repository, c.images.base.image,
                        "{}: pins a base the model does not declare",
                        file.name
                    );
                    assert_eq!(
                        digest, c.images.base.digest,
                        "{}: pins a digest the model does not declare. A repository \
                         this careful about digest-pinning downstream cannot let its \
                         Containerfile and its model disagree (§8.1)",
                        file.name
                    );
                    pinned += 1;
                }
                // A variant layers on the base this build just produced. That is
                // not a floating reference: the base it names was pinned above.
                BaseReference::BuildArg { name } => {
                    assert_eq!(name, "BASE", "{}: unexpected build argument", file.name);
                }
                BaseReference::Tag { reference } => panic!(
                    "{}: `FROM {reference}` is a floating tag. A repository this \
                     careful about digest-pinning downstream cannot float its \
                     upstream (§8.1)",
                    file.name
                ),
            }
        }
    }
    assert_eq!(
        pinned, 1,
        "exactly one Containerfile pins the upstream base"
    );
}

/// `CI-02`: the declared runtime is what each variant installs, and the build
/// never silently substitutes the other one.
#[test]
fn each_variant_installs_its_declared_runtime_ci_02() {
    let c = model();
    let files = containerfiles(&root());

    for variant in &c.images.variant {
        let file = files
            .iter()
            .find(|f| f.name == variant.id)
            .unwrap_or_else(|| panic!("variant {} has no Containerfile", variant.id));
        let runtime = c
            .images
            .runtime_of(variant)
            .expect("the model check requires a declared runtime");
        let installed = installed_packages(file);

        for package in &runtime.packages {
            assert!(
                installed.contains(package),
                "{}: declares runtime `{}` but does not install `{package}` (§8.2)",
                variant.id,
                runtime.id
            );
        }

        // The other runtime's distinguishing packages must be absent. §8.2 says
        // the build fails loudly rather than silently substituting, and a
        // variant that installed both would substitute silently by accident.
        for other in c.images.runtime.iter().filter(|r| r.id != runtime.id) {
            for package in &other.packages {
                if runtime.packages.contains(package) {
                    continue;
                }
                assert!(
                    !installed.contains(package),
                    "{}: declares `{}` but installs `{package}` from `{}` (§8.2)",
                    variant.id,
                    runtime.id,
                    other.id
                );
            }
        }

        assert!(
            file.issues(&format!("DOCKER_HOST={}", runtime.docker_host)),
            "{}: must set DOCKER_HOST to the declared runtime's socket (§8.2)",
            variant.id
        );

        // Every package the model declares for the variant is installed.
        for package in &variant.packages {
            assert!(
                installed.contains(package),
                "{}: the model declares `{package}` and the build does not install it",
                variant.id
            );
        }
    }
}

/// `CI-03`: each variant carries its own rendered tree and no other node's.
#[test]
fn each_variant_carries_only_its_own_rendered_tree_ci_03() {
    let c = model();
    let files = containerfiles(&root());

    for file in &files {
        let trees = copied_trees(file);
        if file.name == "base" {
            // §8.1 puts the rendered networkd and hosts files in the base, so
            // the base is parameterised and built once per node.
            assert!(
                trees.iter().all(|t| t == "${NODE}"),
                "{}: the base is built per node and takes its tree from ${{NODE}}, \
                 not from {trees:?}",
                file.name
            );
            continue;
        }
        for tree in &trees {
            assert_eq!(
                tree, &file.name,
                "{}: copies {}'s rendered tree. A variant carrying another node's \
                 addresses is a mis-wired node that boots and passes a syntax \
                 check (§3.1)",
                file.name, tree
            );
            assert!(
                c.cluster.node(tree).is_some(),
                "{}: copies a tree for `{tree}`, which is not a declared node",
                file.name
            );
        }
    }
}

/// `CI-04`: every image is linted as a bootc host before anything boots it.
#[test]
fn every_image_is_linted_as_a_bootc_host_ci_04() {
    let files = containerfiles(&root());
    for file in &files {
        assert!(
            file.issues("bootc container lint"),
            "{}: must run `bootc container lint`. It catches an image that is not \
             a valid bootc host before a tier spends thirty-five minutes finding \
             out (§10.2)",
            file.name
        );
    }
}

/// `CL-01`: the signature policy admits this repository's promote workflow and
/// nothing else.
#[test]
fn the_signature_policy_binds_the_promote_workflow_cl_01() {
    let c = model();
    let signing = &c.images.signing;
    let dir = root().join(cluster_model::GENERATED_DIR);

    for node in &c.cluster.node {
        let policy =
            std::fs::read_to_string(dir.join(node.name.clone()).join("containers/policy.json"))
                .expect("every node renders a signature policy");

        assert!(
            policy.contains("\"default\": [{ \"type\": \"reject\" }]"),
            "{}: an image the policy does not explicitly admit must not stage (§12.3)",
            node.name
        );
        assert!(policy.contains("sigstoreSigned"), "{}", node.name);
        assert!(policy.contains(&signing.issuer), "{}", node.name);

        // The identity names the workflow, not merely the repository. §12.3 is
        // explicit: an image signed by a different workflow in this same
        // repository does not stage either.
        // The identity must be the *workflow* form. Asserting only that it
        // "contains the workflow" is trivially true when the workflow is empty
        // --- `contains("")` holds for every string --- so the shape is asserted
        // whole, and the parts are asserted non-empty first.
        assert!(
            !signing.workflow.trim().is_empty(),
            "an empty workflow would render a policy admitting anything this \
             repository ever signed (§12.3)"
        );
        let identity = signing.certificate_identity();
        assert_eq!(
            identity,
            format!(
                "https://github.com/{}/{}@{}",
                signing.repository, signing.workflow, signing.ref_
            ),
            "the certificate identity must be the full workflow reference (§12.3)"
        );
        assert!(policy.contains(&identity), "{}: {identity}", node.name);

        // A policy naming only the repository would admit anything any workflow
        // in it ever signed, so the repository-only form must not be what was
        // rendered as the subject.
        let repository_only = format!("https://github.com/{}", signing.repository);
        assert!(
            !policy.contains(&format!("\"subjectEmail\": \"{repository_only}\"")),
            "{}: the subject must be the workflow, not the repository (§12.3)",
            node.name
        );
    }
}

/// `CL-02`: every prefix a node pulls is mirrored locally, so a pull continues
/// when the local registry is unreachable.
#[test]
fn every_pulled_prefix_is_mirrored_locally_cl_02() {
    let c = model();
    let dir = root().join(cluster_model::GENERATED_DIR);
    let storage = c
        .cluster
        .node(&c.policy.drain.migration_target)
        .expect("the migration target is a declared node");
    let local = format!("{}:{}", storage.loopback, c.images.registries.port);

    for node in &c.cluster.node {
        let conf = std::fs::read_to_string(
            dir.join(node.name.clone())
                .join("containers/registries.conf"),
        )
        .expect("every node renders a registry configuration");

        // Each `[[registry]]` block declares a mirror. `containers-registries`
        // tries mirrors before the primary location, so the local copy is
        // preferred and the primary is the fallback --- which is what keeps
        // §14.2 a window rather than an outage.
        let blocks: Vec<&str> = conf.split("[[registry]]").skip(1).collect();
        assert!(
            !blocks.is_empty(),
            "{}: no registry blocks were rendered",
            node.name
        );
        for block in &blocks {
            assert!(
                block.contains(&local),
                "{}: a prefix with no local mirror would pull over WAN even when \
                 the mesh has a copy:\n{block}",
                node.name
            );
        }

        // Every declared fallback is present, and so is this repository's own
        // namespace.
        for fallback in &c.images.registries.fallbacks {
            assert!(
                conf.contains(&format!("prefix = \"{fallback}\"")),
                "{}: {fallback} is declared in the model and not rendered",
                node.name
            );
        }
        assert!(conf.contains(&format!("ghcr.io/{}", c.images.signing.repository)));
    }
}

/// `CL-03`: build once, validate that digest, promote that digest.
///
/// The invariant §9.2 states, checked against the workflows that implement it.
/// A rebuild between validation and publication would promote something no tier
/// ever saw --- and it would look identical in every log.
#[test]
fn the_promoted_digest_is_the_validated_one_cl_03() {
    let flows = workflows(&root());
    let images = flows
        .iter()
        .find(|w| w.name == "images.yml")
        .expect("images.yml");
    let promote = flows
        .iter()
        .find(|w| w.name == "promote.yml")
        .expect("promote.yml");

    // The build captures each digest and publishes it as an output, so the
    // tiers below it consume a digest rather than a tag.
    assert!(
        images.does("GITHUB_OUTPUT"),
        "images.yml must capture each built digest as an output (§9.2)"
    );
    assert!(images.does("podman inspect"), "the digest must be captured");
    for node in ["n1", "n2", "n3"] {
        assert!(
            images.does(&format!("{node}: ${{{{ steps.push.outputs.{node} }}}}")),
            "images.yml must publish {node}'s digest"
        );
    }

    // Promotion resolves the tag to a commit and copies the digest built from
    // it. It never builds.
    assert!(
        promote.does("git rev-list -n1"),
        "promote.yml must resolve the tag to its commit (§9.3)"
    );
    assert!(
        promote.does("crane digest"),
        "it must read the built digest"
    );
    assert!(promote.does("crane copy"), "it must copy, not rebuild");
    assert!(
        !promote.does("podman build") && !promote.does("docker build"),
        "promote.yml must never rebuild between validation and publication (§9.2)"
    );

    // Signed before it is copied: a digest that reached `:stable` unsigned is
    // one every node's policy would refuse, which is a cluster-wide halt rather
    // than a caught mistake.
    let at_sign = promote.step_of("cosign sign").expect("it must sign");
    let at_copy = promote.step_of("crane copy").expect("it must copy");
    assert!(at_sign < at_copy, "sign before copying to :stable (§12.3)");

    // Serialised. Two promotions racing would leave `:stable` pointing at
    // whichever copy finished last, which is not a decision anyone made.
    assert!(promote.does("group: promote"));
    assert!(promote.does("cancel-in-progress: false"));

    // Keyless: no long-lived key to custody (§12.3).
    assert!(promote.does("id-token: write"));
}

/// `CD-12`: nothing dangles and nothing rendered is inert.
///
/// The gate this test names lives in `xtask` because it reads the whole
/// repository --- the rendered tree, the image builds, and the control plane's
/// routes. This runs it, so the claim is discharged by an execution rather than
/// by the gate happening to be wired into `just vv`.
#[test]
fn nothing_dangles_and_nothing_rendered_is_inert_cd_12() {
    let output = std::process::Command::new(env!("CARGO"))
        .args(["run", "-q", "-p", "xtask", "--", "check-wiring"])
        .current_dir(root())
        .output()
        .expect("the gate runs");

    assert!(
        output.status.success(),
        "check-wiring failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    // And the gate is not passing because it looked at nothing: it must have
    // read a rendered tree and a set of routes.
    let files = containerfiles(&root());
    assert!(!files.is_empty(), "no Containerfiles were read");
    assert!(
        root().join("generated").exists(),
        "there is no rendered tree for the gate to have checked"
    );
}

/// `CI-05`: the build targets the declared platform, and the pin records both
/// the index and the manifest inside it.
///
/// The fleet is uniform x86_64 (§2.1), so the platform is a model fact too. A
/// build that took the builder's default would produce whatever the runner
/// happened to be, and on an arm64 runner it would produce an image no node can
/// boot --- which nothing downstream would notice until T1 failed to start one.
#[test]
fn the_build_targets_the_declared_platform_ci_05() {
    let c = model();
    let base = &c.images.base;

    assert!(base.amd64_digest.starts_with("sha256:"));
    assert_ne!(
        base.amd64_digest, base.digest,
        "the index and the manifest inside it are different digests; recording \
         one twice would mean the platform pin says nothing (§8.1)"
    );
    assert_eq!(
        base.architecture, "amd64",
        "the fleet is uniform x86_64 (§2.1)"
    );

    // The date the pin was resolved, which is what the weekly bump measures
    // staleness against. Without it the bump cannot say whether it is doing
    // something (§8.1).
    assert!(
        base.resolved_on.len() == 10 && base.resolved_on.starts_with("20"),
        "resolved_on is `{}`, which is not a date the bump can subtract from",
        base.resolved_on
    );

    let flows = workflows(&root());
    let images = flows
        .iter()
        .find(|w| w.name == "images.yml")
        .expect("images.yml");
    assert!(
        images.does(&format!("--platform linux/{}", base.architecture)),
        "the build must target the declared platform, or an arm64 runner \
         produces an image no node can boot (§2.1, §8.1)"
    );

    // And the bump exists at all. A pin nobody moves is a fleet running a
    // kernel from whenever the repository was written.
    let bump = flows
        .iter()
        .find(|w| w.name == "bump.yml")
        .expect("§8.1: a scheduled workflow opens a PR bumping the digest weekly");
    assert!(bump.does("schedule:"), "the bump is scheduled");
    assert!(
        bump.does("resolved_on"),
        "it measures staleness from the model"
    );
    assert!(
        bump.does("just vv"),
        "the bump passes the full gate before it is proposed (§8.1)"
    );
    assert!(
        !bump.does("git push origin main") && bump.does("create-pull-request"),
        "CI never commits to the repository; a base change is what T2 exists to \
         catch (§9.3)"
    );
}

/// `CL-05`: the release publishes the installer and its checksum.
///
/// §12.1 calls this the root of trust, and it is the one link in the chain that
/// cannot be closed by a signature: the policy that verifies every later image
/// ships *inside* the image, so a first install cannot verify itself. Without a
/// published checksum the chain has no beginning --- a node refuses an unsigned
/// image, and the medium that installed the policy was never attested at all.
#[test]
fn the_release_publishes_the_installer_and_its_checksum_cl_05() {
    let flows = workflows(&root());
    let promote = flows
        .iter()
        .find(|w| w.name == "promote.yml")
        .expect("promote.yml");

    assert!(
        promote.does("bootc-image-builder"),
        "§12.1: the installer ISO is produced from the promoted image"
    );
    assert!(
        promote.does("anaconda-iso"),
        "an installer, not a disk image: §12.1 mounts it via virtual media"
    );
    assert!(promote.does("sha256sum"), "§12.1: its SHA-256 is published");
    assert!(
        promote.does("SHA256SUMS"),
        "the checksums are a release artifact, not a log line somebody has to find"
    );

    // Built from the digest that was promoted, not from a tag that could have
    // moved between the copy and the build.
    assert!(
        promote.does("crane digest") && promote.does("$ref@$digest"),
        "the ISO is built from the promoted digest (§9.2)"
    );

    // And the bootstrap configuration it is built with is the one in the
    // repository, so what the ISO installs is what the model renders.
    assert!(
        promote.does("/config/config.toml"),
        "the ISO carries this repository's bootstrap configuration (§12.1)"
    );

    let config = std::fs::read_to_string(root().join("bootstrap/config.toml"))
        .expect("the bootstrap configuration is committed");
    assert!(
        config.contains("kickstart"),
        "the installer configuration wraps the rendered kickstart (§12.1)"
    );
}

/// `CW-05`: the tunnel Feature installs what will actually run.
///
/// The Feature is published from this repository (§11.1) precisely so that
/// `devcontainer.json` is never touched --- §1 puts that file out of scope. What
/// can be established without running it is that the script handles the four
/// failure modes §21.5 anticipates, and that is what this asserts. Whether the
/// combination *works* is the spike in §22, and no row claims it yet.
#[test]
fn the_tunnel_feature_installs_what_will_run_cw_05() {
    let c = model();
    let dir = root().join("features-src/tunnel");

    let manifest = std::fs::read_to_string(dir.join("devcontainer-feature.json"))
        .expect("§11.1: the tunnel Feature is published from this repository");
    let install_text = std::fs::read_to_string(dir.join("install.sh")).expect("its install script");
    // Instructions only. The script explains why it does *not* use
    // `--random-name`, and a check that read its own justification as the thing
    // it forbids would be unreasonable to write around --- the same reason
    // `audit-deferral` spells its markers in halves.
    let install: String = install_text
        .lines()
        .map(str::trim)
        .filter(|l| !l.starts_with('#'))
        .collect::<Vec<_>>()
        .join("\n");

    // The variant that starts sessions references it, so the Feature and the
    // model cannot drift apart about which one is added.
    let referenced: Vec<&String> = c
        .images
        .variant
        .iter()
        .flat_map(|v| v.features.iter())
        .collect();
    assert!(
        referenced.iter().any(|f| f.contains("features/tunnel")),
        "a variant must add the Feature, or nothing ever installs it"
    );

    // The CLI is baked; the server payload is not. vscode.dev pins the server by
    // commit and ships weekly, so a baked server goes stale and is re-downloaded
    // anyway --- the layer would grow for nothing and the staleness would be
    // invisible (§21.5).
    assert!(
        install.contains("download?build=stable&os="),
        "the CLI is baked"
    );
    assert!(
        !install.contains("server-linux") && !install.contains("--install-server"),
        "the server payload must not be baked: it is pinned by commit upstream"
    );

    // UID alignment, decided at install while this still runs as root and the
    // answer is knowable. A mismatch fails authentication silently, on one
    // container, which is the worst way for it to fail.
    assert!(
        install.contains("_REMOTE_USER"),
        "the container user is read"
    );
    assert!(install.contains("id -u"), "its uid is resolved");
    assert!(
        install.contains("auth_root/$container_uid") || install.contains("${container_uid}"),
        "the auth directory must be namespaced by uid (§21.5)"
    );

    // Supervision, with backoff from the model rather than a literal.
    assert!(
        install.contains("supervise.sh"),
        "a supervisor is installed"
    );
    assert!(
        install.contains("CLUSTER_TUNNEL_BACKOFF_MAX"),
        "backoff is bounded: a tunnel failing on an expired token would otherwise \
         spin against the identity provider"
    );
    let manifest_has_backoff =
        manifest.contains("backoffInitialSeconds") && manifest.contains("backoffMaxSeconds");
    assert!(
        manifest_has_backoff,
        "the bounds are Feature options, so they come from model/policy.toml \
         rather than being a second copy inside the script"
    );

    // The name is the address. `--random-name` would change it on every restart,
    // which is exactly the property §14.3 depends on not changing.
    assert!(
        !install.contains("--random-name"),
        "the tunnel name is the address and must be stable across restarts (§14.3)"
    );
    assert!(
        install.contains(&format!("{}${{SESSION_ID}}", c.policy.tunnel.name_prefix))
            || install.contains("NAME_PREFIX}${SESSION_ID")
    );

    // And the archive path has something to call (§15.3).
    assert!(
        install.contains("unregister.sh") && install.contains("tunnel unregister"),
        "tunnel names are globally unique per account; an archive that left one \
         registered collides with any session recreated under the same id"
    );
}
