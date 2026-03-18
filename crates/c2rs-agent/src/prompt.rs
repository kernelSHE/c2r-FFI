//! Build LLM prompt from build stderr, file snippets, mapping, and relevant IR.

use c2rs_core::BuildFixContext;
use std::collections::HashSet;
use std::path::Path;

const CONTEXT_LINES: usize = 5;

/// Collect (file path relative to crate, line number) from rustc stderr.
/// Handles " --> src/foo.rs:12:34" and "src/foo.rs:12:34" and "src/foo.rs:12:5: 12:34".
pub fn collect_error_locations(stderr: &str) -> Vec<(String, u32)> {
    let mut out = Vec::new();
    for line in stderr.lines() {
        let t = line.trim();
        // "  --> src/foo.rs:12:34" or "src/foo.rs:12:5"
        let rest = t.strip_prefix("-->").map(str::trim).unwrap_or(t);
        if let Some((path, colon_rest)) = rest.split_once(':') {
            if path.ends_with(".rs") {
                if let Ok(n) = colon_rest
                    .split(':')
                    .next()
                    .unwrap_or("0")
                    .trim()
                    .parse::<u32>()
                {
                    out.push((path.trim().to_string(), n));
                }
            }
        }
    }
    out
}

/// Read file with line numbers and ±CONTEXT_LINES context.
pub fn file_snippet(path: &Path, line: u32) -> Option<String> {
    let content = std::fs::read_to_string(path).ok()?;
    let lines: Vec<&str> = content.lines().collect();
    let one_based = line as usize;
    if one_based == 0 {
        return None;
    }
    let idx = one_based.saturating_sub(1);
    let start = idx.saturating_sub(CONTEXT_LINES);
    let end = (idx + CONTEXT_LINES + 1).min(lines.len());
    let mut out = String::new();
    for (i, &l) in lines[start..end].iter().enumerate() {
        let line_no = start + i + 1;
        out.push_str(&format!("{:4} | {}\n", line_no, l));
    }
    Some(out)
}

/// Build full prompt for the fix provider.
pub fn build_prompt(ctx: &BuildFixContext) -> Result<String, c2rs_core::Error> {
    let mut sections = Vec::new();

    sections.push("## Cargo build stderr\n```".to_string());
    sections.push(ctx.build_stderr.clone());
    sections.push("```".to_string());

    let out_dir = &ctx.out_dir;
    let meta_dir = &ctx.meta_dir;
    let src_dir = out_dir.join("src");

    let locations = collect_error_locations(&ctx.build_stderr);
    let mut seen = HashSet::new();
    for (rel_path, line) in &locations {
        if !seen.insert((rel_path.clone(), line)) {
            continue;
        }
        let full = if rel_path.starts_with("src/") {
            out_dir.join(rel_path)
        } else {
            src_dir.join(rel_path)
        };
        if let Some(snippet) = file_snippet(&full, *line) {
            sections.push(format!(
                "## File snippet: {} (around line {})\n```",
                rel_path, line
            ));
            sections.push(snippet);
            sections.push("```".to_string());
        }
    }

    let mapping_path = meta_dir.join("mapping.json");
    if mapping_path.exists() {
        let mapping_str = std::fs::read_to_string(&mapping_path)
            .map_err(|e| c2rs_core::Error::Codegen(e.to_string()))?;
        sections.push("## mapping.json (excerpt)\n```json".to_string());
        sections.push(mapping_str);
        sections.push("```".to_string());
    }

    let ir_dir = meta_dir.join("ir");
    if ir_dir.exists() {
        let mapping: std::collections::HashMap<String, String> = if mapping_path.exists() {
            let s = std::fs::read_to_string(&mapping_path).unwrap_or_default();
            serde_json::from_str(&s)
                .ok()
                .and_then(|v: serde_json::Value| v.get("files").cloned())
                .and_then(|v| serde_json::from_value(v).ok())
                .unwrap_or_default()
        } else {
            std::collections::HashMap::new()
        };
        let rs_to_c: std::collections::HashMap<String, String> =
            mapping.into_iter().map(|(k, v)| (v, k)).collect();
        let mut ir_files = Vec::new();
        for (rel_path, _) in &locations {
            let rs_rel = rel_path.strip_prefix("src/").unwrap_or(rel_path);
            if let Some(c_rel) = rs_to_c.get(rs_rel) {
                let stem = c_rel.strip_suffix(".c").unwrap_or(c_rel);
                let stem = stem.replace('/', "_");
                ir_files.push(ir_dir.join(format!("{}.json", stem)));
            }
        }
        for path in ir_files {
            if path.exists() {
                let json = std::fs::read_to_string(&path)
                    .map_err(|e| c2rs_core::Error::Codegen(e.to_string()))?;
                sections.push(format!(
                    "## IR: {}\n```json",
                    path.file_name().unwrap().to_string_lossy()
                ));
                sections.push(json);
                sections.push("```".to_string());
            }
        }
    }

    sections.push(
        r#"## Instructions
Output a single unified diff patch that fixes the compile errors.
- Do NOT add `extern "C"`.
- Prefer minimal changes; avoid large rewrites.
- Prefer safe Rust; add `unsafe` only if strictly necessary.
- Patch paths must be relative to the crate root (e.g. `src/foo.rs`).
Output only the patch (or a markdown code block with the patch)."#
            .to_string(),
    );

    Ok(sections.join("\n\n"))
}
