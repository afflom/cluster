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
mod quadlet;
mod services;
mod ssh;
mod trust;
mod units;

pub use kickstart::SECRET_PLACEHOLDERS;

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

/// The prefix of the header line naming the asserting IDs.
pub const ASSERTED_BY: &str = "# Asserted by:";

impl Rendered {
    /// The full file, header included --- the bytes that go on disk.
    pub fn contents(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!(
            "# {GENERATED_MARKER} by `just render` from model/. Do not edit.\n"
        ));
        out.push_str("# R1: the model is the single source. A hand-edit here is the same class\n");
        out.push_str("# of error as a hand-edited CONFORMANCE.md, and check-render says so.\n");
        out.push_str(&format!("{ASSERTED_BY} {}\n", self.ids.join(", ")));
        out.push('\n');
        out.push_str(&self.body);
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

/// Render every artifact the model owns, in a stable order.
///
/// Stability matters: the tree is committed and diff-gated, so a render that
/// reordered itself between runs would produce a diff nobody wrote.
pub fn render_all(c: &Cluster) -> Vec<Rendered> {
    let mut out = Vec::new();
    for node in &c.cluster.node {
        out.extend(networkd::render(c, node));
        out.push(firewall::render(c, node));
        out.push(hosts::render(c, node));
        out.extend(quadlet::render(c, node));
        out.push(kargs::render(c, node));
        out.extend(units::render(c, node));
        out.extend(trust::render(c, node));
        out.extend(services::render(c, node));
        out.extend(host::render(c, node));
        out.extend(helpers::render(c, node));
        out.push(kickstart::render(c, node));
    }
    out.push(ssh::render(c));
    for file in &mut out {
        file.ids.push(TREE_CLAIM);
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
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
