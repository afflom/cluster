//! R1 over infrastructure: the `generated/` tree equals the model (`SPEC.md`
//! §7.2).
//!
//! The template applies R1 to one file. This gate applies it to a tree: every
//! `.network` file, the firewall, the hosts file, every Quadlet, the kernel
//! arguments, the timer units, the kickstarts and `ssh_config`. A hand-edited
//! `.network` file is the same class of error as a hand-edited `CONFORMANCE.md`,
//! and it fails here for the same reason.
//!
//! Two checks, and the second is the one that matters.
//!
//! **The bytes agree.** Regenerate in memory, compare against what is
//! committed, and report the first file that differs. This catches a hand-edit
//! and it catches a model change nobody re-rendered.
//!
//! **Every file is asserted about.** Each rendered file names the conformance
//! IDs that assert over it, and those are cross-referenced against the register.
//! Without this the gate would be satisfied by a tree that renders perfectly and
//! that nothing tests --- which is precisely the vacuous gate `AGENTS.md` warns
//! about, passing green over content it never reads.

use std::collections::BTreeSet;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use cluster_model::render::{ASSERTED_BY, GENERATED_MARKER};
use cluster_model::{render_all, Cluster, GENERATED_DIR};
use repo_model::{class_of, Model};

use crate::Fail;

/// The ID class that asserts over rendered artifacts (`SPEC.md` §19.2).
const RENDER_CLASS: &str = "CD";

/// Regenerate the tree, or check that the committed one equals it.
pub fn check_render(root: &Path, write: bool) -> Result<(), Fail> {
    let cluster = Cluster::load(&root.join("model"))?;
    cluster.check()?;
    let model = Model::load(&root.join("model"))?;

    let files = render_all(&cluster);
    if files.is_empty() {
        return Err("the model renders no artifacts, which cannot be right (§7.2)".into());
    }

    let dir = root.join(GENERATED_DIR);

    if write {
        // Remove first, so that a file the model stopped rendering --- a retired
        // variant's Quadlet, say --- does not survive as a stale artifact the
        // check would then never look at (§17.2).
        if dir.exists() {
            std::fs::remove_dir_all(&dir)?;
        }
        for file in &files {
            let path = dir.join(&file.path);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            write_mode(&path, &file.contents())?;
        }
        println!(
            "render: wrote {} files under {}",
            files.len(),
            dir.display()
        );
        return Ok(());
    }

    // ---- the bytes agree ----
    let mut problems = Vec::new();
    for file in &files {
        let path = dir.join(&file.path);
        match std::fs::read_to_string(&path) {
            Err(e) => problems.push(format!("{}: {e}", file.path)),
            Ok(committed) if committed != file.contents() => {
                problems.push(format!(
                    "{}: the committed file disagrees with the model",
                    file.path
                ));
            }
            Ok(_) => {}
        }
    }

    // A file the model does not render is a file nothing owns. It would keep
    // shipping inside an image with nothing regenerating it and nothing
    // asserting over it, which is the stale-artifact failure §17.2 exists to
    // turn into a build failure.
    let expected: BTreeSet<PathBuf> = files.iter().map(|f| dir.join(&f.path)).collect();
    for found in walk(&dir)? {
        if !expected.contains(&found) {
            problems.push(format!(
                "{}: present under {GENERATED_DIR}/ but rendered by nothing (§17.2)",
                found.strip_prefix(&dir).unwrap_or(&found).display()
            ));
        }
    }

    if !problems.is_empty() {
        return Err(format!(
            "R1: the model is the single source, and `{GENERATED_DIR}/` is rendered from it. \
             A hand-edited artifact is the same class of error as a hand-edited \
             CONFORMANCE.md. Run `just render`.\n\n{}",
            problems.join("\n")
        )
        .into());
    }

    // ---- every file is asserted about ----
    //
    // The class rule §19.2 anticipated for the first `CD-` row: every file under
    // `generated/` names at least one registered `CD-` ID in its header.
    let mut gaps = Vec::new();
    let mut asserted: BTreeSet<&str> = BTreeSet::new();
    for file in &files {
        let contents = file.contents();
        // A strictly-validated format carries no provenance: `policy.json`
        // rejects an unknown key, and `bootc install` says so only at
        // deployment. Those files are still rendered, still diff-gated, and
        // still covered by the claims below --- what they lack is a line inside
        // the bytes, and demanding one made the image unbootable.
        let carries_provenance =
            cluster_model::render::Syntax::of(&file.body) != cluster_model::render::Syntax::Json;
        if carries_provenance && !contents.contains(GENERATED_MARKER) {
            gaps.push(format!("{}: no `{GENERATED_MARKER}` marker", file.path));
        }
        if file.ids.is_empty() {
            gaps.push(format!(
                "{}: names no conformance ID. Rendering an artifact nothing asserts \
                 about is a gap, not a convenience (§7.2)",
                file.path
            ));
        }
        let mut named_render_class = false;
        for id in &file.ids {
            if model.ids.get(id).is_none() {
                gaps.push(format!(
                    "{}: names `{id}`, which is not in the register (§7.2)",
                    file.path
                ));
            }
            if class_of(id) == Some(RENDER_CLASS) {
                named_render_class = true;
            }
            asserted.insert(id);
        }
        if !named_render_class {
            gaps.push(format!(
                "{}: names no `{RENDER_CLASS}-` ID. A rendered artifact is asserted about \
                 by the class that exists for rendered artifacts (§19.2)",
                file.path
            ));
        }
        if carries_provenance && !contents.contains(ASSERTED_BY) {
            gaps.push(format!("{}: header names no asserting IDs", file.path));
        }
    }

    // The other direction. A `CD-` row with a scenario, a test, and no rendered
    // file is a claim about a tree it does not touch --- the differential test
    // comparing the reference against itself, in a different costume.
    for row in &model.ids.id {
        if class_of(&row.id) == Some(RENDER_CLASS) && !asserted.contains(row.id.as_str()) {
            gaps.push(format!(
                "{}: registered as a `{RENDER_CLASS}-` claim but no rendered file names it. \
                 A claim about the rendered tree that touches none of it is vacuous \
                 (§7.2, §19.2)",
                row.id
            ));
        }
    }

    if !gaps.is_empty() {
        return Err(format!(
            "R1: every rendered artifact names the claims that assert over it, and every \
             `{RENDER_CLASS}-` claim names an artifact.\n\n{}",
            gaps.join("\n")
        )
        .into());
    }

    println!(
        "check-render: {} rendered files equal the model, {} CD- claims cover them (§7.2)",
        files.len(),
        asserted.len()
    );
    Ok(())
}

/// Write a rendered file with an explicit mode.
///
/// `0644`, set on the file that is created rather than inherited from whatever
/// umask the render ran under. A developer with a permissive umask rendered the
/// whole tree `0666`, the build copied the modes through, and the image shipped a
/// world-writable `policy.json` --- the one file §12.3 calls the only thing
/// between an unattended node and an arbitrary image. systemd said so about the
/// units beside it ("marked world-writable, proceeding anyway") and nothing
/// failed.
///
/// The image build chmods these too. Two places, deliberately: this one keeps the
/// committed tree right, and that one keeps it right whatever wrote it.
fn write_mode(path: &Path, contents: &str) -> Result<(), Fail> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;

    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o644)
        .open(path)?;
    file.write_all(contents.as_bytes())?;
    // `mode` only applies at creation, so an existing file keeps whatever it
    // had --- which is exactly the case that produced the 0666 tree.
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o644))?;
    Ok(())
}

/// Every file under `dir`, recursively. Missing is not empty: a `generated/`
/// that does not exist is a tree nobody rendered, and the byte comparison above
/// has already reported it file by file.
fn walk(dir: &Path) -> Result<Vec<PathBuf>, Fail> {
    let mut out = Vec::new();
    if !dir.exists() {
        return Ok(out);
    }
    let mut stack = vec![dir.to_path_buf()];
    while let Some(next) = stack.pop() {
        for entry in std::fs::read_dir(&next)? {
            let path = entry?.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                out.push(path);
            }
        }
    }
    out.sort();
    Ok(out)
}
