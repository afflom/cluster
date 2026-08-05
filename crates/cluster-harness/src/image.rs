//! What can be established about an image before anything boots
//! (`SPEC.md` §8, §9.2, §10.2).
//!
//! T0's job is everything checkable without a machine, and for images that is
//! more than it looks. A Containerfile is a text file that has to agree with
//! `model/images.toml` about the base digest, the packages, the runtime, and
//! whose rendered tree it copies in --- and every one of those agreements can be
//! read rather than booted.
//!
//! This matters because the alternative is discovering a drifted Containerfile
//! in T2, thirty-five minutes and three guests later, or on a node.
//!
//! # What is *not* here
//!
//! Whether the declared runtime's socket actually answers a Docker API ping is a
//! property of a running node, and it is `CB-`'s at T1. The distinction this
//! module holds to is that a static check may assert what the build was *told*
//! to do, never what it achieved.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use cluster_model::Cluster;

/// A Containerfile, read.
#[derive(Debug, Clone)]
pub struct Containerfile {
    /// Which variant it builds, or `base`.
    pub name: String,
    /// Where it lives.
    pub path: PathBuf,
    /// Its text.
    pub text: String,
}

impl Containerfile {
    /// Every non-comment line, so a rule about what the build *does* is not
    /// tripped by a comment explaining what it does not.
    pub fn effective_lines(&self) -> Vec<&str> {
        self.text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .collect()
    }

    /// Does the build issue this instruction?
    pub fn issues(&self, fragment: &str) -> bool {
        self.effective_lines().iter().any(|l| l.contains(fragment))
    }
}

/// Read every Containerfile under `images/`.
pub fn containerfiles(root: &Path) -> Vec<Containerfile> {
    let dir = root.join("images");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path().join("Containerfile");
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let name = entry.file_name().to_string_lossy().to_string();
        out.push(Containerfile { name, path, text });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

/// How a Containerfile names its base.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BaseReference {
    /// `FROM repo@sha256:…` --- pinned.
    Digest {
        /// The repository.
        repository: String,
        /// The digest.
        digest: String,
    },
    /// `FROM repo:tag` --- floating, and never acceptable for the upstream base.
    Tag {
        /// The whole reference, so the failure can quote it.
        reference: String,
    },
    /// `FROM ${ARG}` --- a variant layering on a base this build produced.
    BuildArg {
        /// The argument name.
        name: String,
    },
}

/// How each `FROM` in a Containerfile names its base.
pub fn base_references(file: &Containerfile) -> Vec<BaseReference> {
    file.effective_lines()
        .iter()
        .filter_map(|line| line.strip_prefix("FROM "))
        .map(|reference| {
            let reference = reference.trim();
            if reference.starts_with("${") {
                return BaseReference::BuildArg {
                    name: reference
                        .trim_start_matches("${")
                        .trim_end_matches('}')
                        .to_string(),
                };
            }
            match reference.split_once('@') {
                Some((repository, digest)) => BaseReference::Digest {
                    repository: repository.to_string(),
                    digest: digest.to_string(),
                },
                None => BaseReference::Tag {
                    reference: reference.to_string(),
                },
            }
        })
        .collect()
}

/// Which rendered node tree a Containerfile copies in.
///
/// A variant that copied another node's tree would ship the compute node's addresses on
/// the testbed --- a mis-wired node that boots, passes a syntax check, and is wrong.
/// The `${NODE}` form is the base's, which is parameterised because §8.1 puts
/// the rendered networkd and hosts files in the base.
pub fn copied_trees(file: &Containerfile) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for line in file.effective_lines() {
        let Some(rest) = line.strip_prefix("COPY ") else {
            continue;
        };
        for token in rest.split_whitespace() {
            let Some(tail) = token.strip_prefix("generated/") else {
                continue;
            };
            if let Some(node) = tail.split('/').next() {
                out.insert(node.to_string());
            }
        }
    }
    out
}

/// The packages a Containerfile installs.
pub fn installed_packages(file: &Containerfile) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for line in file.effective_lines() {
        // `dnf -y install a b c` and `dnf -y install \` continuations both land
        // here because the reader joins nothing: a continuation's tail is its
        // own line and its tokens are still package names.
        let tokens: Vec<&str> = line.split_whitespace().collect();
        let installing =
            tokens.windows(2).any(|w| w[1] == "install") || tokens.first() == Some(&"install");
        let after_install = if installing {
            tokens
                .iter()
                .position(|t| *t == "install")
                .map(|at| &tokens[at + 1..])
                .unwrap_or(&[])
        } else if !line.starts_with("RUN") && !line.contains('=') && !line.contains('/') {
            // A continuation line: bare package names.
            &tokens[..]
        } else {
            &[]
        };
        for token in after_install {
            let token = token.trim_end_matches('\\');
            if token.is_empty() || token.starts_with('-') || token.starts_with('&') {
                continue;
            }
            out.insert(token.to_string());
        }
    }
    out
}

/// The variant a Containerfile builds, if it builds one.
pub fn variant_of<'a>(file: &Containerfile, cluster: &'a Cluster) -> Option<&'a str> {
    cluster
        .images
        .variant
        .iter()
        .find(|v| v.id == file.name)
        .map(|v| v.id.as_str())
}

/// A workflow file, read.
#[derive(Debug, Clone)]
pub struct Workflow {
    /// Its filename.
    pub name: String,
    /// Its text.
    pub text: String,
}

/// Read every workflow under `.github/workflows/`.
pub fn workflows(root: &Path) -> Vec<Workflow> {
    let dir = root.join(".github/workflows");
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_none_or(|e| e != "yml") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        out.push(Workflow {
            name: entry.file_name().to_string_lossy().to_string(),
            text,
        });
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

impl Workflow {
    /// The workflow's instructions: every line that is not a comment.
    ///
    /// A workflow explains itself, and prose that names an instruction must not
    /// be mistaken for the instruction --- the same reason `audit-deferral`
    /// spells its markers in halves and skips backticked mentions. A gate that
    /// reads comments as code cannot be reasoned about by whoever writes them.
    fn instructions(&self) -> impl Iterator<Item = &str> {
        self.text
            .lines()
            .map(str::trim)
            .filter(|l| !l.starts_with('#'))
    }

    /// Does the workflow issue this instruction?
    pub fn does(&self, fragment: &str) -> bool {
        self.instructions().any(|l| l.contains(fragment))
    }

    /// Which instruction, counting from the first, issues this fragment.
    ///
    /// Ordering questions --- "is it signed before it is copied?" --- are asked
    /// over instructions, never over bytes, so the header comment explaining the
    /// ordering does not answer the question about it.
    pub fn step_of(&self, fragment: &str) -> Option<usize> {
        self.instructions().position(|l| l.contains(fragment))
    }
}
