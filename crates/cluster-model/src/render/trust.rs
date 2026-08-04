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

use crate::render::{node_path, Rendered};
use crate::Cluster;

pub(crate) fn render(c: &Cluster) -> Vec<Rendered> {
    vec![policy(c), registries(c)]
}

fn policy(c: &Cluster) -> Rendered {
    let s = &c.images.signing;
    let repository = format!("{}/{}", "ghcr.io", s.repository);

    let mut body = String::new();
    // Nothing but the schema. `containers-policy.json` validates strictly and
    // rejects an unknown key --- a `_comment` field was tried and `bootc install`
    // refused the policy with `Unknown key "_comment"`. The reasoning lives in
    // this function's doc comment, which is where a reader of the repository
    // looks anyway.
    body.push_str("{\n");
    // Reject by default. The interesting half of a policy is what it refuses,
    // and a default of `insecureAcceptAnything` would make every rule below it
    // decoration --- the same failure §4.4's default-drop exists to avoid.
    body.push_str("  \"default\": [{ \"type\": \"reject\" }],\n");
    body.push_str("  \"transports\": {\n");
    // The registry path: default-reject, one signed identity.
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
    // Only schema keys. A `_transparencyLog` field was added here to give the
    // model's value a reader, and `bootc install` rejected the whole policy for
    // it. The transparency log belongs where signatures are *made* --- the
    // promotion workflow --- not where they are verified.
    body.push_str("          \"rekorPublicKeyPath\": \"/etc/containers/rekor.pub\",\n");
    body.push_str("          \"signedIdentity\": { \"type\": \"matchRepository\" }\n");
    body.push_str("        }\n");
    body.push_str("      ]\n");
    body.push_str("    },\n");

    // The node's own local store, accepted.
    //
    // This is not a loophole, and it is worth being exact about why. §12.3 is
    // about what a node **stages from a registry**: that path is the `docker`
    // transport above, and it stays default-reject with one signed identity.
    // `containers-storage` is what is already on this machine, and anything
    // there arrived either through that strict path or from the installer ---
    // whose medium is anchored by the checksum §12.1 publishes and calls the
    // root of trust.
    //
    // Without this, `bootc install` cannot read the image it was told to
    // install: the installer works from local storage, the local copy carries no
    // signature, and the deployment is refused. That failure is real and was
    // observed --- "is rejected by policy" from bootc-image-builder.
    body.push_str("    \"containers-storage\": {\n");
    body.push_str("      \"\": [{ \"type\": \"insecureAcceptAnything\" }]\n");
    body.push_str("    }\n");
    body.push_str("  }\n");
    body.push_str("}\n");

    Rendered::new(
        node_path("containers/policy.json"),
        vec!["CD-11", "CL-01"],
        body,
    )
}

fn registries(c: &Cluster) -> Rendered {
    let r = &c.images.registries;
    // The registry runs where the data volume is, and that node's loopback is a
    // model fact --- derived, never written down a second time.
    let host = c
        .node_with_role(&c.policy.drain.migration_target)
        .expect("the model check requires the migration target to be a declared node");
    let local = format!("{}:{}", host.loopback, r.port);

    let mut body = String::new();
    body.push_str(&format!(
        "# Where a node pulls from. The local registry first, then the fallbacks.\n\
         #\n\
         # Zot is unreachable during the storage node's own reboot --- by design --- and a pull\n\
         # that failed then would turn §14.2's stated window into an outage. The\n\
         # fallbacks are what make the window a degradation instead.\n\n"
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
        node_path("containers/registries.conf"),
        vec!["CD-11", "CL-02"],
        body,
    )
}
