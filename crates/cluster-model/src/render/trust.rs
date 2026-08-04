//! What a node will stage, and where it pulls from (`SPEC.md` §12.3, §14.2).
//!
//! Two files, and the first is the more important one this repository renders.
//!
//! `/etc/containers/policy.json` is the only thing standing between a node and
//! an arbitrary image. §13 applies whatever `:stable` points at, unattended,
//! with no operator present --- so the policy is not a precaution, it is the
//! precondition that makes unattended update safe to run at all. It requires a
//! sigstore signature whose OIDC issuer is GitHub and whose identity is *this
//! repository's promote workflow*: an image signed by anything else does not
//! stage, including one signed by a different workflow in the same repository.
//!
//! `registries.conf` puts the local registry first with declared fallbacks. Zot
//! is unreachable during its own node's reboot, by design, and pulls continue
//! over WAN rather than failing --- which is what makes §14.2 a stated window
//! rather than an outage.

use crate::render::Rendered;
use crate::{Cluster, Node};

pub(crate) fn render(c: &Cluster, node: &Node) -> Vec<Rendered> {
    vec![policy(c, node), registries(c, node)]
}

fn policy(c: &Cluster, node: &Node) -> Rendered {
    let s = &c.images.signing;
    let repository = format!("{}/{}", "ghcr.io", s.repository);

    let mut body = String::new();
    // No `#` preamble: this is JSON, which has none. The reasoning lives in this
    // function's doc comment and, for a reader holding only the file, in the
    // `_comment` field below.
    body.push_str("{\n");
    body.push_str(&format!(
        "  \"_comment\": \"{}'s signature policy. Default reject; the identity is a \
         workflow reference, not merely a repository, because §12.3 refuses an image \
         signed by a different workflow in this same repository.\",\n",
        node.name
    ));
    // Reject by default. The interesting half of a policy is what it refuses,
    // and a default of `insecureAcceptAnything` would make every rule below it
    // decoration --- the same failure §4.4's default-drop exists to avoid.
    body.push_str("  \"default\": [{ \"type\": \"reject\" }],\n");
    body.push_str("  \"transports\": {\n");
    body.push_str("    \"docker\": {\n");
    body.push_str(&format!("      \"{repository}\": [\n"));
    body.push_str("        {\n");
    body.push_str("          \"type\": \"sigstoreSigned\",\n");
    body.push_str("          \"fulcio\": {\n");
    body.push_str("            \"caPath\": \"/etc/containers/fulcio_ca.pem\",\n");
    body.push_str(&format!("            \"oidcIssuer\": \"{}\",\n", s.issuer));
    body.push_str(&format!(
        "            \"subjectEmail\": \"{}\"\n",
        s.certificate_identity()
    ));
    body.push_str("          },\n");
    body.push_str(&format!(
        "          \"rekorPublicKeyPath\": \"/etc/containers/rekor.pub\",\n\
                   \"_transparencyLog\": \"{}\",\n",
        s.transparency_log
    ));
    body.push_str("          \"signedIdentity\": { \"type\": \"matchRepository\" }\n");
    body.push_str("        }\n");
    body.push_str("      ]\n");
    body.push_str("    }\n");
    body.push_str("  }\n");
    body.push_str("}\n");

    Rendered::new(
        format!("{}/containers/policy.json", node.name),
        vec!["CD-11", "CL-01"],
        body,
    )
}

fn registries(c: &Cluster, node: &Node) -> Rendered {
    let r = &c.images.registries;
    // The registry runs where the data volume is, and that node's loopback is a
    // model fact --- derived, never written down a second time.
    let host = c
        .cluster
        .node(&c.policy.drain.migration_target)
        .expect("the model check requires the migration target to be a declared node");
    let local = format!("{}:{}", host.loopback, r.port);

    let mut body = String::new();
    body.push_str(&format!(
        "# Where {} pulls from. The local registry first, then the fallbacks.\n\
         #\n\
         # Zot is unreachable during {}'s own reboot --- by design --- and a pull\n\
         # that failed then would turn §14.2's stated window into an outage. The\n\
         # fallbacks are what make the window a degradation instead.\n\n",
        node.name, host.name
    ));

    body.push_str("unqualified-search-registries = [\"ghcr.io\", \"docker.io\"]\n\n");

    // The repository's own images, and each upstream the local registry
    // pull-through caches (§5.4).
    let mut prefixes = vec![format!("ghcr.io/{}", c.images.signing.repository)];
    prefixes.extend(r.fallbacks.iter().cloned());

    for prefix in prefixes {
        body.push_str(&format!("[[registry]]\nprefix = \"{prefix}\"\n"));
        body.push_str(&format!("location = \"{prefix}\"\n\n"));
        body.push_str(&format!("[[registry.mirror]]\nlocation = \"{local}\"\n"));
        // The mesh is a physically isolated L2 with exactly two endpoints per
        // segment (§4.4), and Zot serves plain HTTP on it. TLS here would need a
        // certificate authority this cluster does not have, for a segment
        // nothing else can reach.
        body.push_str("insecure = true\n\n");
    }

    Rendered::new(
        format!("{}/containers/registries.conf", node.name),
        vec!["CD-11", "CL-02"],
        body,
    )
}
