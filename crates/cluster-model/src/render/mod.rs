//! Rendering every infrastructure artifact from the model (`SPEC.md` §7.2).
//!
//! The template applies R1 to documentation. This module extends it to the
//! files that decide what a node *is*: which card carries which address, which
//! routes exist, what the firewall accepts, which containers become units, what
//! the kernel is told at boot, and how often the updater wakes.
//!
//! Every rendered file carries a header naming the conformance IDs that assert
//! over it. `cargo xtask check-render` cross-references those against the
//! register, so rendering an artifact nothing asserts about is a failure rather
//! than a silent gap --- which is the difference between a gate that reads the
//! tree and a gate that merely regenerates it.

mod firewall;
mod helpers;
mod host;
mod hosts;
mod kargs;
mod kickstart;
mod networkd;
pub(crate) mod quadlet;
mod services;
mod ssh;
mod trust;
mod units;

pub use kickstart::{KICKSTART_PLACEHOLDER, RETIRED_PLACEHOLDERS};

use crate::Cluster;

/// One rendered file, and the claims that assert over it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rendered {
    /// Path relative to `generated/`, always with `/` separators.
    pub path: String,
    /// The conformance IDs asserting over this file (§7.2).
    ///
    /// Never empty: a rendered artifact nothing asserts about is a gap, and
    /// `check-render` reports it as one.
    pub ids: Vec<&'static str>,
    /// The file's contents below the generated header.
    pub body: String,
}

/// The marker every rendered file opens with, mirroring `CONFORMANCE.md`'s.
pub const GENERATED_MARKER: &str = "@generated";

/// The text of the header line naming the asserting IDs.
///
/// Without a comment marker, because not every rendered file is commented the
/// same way --- and one of them cannot be commented at all.
pub const ASSERTED_BY: &str = "Asserted by:";

/// How a rendered file carries its provenance.
///
/// Inferred from the body rather than declared per file. A field somebody has
/// to set is a field somebody forgets, and the two ways this went wrong were
/// both silent: a `#` header made `policy.json` invalid JSON, which `bootc
/// install` rejected only at deployment; and it pushed the shebang off line one
/// of the greenboot check, which is the script deciding whether an unattended
/// reboot stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Syntax {
    /// `#` begins a comment, and the header goes first.
    Hash,
    /// An interpreted script: the shebang must stay on line one, so the header
    /// follows it.
    Script,
    /// JSON, which carries no provenance at all.
    ///
    /// Not because JSON has no comments --- fields would have served --- but
    /// because the formats this repository renders JSON *for* validate strictly.
    /// `containers-policy.json` rejects an unknown key outright, and
    /// `bootc install` reports that only at deployment. A file whose format
    /// refuses provenance does not get provenance; it is still diff-gated, and
    /// the claims that assert over it are in the register where `check-render`
    /// reads them.
    Json,
}

impl Syntax {
    /// Which syntax a body is written in.
    pub fn of(body: &str) -> Self {
        let start = body.trim_start();
        if start.starts_with("#!") {
            Self::Script
        } else if start.starts_with('{') {
            Self::Json
        } else {
            Self::Hash
        }
    }
}

impl Rendered {
    /// The full file, header included --- the bytes that go on disk.
    ///
    /// The header adapts to the file's syntax. It used to be `#` unconditionally,
    /// which made every rendered JSON document invalid and displaced the shebang
    /// on every rendered script.
    pub fn contents(&self) -> String {
        let ids = self.ids.join(", ");
        match Syntax::of(&self.body) {
            Syntax::Hash => {
                let mut out = Self::hash_header(&ids);
                out.push('\n');
                out.push_str(&self.body);
                out
            }
            Syntax::Script => {
                // The interpreter line has to be the first bytes of the file, or
                // the kernel does not see it and the script runs under whatever
                // the caller happened to use.
                let mut lines = self.body.splitn(2, '\n');
                let shebang = lines.next().unwrap_or_default();
                let rest = lines.next().unwrap_or_default();
                let mut out = String::from(shebang);
                out.push('\n');
                out.push_str(&Self::hash_header(&ids));
                out.push_str(rest);
                out
            }
            // Nothing added. An earlier version injected `_generated` and
            // `_assertedBy` fields, and `bootc install` refused the signature
            // policy with `Unknown key "_generated"` --- the schema validates
            // strictly, and a comment would have been rejected for the same
            // reason a field was.
            Syntax::Json => {
                let _ = ids;
                self.body.trim_start().to_string()
            }
        }
    }

    /// The `#`-commented header.
    fn hash_header(ids: &str) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "# {GENERATED_MARKER} by `just render` from model/. Do not edit.\n"
        ));
        out.push_str("# R1: the model is the single source. A hand-edit here is the same class\n");
        out.push_str("# of error as a hand-edited CONFORMANCE.md, and check-render says so.\n");
        out.push_str(&format!("# {ASSERTED_BY} {ids}\n"));
        out
    }

    pub(crate) fn new(path: impl Into<String>, ids: Vec<&'static str>, body: String) -> Self {
        Self {
            path: path.into(),
            ids,
            body,
        }
    }
}

/// The claim that asserts over the tree as a whole: the committed bytes equal
/// the render, and every file names the claims that cover it (§7.2).
///
/// Appended to every file here rather than repeated in each renderer, because a
/// renderer that forgot it would produce a file outside the one claim that is
/// about all of them --- and the gap would be invisible, since the file would
/// still name a `CD-` ID and pass.
pub const TREE_CLAIM: &str = "CD-09";

/// The claim that every file is valid in its own syntax (§7.2).
///
/// Appended to every file for the same reason [`TREE_CLAIM`] is: it is a
/// property of all of them, and a renderer that had to remember it would be a
/// renderer that eventually did not. It exists because the header was `#` for
/// every file at first, which made every rendered JSON document invalid and
/// displaced the shebang on every rendered script.
pub const SYNTAX_CLAIM: &str = "CD-16";

/// Render every artifact the model owns, in a stable order.
///
/// Stability matters: the tree is committed and diff-gated, so a render that
/// reordered itself between runs would produce a diff nobody wrote.
pub fn render_all(c: &Cluster) -> Vec<Rendered> {
    let mut out = Vec::new();
    out.extend(networkd::render(c));
    out.extend(firewall::render(c));
    out.push(hosts::render(c));
    out.extend(quadlet::render(c));
    out.push(kargs::render(c));
    out.extend(kargs::role_kargs(c));
    out.extend(units::render(c));
    out.extend(trust::render(c));
    out.extend(services::render(c));
    out.extend(host::render(c));
    out.extend(helpers::render(c));
    out.push(kickstart::render(c));
    out.push(ssh::render(c));
    for file in &mut out {
        file.ids.push(TREE_CLAIM);
        file.ids.push(SYNTAX_CLAIM);
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

/// Where the one node tree lives under `generated/`.
///
/// One directory, not one per node. There was a `generated/n1/`, `n2/` and
/// `n3/` when there were three images; there is one image now (§8.4), so there
/// is one tree, and every file in it is byte-identical on all three machines.
///
/// The handful of artifacts that genuinely differ per machine --- the `.network`
/// files, `/etc/hosts`, the role marker --- are not here at all. They depend on
/// an ordinal the image does not know, and `cluster-init` writes them at boot
/// (§3.3, §4.3). What is rendered is the *policy* they are written from.
pub const NODE_DIR: &str = "node";

/// A path under the node tree.
pub(crate) fn node_path(rest: impl std::fmt::Display) -> String {
    format!("{NODE_DIR}/{rest}")
}

/// An INI-style section with a trailing blank line, which is the shape
/// `systemd-networkd`, Quadlet, and unit files all take.
pub(crate) fn section(name: &str, lines: &[String]) -> String {
    let mut out = format!("[{name}]\n");
    for line in lines {
        out.push_str(line);
        out.push('\n');
    }
    out.push('\n');
    out
}
