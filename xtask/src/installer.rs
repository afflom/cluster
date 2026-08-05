//! The installer configuration the ISO is built from (`SPEC.md` §12.1).
//!
//! `bootstrap/config.toml` is a template. It carries `@@RENDERED_KICKSTART@@`
//! where the kickstart body goes, because the kickstart is rendered from the
//! model and diff-gated (§7.2, `CD-07`) --- a configuration carrying a copy of
//! it would be a second source for the disk layout, which is exactly what R1
//! forbids. Something has to fill the placeholder before the builder runs, and
//! this is that something.
//!
//! # Why this is a task and not four lines of the workflow
//!
//! It was four lines of the workflow: a Python heredoc inside `promote.yml`,
//! doing the read, the escape and the write. Two things were wrong with that,
//! and only the second is obvious.
//!
//! Nothing compiled it. A heredoc in a YAML string is checked by no gate in
//! this repository; a typo in it is found by an operator holding a broken ISO.
//!
//! And the test that was supposed to cover it *reimplemented* it. `CL-08` read
//! `promote.yml`, confirmed it mentioned the placeholder and the kickstart
//! path, and then performed its own substitution in Rust and asserted the
//! result parsed. So it proved the escaping the test author wrote was sound. It
//! could not have caught the shipping escaping being wrong, because it never
//! ran it. A gate that tests a copy of the thing is a gate that is green while
//! the thing is broken --- and this substitution had already shipped one defect
//! (a single-quoted TOML string cannot hold a newline, and a kickstart is all
//! newlines).
//!
//! So the substitution is code, in the workspace, compiled and linted and
//! tested like everything else, and the workflow invokes it. The test and the
//! release path now run the same function.
//!
//! # The escaping, and how it is checked
//!
//! The placeholder sits inside a TOML multi-line basic string. Within one,
//! `\` and `"` are the characters that need escaping --- every `"`, not merely
//! a run of three, because a value ending in a quote would otherwise close the
//! delimiter early. Control characters other than tab and newline are not
//! permitted in a basic string at all and are escaped by codepoint.
//!
//! None of that is trusted. The written file is parsed back, and the string it
//! yields is compared against the kickstart it came from. If the escaping were
//! wrong in either direction the round trip would not hold, which is a stronger
//! statement than "it parses" and the one that actually matters: what the
//! builder reads must be, byte for byte, what the model rendered.

use std::path::{Path, PathBuf};

use cluster_model::render::KICKSTART_PLACEHOLDER;

use crate::Fail;

/// The template the release path fills.
pub const TEMPLATE: &str = "bootstrap/config.toml";

/// The rendered kickstart that fills it.
pub const KICKSTART: &str = "generated/bootstrap/node.ks";

/// Where the filled configuration is written when nothing says otherwise.
pub const DEFAULT_OUTPUT: &str = "iso/config.toml";

/// Fill the template and write the configuration the builder is given.
///
/// Returns what was written, so a caller can assert over it without reading the
/// file back.
pub fn installer_config(root: &Path, output: &Path) -> Result<String, Fail> {
    let template_path = root.join(TEMPLATE);
    let kickstart_path = root.join(KICKSTART);

    let template = std::fs::read_to_string(&template_path)
        .map_err(|e| format!("{}: {e}", template_path.display()))?;
    let kickstart = std::fs::read_to_string(&kickstart_path).map_err(|e| {
        format!(
            "{}: {e}\nrun `just render`, because the kickstart is rendered from the model",
            kickstart_path.display()
        )
    })?;

    let filled = fill(&template, &kickstart)?;

    if let Some(parent) = output.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("{}: {e}", parent.display()))?;
    }
    std::fs::write(output, &filled).map_err(|e| format!("{}: {e}", output.display()))?;
    println!(
        "installer-config: wrote {} ({} bytes of kickstart)",
        output.display(),
        kickstart.len()
    );
    Ok(filled)
}

/// Substitute the kickstart into the template, and prove the result reads back.
///
/// Separated from the I/O so that the release path and the test exercise the
/// same function over the same bytes.
pub fn fill(template: &str, kickstart: &str) -> Result<String, Fail> {
    if !template.contains(KICKSTART_PLACEHOLDER) {
        return Err(format!(
            "{TEMPLATE} no longer names {KICKSTART_PLACEHOLDER}. Either the template stopped \
             needing a kickstart --- in which case this task should go --- or the placeholder \
             was renamed and the ISO would ship whatever the template says instead (§12.1)"
        )
        .into());
    }

    let filled = template.replace(KICKSTART_PLACEHOLDER, &escape_basic(kickstart));

    // The check that matters. Not "it parses" --- what the builder reads has to
    // be, byte for byte, what the model rendered, and only a round trip says so.
    let parsed: toml::Value = filled
        .parse()
        .map_err(|e| format!("the filled configuration is not TOML: {e}"))?;
    let contents = parsed
        .get("customizations")
        .and_then(|c| c.get("installer"))
        .and_then(|i| i.get("kickstart"))
        .and_then(|k| k.get("contents"))
        .and_then(|c| c.as_str())
        .ok_or(
            "the filled configuration carries no [customizations.installer.kickstart] contents",
        )?;

    // Trailing newlines are the template's layout, not the kickstart's content:
    // the placeholder sits on its own line inside the delimiters.
    if contents.trim_end_matches('\n') != kickstart.trim_end_matches('\n') {
        return Err(format!(
            "the kickstart does not survive the round trip. The builder would install \
             something other than what `just render` produced.\n\
             rendered {} bytes, read back {} bytes",
            kickstart.trim_end_matches('\n').len(),
            contents.trim_end_matches('\n').len()
        )
        .into());
    }
    if contents.contains("@@") {
        return Err(format!(
            "a placeholder survived the substitution: the ISO would carry it literally, \
             which fails at install and nowhere earlier (§12.1)\n{}",
            contents
                .lines()
                .filter(|l| l.contains("@@"))
                .collect::<Vec<_>>()
                .join("\n")
        )
        .into());
    }
    Ok(filled)
}

/// Escape a value for a TOML multi-line basic string.
///
/// Every `"`, not merely a run of three: a value ending in a quote would
/// otherwise close the delimiter early. Tab and newline are the only control
/// characters a basic string may carry literally.
fn escape_basic(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + value.len() / 8);
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' | '\t' => out.push(ch),
            c if (c as u32) < 0x20 || c as u32 == 0x7f => {
                out.push_str(&format!("\\u{:04X}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

/// The output path from the command line, or the default.
pub fn output_path(root: &Path) -> PathBuf {
    let mut args = std::env::args().skip_while(|a| a != "--output");
    match args.nth(1) {
        Some(explicit) => PathBuf::from(explicit),
        None => root.join(DEFAULT_OUTPUT),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEMPLATE_BODY: &str = "\
[customizations.installer.kickstart]
contents = \"\"\"
@@RENDERED_KICKSTART@@
\"\"\"

[customizations.installer.modules]
disable = [\"org.fedoraproject.Anaconda.Modules.Users\"]
";

    #[test]
    fn an_ordinary_kickstart_round_trips() {
        let ks = "clearpart --all --initlabel\npart / --size 20480\n%post\necho hello\n%end\n";
        let filled = fill(TEMPLATE_BODY, ks).expect("it fills");
        let parsed: toml::Value = filled.parse().expect("it parses");
        assert_eq!(
            parsed["customizations"]["installer"]["kickstart"]["contents"]
                .as_str()
                .unwrap()
                .trim_end_matches('\n'),
            ks.trim_end_matches('\n')
        );
    }

    /// The characters that close a multi-line basic string early, or that a
    /// basic string may not carry at all. Each of these produced a file the
    /// builder would have refused, at ISO build time and nowhere earlier.
    #[test]
    fn every_character_that_would_break_the_string_survives() {
        for awkward in [
            // A shell line continuation, which the kickstart's %post uses.
            "echo one \\\n  two\n",
            // A run of three quotes closes the delimiter.
            "echo \"\"\"\n",
            // A quote immediately before the closing delimiter.
            "echo \"\n",
            // A lone backslash at the very end.
            "echo \\\n",
            // Every kind of quoting a %post is likely to carry.
            "sed -i 's/\"a\"/\\\"b\\\"/' /etc/thing\n",
            // A control character a basic string may not hold literally.
            "echo \u{7}bell\n",
            // Tabs and CRs.
            "\techo\ttabbed\r\n",
        ] {
            let filled = fill(TEMPLATE_BODY, awkward)
                .unwrap_or_else(|e| panic!("{awkward:?} must survive: {e}"));
            let parsed: toml::Value = filled
                .parse()
                .unwrap_or_else(|e| panic!("{awkward:?} produced unparseable TOML: {e}"));
            assert_eq!(
                parsed["customizations"]["installer"]["kickstart"]["contents"]
                    .as_str()
                    .unwrap()
                    .trim_end_matches('\n'),
                awkward.trim_end_matches('\n'),
                "{awkward:?} did not survive the round trip"
            );
        }
    }

    /// A template that no longer names the placeholder is a template nothing
    /// fills, which is the defect this task exists to make impossible.
    #[test]
    fn a_template_naming_no_placeholder_is_refused() {
        let err = fill("contents = \"static\"\n", "anything\n").expect_err("no placeholder");
        assert!(format!("{err}").contains(KICKSTART_PLACEHOLDER));
    }

    /// A single-quoted destination cannot hold a kickstart, and that shipped
    /// once. The round trip is what catches it rather than the eye.
    #[test]
    fn a_single_quoted_destination_is_caught() {
        let bad = "[customizations.installer.kickstart]\ncontents = \"@@RENDERED_KICKSTART@@\"\n";
        let err = fill(bad, "part /\npart /var\n").expect_err("a newline in a basic string");
        assert!(
            format!("{err}").contains("not TOML"),
            "it must say the configuration would not parse: {err}"
        );
    }
}
