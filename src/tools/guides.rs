//! Workflow guides served as MCP resources, plus the server instructions that point to them.
//!
//! The guide bodies are the plugin's `SKILL.md` files, so clients that register the MCP
//! server without the Claude Code plugin still get the same guidance.

use rmcp::model::{Resource, ResourceContents};

/// Server instructions sent in the `initialize` response.
pub const INSTRUCTIONS: &str = "\
pollard analyzes CPU performance profiles in Firefox Profiler format (samply, perf data \
imported through samply). Use it when the user wants to profile a program, find hotspots, \
explain where time goes, or compare two runs.

Workflow:
1. Get a profile. If the user has none, record one with \
`samply record --save-only -o /tmp/profile.json.gz -- <cmd>`, or convert perf data with \
`samply import perf.data --save-only -o /tmp/profile.json.gz`. Read the \
`pollard://guides/profile-recording` resource for attaching to a running process, \
prerequisites, and pitfalls.
2. `load_profile` with the file path, then `summary` for orientation.
3. Drill down with `top_functions`, `call_tree`, `stacks_containing`, `source_for_function`, \
and `asm_for_function`. Use `compare_profiles` or `compare_functions` for before/after runs.
4. If framework noise dominates (tracing-subscriber, tokio internals, stdlib glue), read the \
`pollard://guides/view-presets` resource and build a filtered view with `create_view`.";

/// A guide served as an MCP resource.
struct Guide {
    uri: &'static str,
    name: &'static str,
    description: &'static str,
    body: &'static str,
}

const GUIDES: &[Guide] = &[
    Guide {
        uri: "pollard://guides/profile-recording",
        name: "profile-recording",
        description: "How to record a profile with samply or convert perf data, then load it into pollard.",
        body: include_str!("../../skills/profile-recording/SKILL.md"),
    },
    Guide {
        uri: "pollard://guides/view-presets",
        name: "view-presets",
        description: "Copy-paste hide_modules / hide_frames regex sets for create_view that remove Rust framework noise.",
        body: include_str!("../../skills/view-presets/SKILL.md"),
    },
];

/// Lists all guides as MCP resources.
pub fn list() -> Vec<Resource> {
    GUIDES
        .iter()
        .map(|g| {
            Resource::new(g.uri, g.name)
                .with_description(g.description)
                .with_mime_type("text/markdown")
        })
        .collect()
}

/// Returns the contents of the guide at `uri`, or `None` if no guide has that URI.
pub fn read(uri: &str) -> Option<ResourceContents> {
    let guide = GUIDES.iter().find(|g| g.uri == uri)?;
    Some(
        ResourceContents::text(strip_frontmatter(guide.body), guide.uri)
            .with_mime_type("text/markdown"),
    )
}

/// Removes a leading YAML frontmatter block. The skill metadata in it is meaningless to MCP clients.
fn strip_frontmatter(body: &str) -> &str {
    // Compare lines with their terminators trimmed so CRLF checkouts strip too.
    let mut lines = body.split_inclusive('\n');
    let mut offset = match lines.next() {
        Some(first) if first.trim_end() == "---" => first.len(),
        _ => return body,
    };
    for line in lines {
        offset += line.len();
        if line.trim_end() == "---" {
            return body[offset..].trim_start();
        }
    }
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_guide_reads_without_frontmatter() {
        for guide in GUIDES {
            let Some(ResourceContents::TextResourceContents { text, .. }) = read(guide.uri) else {
                panic!("guide {} did not read as text", guide.uri);
            };
            assert!(
                text.starts_with("# "),
                "guide {} starts with: {text:.40}",
                guide.uri
            );
        }
    }

    #[test]
    fn instructions_reference_existing_guides() {
        for uri in INSTRUCTIONS
            .split('`')
            .filter(|s| s.starts_with("pollard://"))
        {
            assert!(
                read(uri).is_some(),
                "instructions reference unknown guide {uri}"
            );
        }
    }

    #[test]
    fn strip_frontmatter_handles_line_endings() {
        assert_eq!(
            strip_frontmatter("---\nname: x\n---\n\n# Body\n"),
            "# Body\n"
        );
        assert_eq!(
            strip_frontmatter("---\r\nname: x\r\n---\r\n\r\n# Body\r\n"),
            "# Body\r\n"
        );
        assert_eq!(strip_frontmatter("---\nname: x\n---"), "");
        // Unterminated or absent frontmatter leaves the body untouched.
        assert_eq!(strip_frontmatter("---\nname: x\n"), "---\nname: x\n");
        assert_eq!(strip_frontmatter("# Body\n"), "# Body\n");
    }

    #[test]
    fn unknown_uri_is_none() {
        assert!(read("pollard://guides/nope").is_none());
    }
}
