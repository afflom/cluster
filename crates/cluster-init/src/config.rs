//! The rendered policy this binary reads (`SPEC.md` §3.1, §4.1).
//!
//! `generated/node/init.conf` ships in the image and is diff-gated against
//! `model/` like everything else. Nothing here is a constant in this crate: a
//! route metric compiled into a binary and also written in the model is two
//! sources for one number, and the one that drifts is the one nobody reads.
//!
//! The format is `key=value` with `#` comments, which is what lets the file
//! carry the generated provenance header every other rendered artifact carries.
//! A stricter format would have been a second parser for no gain.

use std::collections::BTreeMap;

use crate::InitError;

/// Everything the node needs and nothing it can work out for itself.
#[derive(Debug, Clone, Default)]
pub struct Config {
    values: BTreeMap<String, String>,
    /// One row per role, in declaration order (§2.3).
    pub roles: Vec<RoleRow>,
}

/// How a machine comes to hold a role, and what the role implies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoleRow {
    /// `storage`, `compute`, `testbed`.
    pub id: String,
    /// `bulk-disk` for the role a machine works out for itself, `assigned` for
    /// the ones the registrar hands out.
    pub detect: String,
    /// The ordinal this role always takes, for the self-detected role only.
    pub ordinal: Option<u32>,
    /// Position in the registrar's hand-out sequence (§2.3.2).
    pub assign_order: Option<u32>,
    /// Position in the rollout sequence (§13.2).
    pub update_position: u32,
}

impl RoleRow {
    /// Whether a machine works this role out from its own hardware (§2.3.1).
    pub fn is_self_detected(&self) -> bool {
        self.detect == "bulk-disk"
    }
}

impl Config {
    /// Parse the rendered policy.
    pub fn parse(text: &str) -> Result<Self, InitError> {
        let mut values = BTreeMap::new();
        let mut roles = Vec::new();
        for (number, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (key, value) = line.split_once('=').ok_or_else(|| {
                InitError::Config(format!("line {}: `{line}` is not key=value", number + 1))
            })?;
            if key == "role" {
                roles.push(Self::role_row(value, number + 1)?);
            } else {
                values.insert(key.to_string(), value.to_string());
            }
        }
        Ok(Self { values, roles })
    }

    /// `id:detect:ordinal:assign_order:update_position`, with `-` for absent.
    fn role_row(value: &str, line: usize) -> Result<RoleRow, InitError> {
        let parts: Vec<&str> = value.split(':').collect();
        let [id, detect, ordinal, order, position] = parts.as_slice() else {
            return Err(InitError::Config(format!(
                "line {line}: `role={value}` needs five colon-separated fields"
            )));
        };
        let optional = |field: &str| -> Result<Option<u32>, InitError> {
            if field == "-" {
                return Ok(None);
            }
            field
                .parse()
                .map(Some)
                .map_err(|_| InitError::Config(format!("line {line}: `{field}` is not a number")))
        };
        Ok(RoleRow {
            id: (*id).to_string(),
            detect: (*detect).to_string(),
            ordinal: optional(ordinal)?,
            assign_order: optional(order)?,
            update_position: position.parse().map_err(|_| {
                InitError::Config(format!("line {line}: `{position}` is not a number"))
            })?,
        })
    }

    /// A declared string, or an error naming the key.
    ///
    /// An error rather than a default. A default here would be this crate's
    /// second opinion about a model fact, and the failure it hides --- a key the
    /// renderer stopped emitting --- is exactly the one worth failing the boot
    /// over.
    pub fn string(&self, key: &str) -> Result<&str, InitError> {
        self.values
            .get(key)
            .map(String::as_str)
            .ok_or_else(|| InitError::Config(format!("`{key}` is not in the rendered policy")))
    }

    /// A declared number.
    pub fn number(&self, key: &str) -> Result<u32, InitError> {
        let raw = self.string(key)?;
        raw.parse()
            .map_err(|_| InitError::Config(format!("`{key}` is `{raw}`, which is not a number")))
    }

    /// The role a machine works out for itself (§2.3.1).
    pub fn self_detected_role(&self) -> Result<&RoleRow, InitError> {
        let mut found = self.roles.iter().filter(|r| r.is_self_detected());
        let first = found.next().ok_or_else(|| {
            InitError::Config("no role is self-detected, so no machine can be the registrar".into())
        })?;
        if found.next().is_some() {
            return Err(InitError::Config(
                "more than one role is self-detected (§2.3.1)".into(),
            ));
        }
        Ok(first)
    }

    /// The roles the registrar hands out, in the order it hands them out.
    pub fn assigned_roles(&self) -> Vec<&RoleRow> {
        let mut out: Vec<&RoleRow> = self
            .roles
            .iter()
            .filter(|r| r.assign_order.is_some())
            .collect();
        out.sort_by_key(|r| r.assign_order);
        out
    }

    /// A role by name.
    pub fn role(&self, id: &str) -> Option<&RoleRow> {
        self.roles.iter().find(|r| r.id == id)
    }

    /// The fully-qualified name of an ordinal, e.g. `node2.devcluster` (§4.3).
    pub fn name_of(&self, ordinal: u32) -> Result<String, InitError> {
        Ok(self
            .string("name_template")?
            .replace("{ordinal}", &ordinal.to_string())
            .replace("{domain}", self.string("domain")?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bytes the renderer actually writes, trimmed to what matters here.
    const SAMPLE: &str = "\
# a comment
mesh_min_speed_mbps=10000
mesh_count=2
domain=devcluster
name_template=node{ordinal}.{domain}

role=storage:bulk-disk:1:-:3
role=compute:assigned:-:1:2
role=testbed:assigned:-:2:1
";

    #[test]
    fn the_rendered_policy_parses() {
        let c = Config::parse(SAMPLE).expect("it parses");
        assert_eq!(c.number("mesh_count").unwrap(), 2);
        assert_eq!(c.string("domain").unwrap(), "devcluster");
        assert_eq!(c.roles.len(), 3);
    }

    #[test]
    fn names_come_from_the_template_and_not_from_this_crate() {
        let c = Config::parse(SAMPLE).expect("it parses");
        assert_eq!(c.name_of(2).unwrap(), "node2.devcluster");
    }

    /// One self-detected role, and the rest in hand-out order (§2.3.2).
    #[test]
    fn roles_split_into_detected_and_assigned() {
        let c = Config::parse(SAMPLE).expect("it parses");
        assert_eq!(c.self_detected_role().unwrap().id, "storage");
        assert_eq!(c.self_detected_role().unwrap().ordinal, Some(1));
        assert_eq!(
            c.assigned_roles()
                .iter()
                .map(|r| r.id.as_str())
                .collect::<Vec<_>>(),
            ["compute", "testbed"],
            "the first machine to register becomes compute"
        );
    }

    /// A missing key fails the boot rather than defaulting. A default would be
    /// this crate's second opinion about a model fact.
    #[test]
    fn a_missing_key_is_an_error_and_not_a_default() {
        let c = Config::parse(SAMPLE).expect("it parses");
        let err = c.string("transit_metric").expect_err("it is absent");
        assert!(
            format!("{err}").contains("transit_metric"),
            "the error names the key: {err}"
        );
    }

    #[test]
    fn a_malformed_role_row_is_rejected() {
        assert!(Config::parse("role=storage:bulk-disk:1\n").is_err());
        assert!(Config::parse("not-a-pair\n").is_err());
    }
}
