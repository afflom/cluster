//! The typed shape of `model/*.toml`.
//!
//! Nothing here interprets the model; [`crate::Model::check`] does that. These
//! types exist so that a malformed model is a parse error rather than a
//! silently wrong constant.

use serde::Deserialize;

use crate::ModelError;

/// One of the three honesty levels (R2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Level {
    /// A fact reproduced from an authority. Not established here.
    SomeTrue,
    /// Constructed here and validated against its oracle.
    Build,
    /// Measured and reported, never asserted.
    Open,
}

impl Level {
    /// The token used in `model/*.toml` and in generated documentation.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SomeTrue => "some-true",
            Self::Build => "build",
            Self::Open => "open",
        }
    }
}

/// `model/ledger.toml`.
#[derive(Debug, Clone, Deserialize)]
pub struct Ledger {
    /// The schema tag.
    pub spec: String,
    /// One row per claim.
    pub claim: Vec<Claim>,
}

/// One claim, at exactly one honesty level.
#[derive(Debug, Clone, Deserialize)]
pub struct Claim {
    /// The conformance ID, or an `AUTH-`/`OPEN-` prefixed identifier.
    pub id: String,
    /// The honesty level. Untagged claims do not ship (R2).
    pub level: Level,
    /// What is claimed.
    pub statement: String,
    /// The Gherkin file carrying the scenario (R3).
    #[serde(default)]
    pub feature: Option<String>,
    /// The authority a `some-true` claim is reproduced from.
    #[serde(default)]
    pub authority: Option<String>,
    /// Recorded sample size, for a claim that is a statistic.
    #[serde(default)]
    pub sample_size: Option<u64>,
    /// Recorded seed, for a claim that is a statistic.
    #[serde(default)]
    pub seed: Option<u64>,
}

impl Ledger {
    /// The meta-gate's structural half: every claim is well formed for its
    /// level (R2).
    ///
    /// The behavioural half --- that no test asserts an `open` claim as
    /// established --- lives in `repo-conformance`, because it needs the
    /// test names, not the model.
    pub fn check(&self) -> Result<(), ModelError> {
        for c in &self.claim {
            match c.level {
                Level::SomeTrue => {
                    if c.authority.is_none() {
                        return Err(ModelError::Inconsistent(format!(
                            "{}: a some-true claim must name the authority it is \
                             reproduced from",
                            c.id
                        )));
                    }
                }
                Level::Build => {
                    if c.feature.is_none() {
                        return Err(ModelError::Inconsistent(format!(
                            "{}: a build claim must name the Gherkin scenario that \
                             validates it (R3)",
                            c.id
                        )));
                    }
                    if c.authority.is_some() {
                        return Err(ModelError::Inconsistent(format!(
                            "{}: a build claim is evidence, not a reproduction of an \
                             authority; it must not name one",
                            c.id
                        )));
                    }
                }
                Level::Open => {
                    if c.authority.is_some() {
                        return Err(ModelError::Inconsistent(format!(
                            "{}: an open claim is a measurement and cannot cite an \
                             authority for its value",
                            c.id
                        )));
                    }
                }
            }
            // The first class rule this repository has (SPEC.md §19.2): an
            // `OPEN-` row carries its sample size and its seed. An open claim is
            // measured and reported, and a measurement with neither is a number
            // with no way to tell whether it was taken once or a thousand times
            // --- which is the difference between a reported quantity and an
            // asserted one. The template left this slot empty on purpose and
            // said a repository adding a class adds its rule in the commit that
            // adds the first ID in it; this is that commit.
            if c.id.starts_with("OPEN-") {
                if c.level != Level::Open {
                    return Err(ModelError::Inconsistent(format!(
                        "{}: an OPEN- row is an `open` claim, not `{}` (SPEC.md §19.2)",
                        c.id,
                        c.level.as_str()
                    )));
                }
                if c.sample_size.is_none() || c.seed.is_none() {
                    return Err(ModelError::Inconsistent(format!(
                        "{}: an open claim carries its sample_size and its seed. A measured \
                         quantity with neither cannot be told apart from an asserted one \
                         (R2, SPEC.md §19.2)",
                        c.id
                    )));
                }
            }
        }
        Ok(())
    }

    /// Look up a claim by conformance ID.
    pub fn get(&self, id: &str) -> Option<&Claim> {
        self.claim.iter().find(|c| c.id == id)
    }
}

/// `model/ids.toml` --- the conformance ID register.
#[derive(Debug, Clone, Deserialize)]
pub struct Ids {
    /// The schema tag.
    pub spec: String,
    /// One row per conformance ID.
    pub id: Vec<IdRow>,
    /// The error types a shipped crate may let a caller see (R5).
    ///
    /// The template hard-coded this list in `xtask`, which made R5 a promise
    /// about three names nobody could change without editing a gate. Reading it
    /// from the model is what makes "every error a caller can see is one the
    /// model sanctions" literally true rather than approximately so.
    #[serde(default)]
    pub error: Vec<SanctionedError>,
}

/// One error type a shipped crate is permitted to return (R5).
#[derive(Debug, Clone, Deserialize)]
pub struct SanctionedError {
    /// The type's name, as it appears in a `Result<_, _>`.
    pub name: String,
    /// The shipped crate that defines it.
    #[serde(rename = "crate")]
    pub krate: String,
    /// The conformance ID under which the error is a sanctioned outcome.
    pub sanctioned_by: String,
    /// What condition it reports.
    pub statement: String,
}

/// One registered conformance ID.
#[derive(Debug, Clone, Deserialize)]
pub struct IdRow {
    /// The ID, e.g. `CS-04`.
    pub id: String,
    /// The honesty level of the claim (R2).
    pub level: Level,
    /// The Gherkin suite the scenario belongs to.
    pub suite: String,
    /// The lowest tier that can discharge this claim (`SPEC.md` §10.2).
    ///
    /// A model fact rather than a convention, because §19.2's rule for the
    /// `CH-` class depends on it: a hardware claim is discharged only on real
    /// nodes, and a `CH-` scenario collected by a simulated run would be a
    /// false statement about a physical machine. Recording the tier here lets
    /// the collector enforce that instead of trusting each tier to remember.
    pub tier: Tier,
    /// What the ID claims.
    pub statement: String,
}

/// Which validation tier discharges a claim (`SPEC.md` §10.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
pub enum Tier {
    /// Static: the render diff, container lint, and everything checkable
    /// without booting. Runs in `just vv`.
    T0,
    /// One node boots under OVMF and the health predicate passes.
    T1,
    /// Three nodes, mesh wired, failover, and a full simulated rollout.
    T2,
    /// Real hardware. The only tier that can establish a `CH-` claim.
    T3,
}

impl Tier {
    /// The token used in `model/ids.toml` and in generated documentation.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::T0 => "T0",
            Self::T1 => "T1",
            Self::T2 => "T2",
            Self::T3 => "T3",
        }
    }
}

impl Ids {
    /// Look up a row.
    pub fn get(&self, id: &str) -> Option<&IdRow> {
        self.id.iter().find(|r| r.id == id)
    }
}

/// `model/authorities.toml` --- what this repository cites (`CM-03`).
#[derive(Debug, Clone, Deserialize)]
pub struct Authorities {
    /// The schema tag.
    pub spec: String,
    /// One row per cited authority.
    pub authority: Vec<AuthorityRow>,
}

/// A cited authority. Never re-derived, vendored, or gated on.
#[derive(Debug, Clone, Deserialize)]
pub struct AuthorityRow {
    /// Stable identifier, e.g. `CL-MM01`.
    pub id: String,
    /// Human-readable name.
    pub name: String,
    /// What a third party needs to find the source.
    pub citation: String,
    /// A checksum over the committed artifact, or `none`.
    pub checksum: String,
    /// Why there is no checksum, when there is none.
    #[serde(default)]
    pub checksum_reason: String,
    /// What the authority says.
    pub statement: String,
    /// The conformance IDs that are evidence this library realizes it.
    #[serde(default)]
    pub realized_by: Vec<String>,
}
