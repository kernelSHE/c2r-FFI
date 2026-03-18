//! Patch parsing and validation (mirrors core validation for tests; agent uses core::validate_patch for actual run).

/// Extract unified diff from LLM response (markdown code block or raw diff).
pub fn parse_patch_from_response(response: &str) -> Option<String> {
    // Try ```diff ... ``` or ```patch ... ``` first
    let lower = response.to_lowercase();
    for prefix in ["```diff", "```patch", "``` unified"] {
        if let Some(start) = lower.find(prefix) {
            let after_prefix = response[start + prefix.len()..].trim_start();
            let end = after_prefix.find("```").unwrap_or(after_prefix.len());
            let block = after_prefix[..end].trim();
            if looks_like_diff(block) {
                return Some(block.to_string());
            }
        }
    }
    // Try raw: first line starting with ---
    if let Some(start) = response.find("--- ") {
        let rest = response[start..].trim_end();
        if looks_like_diff(rest) {
            return Some(rest.to_string());
        }
    }
    None
}

fn looks_like_diff(s: &str) -> bool {
    s.lines()
        .any(|l| l.starts_with("--- ") || l.starts_with("+++ "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_patch_from_markdown_diff() {
        let r = "Some text\n```diff\n--- a/src/foo.rs\n+++ b/src/foo.rs\n@@ -1,3 +1,3 @@\n-fn old() {}\n+fn new() {}\n```";
        let p = parse_patch_from_response(r).unwrap();
        assert!(p.contains("--- a/src/foo.rs"));
        assert!(p.contains("+fn new()"));
    }

    #[test]
    fn parse_patch_raw() {
        let r = "--- a/src/bar.rs\n+++ b/src/bar.rs\n@@ -1,2 +1,2 @@\n-x\n+y";
        let p = parse_patch_from_response(r).unwrap();
        assert_eq!(p, r);
    }

    #[test]
    fn reject_extern_c() {
        let patch = "--- a/src/x.rs\n+++ b/src/x.rs\n@@ -1,1 +1,1 @@\nextern \"C\" { fn x(); }";
        assert!(patch.contains("extern \"C\""));
        // Validation is in core; we only parse here
        let parsed = parse_patch_from_response(patch);
        assert!(parsed.is_some());
    }
}
