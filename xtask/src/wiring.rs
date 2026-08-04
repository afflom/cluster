//! Nothing dangles, and nothing is inert (`SPEC.md` §7.2, §17.2).
//!
//! `check-render` proves the rendered tree *equals the model*. It says nothing
//! about whether anything **consumes** it, and that turned out to be the gap
//! that mattered: a signature policy can render perfectly, be diff-gated
//! perfectly, be asserted over by a claim that passes --- and never be copied
//! into an image. Every gate is green and the node has no policy.
//!
//! So this gate reads the joins rather than the artifacts:
//!
//! - every file under `generated/` is copied into an image by some build;
//! - every executable a rendered unit invokes is produced by the build, shipped
//!   by a declared package, or a `libexec` helper this repository writes;
//! - every container image the model names *in this repository's namespace* has
//!   a Containerfile;
//! - every control-plane endpoint any component calls is a route the control
//!   plane serves.
//!
//! The last one is the reason for the whole module. `cluster-updater` posts a
//! drain to `/api/nodes/<node>/drain` on the node it is about to reboot, and
//! nothing served it. R1 makes a dangling reference a build failure rather than
//! a stale file (§17.2); this is that rule applied to references that cross a
//! language boundary, where the compiler cannot see them.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use cluster_model::{render_all, Cluster, GENERATED_DIR};

use crate::Fail;

/// Executables a rendered unit may invoke that this repository does not build.
///
/// Each is shipped by a package `model/images.toml` declares, or is in the base
/// image's coreutils. Listed rather than inferred because inferring "is this in
/// some RPM" would need a package database, and a list that is wrong fails
/// loudly here rather than at boot.
const FROM_PACKAGES: &[&str] = &[
    "/usr/bin/bash",
    "/usr/bin/env",
    "/usr/bin/podman",
    "/usr/bin/curl",
    "/usr/bin/jq",
    "/usr/bin/restic",
    "/usr/bin/rsync",
    "/usr/bin/timeout",
    "/usr/bin/systemctl",
    "/usr/bin/nft",
    "/usr/sbin/nft",
    "/usr/bin/install",
    "/usr/bin/ostree",
];

/// Run every wiring check.
pub fn check_wiring(root: &Path) -> Result<(), Fail> {
    let cluster = Cluster::load(&root.join("model"))?;
    cluster.check()?;

    let mut problems = Vec::new();
    problems.extend(every_rendered_file_is_consumed(root, &cluster)?);
    problems.extend(every_invoked_path_is_produced(root)?);
    problems.extend(every_owned_image_is_built(root, &cluster)?);
    problems.extend(every_called_endpoint_is_routed(root)?);
    problems.extend(every_mounted_config_is_rendered(root, &cluster)?);
    problems.extend(every_model_field_is_read(root)?);

    if !problems.is_empty() {
        return Err(format!(
            "R1, §17.2: a dangling reference is a build failure, not a stale file. \
             These references cross a boundary the compiler cannot see.\n\n{}",
            problems.join("\n")
        )
        .into());
    }

    println!("check-wiring: every rendered artifact is consumed and every reference resolves");
    Ok(())
}

/// Every file under `generated/` is copied into an image.
///
/// A rendered file nothing copies is inert: it passes `check-render`, it is
/// asserted over by a `CD-` claim, and it never reaches a node.
fn every_rendered_file_is_consumed(root: &Path, cluster: &Cluster) -> Result<Vec<String>, Fail> {
    let copied = copy_sources(root)?;
    let mut problems = Vec::new();

    for file in render_all(cluster) {
        // `bootstrap/` and `ssh_config` are consumed by the installer and by an
        // operator's workstation, not by an image (§11.1, §12.1).
        // `bootstrap/` is applied by the installer, `ssh_config` by an
        // operator's workstation, and `tailscale/` to the tailnet itself. None
        // of the three is consumed by an image, and all three are rendered so
        // that what they configure stays a model fact (§4.5, §11.1, §12.1).
        let operator_applied = file.path.starts_with("bootstrap/")
            || file.path.starts_with("tailscale/")
            || file.path == "ssh_config";
        if operator_applied {
            continue;
        }
        let path = PathBuf::from(&file.path);
        let node = path
            .components()
            .next()
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .unwrap_or_default();
        let relative = path
            .strip_prefix(&node)
            .unwrap_or(&path)
            .to_string_lossy()
            .to_string();

        let consumed = copied.iter().any(|source| {
            let source =
                source
                    .replace("${NODE}", &node)
                    .replacen(&format!("{GENERATED_DIR}/"), "", 1);
            let source = source.trim_start_matches(&format!("{node}/"));
            // A directory copy consumes everything beneath it.
            relative == source || (source.ends_with('/') && relative.starts_with(source))
        });

        if !consumed {
            problems.push(format!(
                "{GENERATED_DIR}/{}: rendered and copied into no image. It passes \
                 check-render, a CD- claim asserts over it, and it never reaches a \
                 node (§7.2)",
                file.path
            ));
        }
    }
    Ok(problems)
}

/// Every directory a `COPY` writes into, across the image builds.
///
/// Only directories: a copy to a file path produces that file and nothing else,
/// and treating `/etc/exports` as a prefix would make every path beginning with
/// it look produced.
fn copy_destinations(files: &[(String, String)]) -> Vec<String> {
    let mut out = Vec::new();
    for (_, text) in files {
        for line in text.lines().map(str::trim) {
            if line.starts_with('#') {
                continue;
            }
            let Some(rest) = line.strip_prefix("COPY ") else {
                continue;
            };
            if let Some(destination) = rest.split_whitespace().last() {
                if destination.ends_with('/') {
                    out.push(destination.to_string());
                }
            }
        }
    }
    out
}

/// Every `COPY generated/...` source across the image builds.
fn copy_sources(root: &Path) -> Result<Vec<String>, Fail> {
    let mut out = Vec::new();
    for file in containerfiles(root)? {
        for line in file.1.lines().map(str::trim) {
            if line.starts_with('#') {
                continue;
            }
            let Some(rest) = line.strip_prefix("COPY ") else {
                continue;
            };
            for token in rest.split_whitespace() {
                if token.starts_with(&format!("{GENERATED_DIR}/")) {
                    out.push(token.to_string());
                }
            }
        }
    }
    Ok(out)
}

/// Every configuration a Quadlet mounts read-only is rendered.
///
/// A container that mounts `/etc/prometheus` and finds nothing there starts,
/// stays up, and scrapes no one. That is worse than a unit which fails: §10.1's
/// fifth check asks whether the unit is *active*, and this one would be.
///
/// Read-only mounts under `/etc` are configuration by construction --- a
/// writable mount under `/var` is state the service creates for itself, which is
/// why the distinction is on the option and not on a list of names.
fn every_mounted_config_is_rendered(root: &Path, cluster: &Cluster) -> Result<Vec<String>, Fail> {
    let rendered: BTreeSet<String> = render_all(cluster).into_iter().map(|f| f.path).collect();
    let build_text: String = containerfiles(root)?
        .iter()
        .map(|(_, t)| t.as_str())
        .collect();

    let mut problems = Vec::new();
    for variant in &cluster.images.variant {
        for quadlet in variant.all_quadlets(&cluster.images.base) {
            for mount in &quadlet.mount {
                if !mount.options.split(',').any(|o| o == "ro") {
                    continue;
                }
                if !mount.source.starts_with("/etc/") {
                    continue;
                }
                let tail = mount.source.trim_start_matches("/etc/");
                let expected = format!("{}/{tail}", variant.node);
                let present = rendered.contains(&expected)
                    || rendered
                        .iter()
                        .any(|p| p.starts_with(&format!("{expected}/")))
                    || build_text.contains(tail);
                if !present {
                    problems.push(format!(
                        "{}: quadlet `{}` mounts {} read-only and nothing renders it. \
                         The container starts, stays active, and does nothing --- which \
                         §10.1's fifth check cannot tell from working (§5.4)",
                        variant.id, quadlet.name, mount.source
                    ));
                }
            }
        }
    }
    Ok(problems)
}

/// Every field the model declares is read by something.
///
/// The other direction from the rest of this module. A dangling reference fails
/// at boot; a *declared and unread* field fails more quietly than that --- the
/// model says the journal is capped at 2G, the register renders a document
/// saying so, and nothing on any node ever applies it. R1 makes the model the
/// single source, and a source nobody reads is not one.
///
/// The check is a name search across the crates, which is coarse in one
/// direction only: a field whose name appears in an unrelated context passes
/// when it should not. That is the acceptable failure --- the expensive one is a
/// setting nobody applies, and this catches every instance of it.
fn every_model_field_is_read(root: &Path) -> Result<Vec<String>, Fail> {
    let mut source = String::new();
    let mut files = Vec::new();
    gather(&root.join("crates"), &mut files)?;
    for path in files {
        if path.extension().is_none_or(|e| e != "rs") {
            continue;
        }
        // The type definitions are excluded. A field appears in the struct that
        // parses it whether or not anything ever uses the value, so counting
        // that as "read" would make this check pass on a model nothing applies
        // --- which is the whole condition it exists to find.
        let declaration = path
            .parent()
            .is_some_and(|p| p.ends_with("crates/cluster-model/src"))
            && path.file_stem().is_some_and(|stem| {
                ["cluster", "network", "images", "policy"]
                    .contains(&stem.to_string_lossy().as_ref())
            });
        if declaration {
            continue;
        }
        if let Ok(text) = std::fs::read_to_string(&path) {
            source.push_str(&text);
        }
    }

    let mut problems = Vec::new();
    for name in ["cluster", "network", "images", "policy"] {
        let text = std::fs::read_to_string(root.join("model").join(format!("{name}.toml")))?;
        for (number, line) in text.lines().enumerate() {
            let line = line.trim();
            // A quoted line is an element of a multi-line array, not a key ---
            // `"nohz_full=2,3",` contains an `=` and names no field.
            if line.starts_with('#') || line.starts_with('[') || line.starts_with('"') {
                continue;
            }
            let Some((key, _)) = line.split_once('=') else {
                continue;
            };
            let key = key.trim();
            if key.is_empty() || key.contains(' ') || key == "spec" {
                continue;
            }
            // The field name as Rust spells it, plus the `ref`/`where`/`type`
            // keyword renames the types carry.
            let rust = match key {
                "ref" => "ref_",
                "where" => "where_",
                "type" => "fstype",
                "crate" => "krate",
                other => other,
            };
            if !source.contains(rust) {
                problems.push(format!(
                    "model/{name}.toml:{}: `{key}` is declared and read by nothing. R1 \
                     makes the model the single source, and a source nobody reads is \
                     not one --- the setting simply never takes effect",
                    number + 1
                ));
            }
        }
    }
    problems.sort();
    problems.dedup();
    Ok(problems)
}

/// Every Containerfile, as (name, text).
fn containerfiles(root: &Path) -> Result<Vec<(String, String)>, Fail> {
    let mut out = Vec::new();
    let dir = root.join("images");
    if !dir.exists() {
        return Ok(out);
    }
    for entry in std::fs::read_dir(&dir)? {
        let entry = entry?;
        let path = entry.path().join("Containerfile");
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        out.push((entry.file_name().to_string_lossy().to_string(), text));
    }
    out.sort();
    Ok(out)
}

/// Every absolute path a rendered unit invokes is produced by something.
fn every_invoked_path_is_produced(root: &Path) -> Result<Vec<String>, Fail> {
    let files = containerfiles(root)?;
    let build_text: String = files.iter().map(|(_, t)| t.as_str()).collect();

    let mut problems = Vec::new();
    for (path, sources) in invoked_paths(root)? {
        if FROM_PACKAGES.contains(&path.as_str()) {
            continue;
        }
        // Produced by an image build: named outright, or landing inside a
        // directory some COPY ships. `COPY generated/n1/libexec/
        // /usr/libexec/cluster/` produces every helper beneath it, and a check
        // that only looked for the literal path would demand each be named.
        if build_text.contains(&path) {
            continue;
        }
        let shipped_into = copy_destinations(&files)
            .into_iter()
            .any(|destination| path.starts_with(&destination));
        if shipped_into {
            continue;
        }
        problems.push(format!(
            "{path}: invoked by {} and produced by no image build. A unit that \
             invokes a path nothing ships fails at boot, which is the most \
             expensive place to find out (§7.2)",
            sources.iter().cloned().collect::<Vec<_>>().join(", ")
        ));
    }
    Ok(problems)
}

/// Absolute executable paths that rendered units invoke, and where from.
fn invoked_paths(root: &Path) -> Result<BTreeMap<String, BTreeSet<String>>, Fail> {
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let dir = root.join(GENERATED_DIR);
    if !dir.exists() {
        return Ok(out);
    }
    let mut stack = vec![dir.clone()];
    while let Some(next) = stack.pop() {
        for entry in std::fs::read_dir(&next)? {
            let path = entry?.path();
            if path.is_dir() {
                stack.push(path);
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&path) else {
                continue;
            };
            let relative = path
                .strip_prefix(&dir)
                .unwrap_or(&path)
                .display()
                .to_string();
            for line in text.lines().map(str::trim) {
                if line.starts_with('#') {
                    continue;
                }
                // `ExecStart=`, `ExecStop=`, and a shell invocation in a check.
                for token in line.split_whitespace() {
                    let token = token.trim_start_matches("ExecStart=");
                    let token = token.trim_start_matches("ExecStop=");
                    if !token.starts_with("/usr/") {
                        continue;
                    }
                    // Directories are copy targets, not invocations.
                    if token.ends_with('/') {
                        continue;
                    }
                    out.entry(token.to_string())
                        .or_default()
                        .insert(relative.clone());
                }
            }
        }
    }
    Ok(out)
}

/// Every image the model names in this repository's namespace is built here.
fn every_owned_image_is_built(root: &Path, cluster: &Cluster) -> Result<Vec<String>, Fail> {
    let namespace = format!("ghcr.io/{}/", cluster.images.signing.repository);
    let built: BTreeSet<String> = containerfiles(root)?
        .into_iter()
        .map(|(name, _)| name)
        .collect();

    let mut problems = Vec::new();
    for variant in &cluster.images.variant {
        for quadlet in variant.all_quadlets(&cluster.images.base) {
            let Some(tail) = quadlet.image.strip_prefix(&namespace) else {
                continue;
            };
            let name = tail.split(':').next().unwrap_or(tail);
            if !built.contains(name) {
                problems.push(format!(
                    "{}: quadlet `{}` runs `{}`, which is in this repository's \
                     namespace and has no Containerfile. A reference to an image \
                     nobody builds is a unit that never starts (§17.2)",
                    variant.id, quadlet.name, quadlet.image
                ));
            }
        }
    }
    Ok(problems)
}

/// Every control-plane endpoint a component calls is a route it serves.
///
/// The check this module exists for. A caller and a router in different crates,
/// joined by a string, is precisely the reference the compiler cannot see --- and
/// `cluster-updater` posted a drain to an endpoint nothing served.
fn every_called_endpoint_is_routed(root: &Path) -> Result<Vec<String>, Fail> {
    let router = std::fs::read_to_string(root.join("crates/cluster-ctl/src/api.rs"))?;
    let routed: BTreeSet<String> = router
        .lines()
        .filter_map(|l| l.trim().strip_prefix(".route(\""))
        .filter_map(|l| l.split('"').next())
        .map(normalise_route)
        .collect();

    let mut problems = Vec::new();
    for (endpoint, sources) in called_endpoints(root)? {
        // A `*` in a route matches any one segment, so `/api/sessions/{id}/{action}`
        // serves a call to `/api/sessions/abc/migrate`. Comparing the normalised
        // strings outright would demand a literal route per action.
        if routed.iter().any(|route| serves(route, &endpoint)) {
            continue;
        }
        problems.push(format!(
            "{endpoint}: called by {} and served by no route. A caller and a router \
             in different crates are joined by a string the compiler cannot check \
             (§16.1, §17.2). Routes: {routed:?}",
            sources.iter().cloned().collect::<Vec<_>>().join(", ")
        ));
    }
    Ok(problems)
}

/// Does a route serve a call? Segment by segment, with `*` matching any one.
fn serves(route: &str, call: &str) -> bool {
    let route: Vec<&str> = route.split('/').collect();
    let call: Vec<&str> = call.split('/').collect();
    route.len() == call.len()
        && route
            .iter()
            .zip(call.iter())
            .all(|(r, c)| *r == "*" || r == c)
}

/// A route with its variable parts replaced by a placeholder, so
/// `/api/sessions/{id}`, `/api/sessions/abc123` and a shell
/// `/api/sessions/$(echo …)` all compare equal.
///
/// A trailing slash is dropped: a caller that builds `".../api/sessions/"` and
/// appends an id is naming the collection, not a route of its own.
fn normalise_route(route: &str) -> String {
    let route = route.trim_end_matches('/');
    route
        .split('/')
        .map(|segment| {
            let variable = segment.starts_with('{')
                || segment.starts_with(':')
                || segment.contains("{}")
                || segment.starts_with("$(")
                || segment.starts_with('$');
            if variable {
                "*"
            } else {
                segment
            }
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Endpoints any component calls, and where from.
fn called_endpoints(root: &Path) -> Result<BTreeMap<String, BTreeSet<String>>, Fail> {
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    let mut files: Vec<PathBuf> = Vec::new();

    // Every crate but the control plane itself, plus the rendered tree: the
    // greenboot check posts a quarantine from shell.
    for dir in [root.join("crates"), root.join(GENERATED_DIR)] {
        gather(&dir, &mut files)?;
    }

    for path in files {
        if path.starts_with(root.join("crates/cluster-ctl")) {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let relative = path
            .strip_prefix(root)
            .unwrap_or(&path)
            .display()
            .to_string();

        let mut rest = text.as_str();
        while let Some(at) = rest.find("/api/") {
            let tail = &rest[at..];
            // A path ends at whitespace or at any delimiter prose or code puts
            // around it. Without the closing bracket and backtick a doc comment
            // reading ``GET /api/auth/config`)`` becomes an endpoint nothing
            // serves --- the gate reporting on the sentence describing it.
            let end = tail
                .find(|c: char| {
                    c.is_whitespace()
                        // Not `{` or `}`: those delimit a format placeholder or a
                        // route parameter, both of which are *segments* that
                        // normalise to a wildcard, not ends of a path.
                        || matches!(c, '"' | '\\' | '\'' | '`' | ')' | ']' | ',' | ';' | '|')
                })
                .unwrap_or(tail.len());
            let endpoint = normalise_route(&tail[..end]);
            if endpoint.len() > "/api/".len() {
                out.entry(endpoint).or_default().insert(relative.clone());
            }
            rest = &tail[end.max(1)..];
        }
    }
    Ok(out)
}

fn gather(dir: &Path, out: &mut Vec<PathBuf>) -> Result<(), Fail> {
    if !dir.exists() {
        return Ok(());
    }
    for entry in std::fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            if path.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            gather(&path, out)?;
        } else {
            out.push(path);
        }
    }
    Ok(())
}
