//! Core IR and shared types for the C→IR→Rust pipeline.
//!
//! IR is JSON-serializable; Rust is generated only from IR (never directly from C).

pub mod codegen;
pub mod extract;
pub mod ir;
pub mod passes;
pub mod validate;

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Top-level IR for a single translation unit (one .c → one .rs).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranslationUnit {
    /// Source path (e.g. "src/foo.c") for structure preservation.
    pub source_path: String,
    /// Functions in this unit; order and count preserved for mapping.
    pub functions: Vec<Function>,
}

/// IR for a single function (C symbol ↔ Rust symbol tracked via mapping.json).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Function {
    /// C-side name (for mapping).
    pub c_name: String,
    /// Rust-side name (generated; no extern "C").
    pub rust_name: String,
    /// Placeholder for future: params, return type, body IR.
    pub params: Vec<String>,
}

/// Placeholder: parse C to IR (stub).
pub fn c_to_ir(_c_source: &str, source_path: &str) -> Result<TranslationUnit, crate::Error> {
    Ok(TranslationUnit {
        source_path: source_path.to_string(),
        functions: Vec::new(),
    })
}

/// Placeholder: generate Rust from IR (stub).
pub fn ir_to_rust(ir: &TranslationUnit) -> Result<String, crate::Error> {
    let mut out = String::new();
    for f in &ir.functions {
        out.push_str(&format!("pub fn {}(", f.rust_name));
        out.push_str(&f.params.join(", "));
        out.push_str(") {\n}\n");
    }
    if ir.functions.is_empty() {
        out.push_str("// empty unit\n");
    }
    Ok(out)
}

/// CLI/config-driven run configuration.
#[derive(Debug, Clone)]
pub struct Config {
    /// Input C project path (file or directory).
    pub input: PathBuf,
    /// Output directory for .rs and optional IR/mapping.
    pub output: PathBuf,
    /// Emit IR as JSON files.
    pub emit_ir: bool,
    /// Emit mapping.json (C ↔ Rust symbol).
    pub emit_map: bool,
    /// Enable LLM fix loop (repair only).
    pub fix: bool,
    /// Max fix-loop iterations (if fix enabled).
    pub max_iter: Option<u32>,
    /// Dry run: no writes, only log.
    pub dry_run: bool,
    /// Allow using existing output directory.
    pub force: bool,
    /// Include only .c files whose path contains any of these patterns (if non-empty).
    pub filter: Vec<String>,
    /// Exclude .c files whose path contains any of these patterns.
    pub exclude: Vec<String>,
    /// Limit number of .c files after filter/exclude.
    pub max_files: Option<usize>,
}

// --------------- Project scanner ---------------

/// Result of scanning a C project root: .c/.h lists and include dir candidates.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ProjectScan {
    /// All .c files, relative to project root (forward slashes).
    pub c_files: Vec<String>,
    /// All .h files, relative to project root (forward slashes).
    pub h_files: Vec<String>,
    /// Directories that contain at least one .h (relative, forward slashes).
    pub include_dirs: Vec<String>,
}

/// Recursively collect relative paths under `dir` with given extension.
fn collect_by_ext(root: &Path, dir: &Path, ext: &str, out: &mut Vec<String>) -> Result<(), Error> {
    let entries = std::fs::read_dir(dir).map_err(|e| Error::Scan(e.to_string()))?;
    for e in entries {
        let e = e.map_err(|e| Error::Scan(e.to_string()))?;
        let path = e.path();
        if path.is_dir() {
            collect_by_ext(root, &path, ext, out)?;
        } else if path.extension().is_some_and(|x| x == ext) {
            let rel = path
                .strip_prefix(root)
                .map_err(|e| Error::Scan(e.to_string()))?;
            out.push(rel_to_forward_slash(rel));
        }
    }
    Ok(())
}

fn rel_to_forward_slash(p: &Path) -> String {
    p.components()
        .map(|c| c.as_os_str().to_string_lossy().into_owned())
        .collect::<Vec<_>>()
        .join("/")
}

/// Scan project root for .c and .h; compute include dir candidates (dirs containing .h).
pub fn scan_project(root: &Path) -> Result<ProjectScan, Error> {
    let root = root
        .canonicalize()
        .map_err(|e| Error::Scan(e.to_string()))?;
    if !root.is_dir() {
        return Err(Error::Scan("root is not a directory".into()));
    }
    let mut c_files = Vec::new();
    let mut h_files = Vec::new();
    collect_by_ext(&root, &root, "c", &mut c_files)?;
    collect_by_ext(&root, &root, "h", &mut h_files)?;
    c_files.sort();
    h_files.sort();
    let mut include_dirs: std::collections::HashSet<String> = std::collections::HashSet::new();
    for h in &h_files {
        if let Some(parent) = Path::new(h).parent() {
            let parent_str = rel_to_forward_slash(parent);
            if !parent_str.is_empty() {
                include_dirs.insert(parent_str);
            }
        }
    }
    let include_dirs: Vec<String> = {
        let mut v: Vec<_> = include_dirs.into_iter().collect();
        v.sort();
        v
    };
    Ok(ProjectScan {
        c_files,
        h_files,
        include_dirs,
    })
}

fn include_file(rel: &str, filter: &[String], exclude: &[String]) -> bool {
    if !filter.is_empty() && !filter.iter().any(|f| rel.contains(f)) {
        return false;
    }
    if exclude.iter().any(|e| rel.contains(e)) {
        return false;
    }
    true
}

/// File-level mapping: c_file_rel -> rs_file_rel (for mapping.json).
pub fn file_mapping_from_scan(scan: &ProjectScan) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    for c_rel in &scan.c_files {
        let rs_rel = c_rel
            .strip_suffix(".c")
            .map(|s| format!("{}.rs", s))
            .unwrap_or_else(|| format!("{}.rs", c_rel));
        map.insert(c_rel.clone(), rs_rel);
    }
    map
}

/// Full mapping for mapping.json: file-level, function-level, and Unsupported reasons.
#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
pub struct MappingJson {
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub files: std::collections::HashMap<String, String>,
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub functions: std::collections::HashMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unsupported: Vec<String>,
}

/// Generate a single standalone Rust crate under out_dir: Cargo.toml, src/lib.rs, src/*.rs,
/// and c2rs.meta/mapping.json + c2rs.meta/scan.json. Rust is generated from IR only when IR exists.
pub fn emit_rust_project(
    scan: &ProjectScan,
    mapping: &std::collections::HashMap<String, String>,
    out_dir: &Path,
    _emit_mapping_file: bool,
) -> Result<(), Error> {
    let src_dir = out_dir.join("src");
    std::fs::create_dir_all(&src_dir).map_err(|e| Error::Codegen(e.to_string()))?;
    let meta_dir = out_dir.join("c2rs.meta");
    std::fs::create_dir_all(&meta_dir).map_err(|e| Error::Codegen(e.to_string()))?;
    let ir_dir = meta_dir.join("ir");

    let mut all_functions: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    let mut all_unsupported: Vec<String> = Vec::new();

    for (c_rel, rs_rel) in mapping {
        let rs_path = src_dir.join(rs_rel);
        if let Some(parent) = rs_path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| Error::Codegen(e.to_string()))?;
        }
        let is_main = rs_rel.ends_with("main.rs");
        let stem = Path::new(c_rel)
            .with_extension("")
            .to_string_lossy()
            .replace('/', "_");
        let ir_path = ir_dir.join(format!("{}.json", stem));

        let content = if ir_path.exists() {
            let json =
                std::fs::read_to_string(&ir_path).map_err(|e| Error::Codegen(e.to_string()))?;
            let module: crate::ir::ModuleIR =
                serde_json::from_str(&json).map_err(|e| Error::Codegen(e.to_string()))?;
            let module = passes::run_safe_passes_v1(module, &meta_dir)?;
            let is_main = rs_rel.ends_with("main.rs");
            let result = codegen::module_ir_to_rust_with_main(&module, is_main);
            for (c_name, rs_name) in
                codegen::function_mapping_from_module_with_main(&module, is_main)
            {
                all_functions.insert(c_name, rs_name);
            }
            all_unsupported.extend(result.unsupported_reasons);
            let mut rust = result.rust;
            if is_main && !rust.contains("fn main") {
                rust.push_str("fn main() {}\n");
            }
            rust
        } else {
            rs_file_content(c_rel, is_main)
        };
        assert!(
            !content.contains("extern \"C\""),
            "generated Rust must not contain extern \"C\""
        );
        std::fs::write(&rs_path, content).map_err(|e| Error::Codegen(e.to_string()))?;
    }

    let (lib_rs, subdir_mods) = build_mod_tree(mapping.values().map(String::as_str));
    assert!(
        !lib_rs.contains("extern \"C\""),
        "generated lib.rs must not contain extern \"C\""
    );
    std::fs::write(src_dir.join("lib.rs"), lib_rs).map_err(|e| Error::Codegen(e.to_string()))?;
    for (dir, content) in subdir_mods {
        let mod_rs_path = src_dir.join(dir).join("mod.rs");
        std::fs::create_dir_all(mod_rs_path.parent().unwrap())
            .map_err(|e| Error::Codegen(e.to_string()))?;
        std::fs::write(mod_rs_path, content).map_err(|e| Error::Codegen(e.to_string()))?;
    }

    let cargo_toml = r#"[package]
name = "converted"
version = "0.1.0"
edition = "2021"

[workspace]
"#;
    std::fs::write(out_dir.join("Cargo.toml"), cargo_toml)
        .map_err(|e| Error::Codegen(e.to_string()))?;

    let mapping_json = MappingJson {
        files: mapping.clone(),
        functions: all_functions,
        unsupported: all_unsupported,
    };
    let mapping_str =
        serde_json::to_string_pretty(&mapping_json).map_err(|e| Error::Codegen(e.to_string()))?;
    std::fs::write(meta_dir.join("mapping.json"), mapping_str)
        .map_err(|e| Error::Codegen(e.to_string()))?;
    let scan_json =
        serde_json::to_string_pretty(scan).map_err(|e| Error::Codegen(e.to_string()))?;
    std::fs::write(meta_dir.join("scan.json"), scan_json)
        .map_err(|e| Error::Codegen(e.to_string()))?;

    Ok(())
}

/// Content for each generated .rs: source comment block + module doc placeholder; main.rs gets fn main().
fn rs_file_content(c_source_rel: &str, is_main: bool) -> String {
    let mut s = String::new();
    s.push_str("//! Source: ");
    s.push_str(c_source_rel);
    s.push_str("\n//!\n");
    s.push_str("//! (Placeholder module; IR-based codegen not yet applied.)\n\n");
    if is_main {
        s.push_str("fn main() {}\n");
    }
    s
}

/// Build `lib.rs` and per-directory `mod.rs` files for a nested module tree.
///
/// `rs_paths` are relative to `src/`, e.g. `["expat/lib/xmlrole.rs"]`.
/// This will produce:
/// - `lib.rs` with `mod expat;`
/// - `src/expat/mod.rs` with `mod lib;`
/// - `src/expat/lib/mod.rs` with `mod xmlrole;`
fn build_mod_tree<'a>(
    rs_paths: impl Iterator<Item = &'a str>,
) -> (String, std::collections::BTreeMap<String, String>) {
    #[derive(Default)]
    struct DirNode {
        files: std::collections::BTreeSet<String>,
        subdirs: std::collections::BTreeSet<String>,
    }

    let mut dirs: std::collections::BTreeMap<String, DirNode> = std::collections::BTreeMap::new();
    // Ensure root node exists (represents `src/`).
    dirs.entry(String::new()).or_default();

    for p in rs_paths {
        let p = p.strip_suffix(".rs").unwrap_or(p);
        let parts: Vec<&str> = p.split('/').collect();
        if parts.is_empty() {
            continue;
        }
        if parts.len() == 1 {
            // File directly under src/: "<name>.rs"
            let root = dirs.entry(String::new()).or_default();
            root.files.insert(parts[0].to_string());
            continue;
        }

        // Walk directory prefixes, recording subdir relationships.
        let mut cur = String::new(); // "" for root
        for (i, part) in parts[..parts.len() - 1].iter().enumerate() {
            // Parent directory path.
            let parent = cur.clone();
            // Current directory path after adding `part`.
            cur = if cur.is_empty() {
                (*part).to_string()
            } else {
                format!("{}/{}", cur, part)
            };

            // Register subdir on parent (root has path "").
            let parent_node = dirs.entry(parent).or_default();
            parent_node.subdirs.insert((*part).to_string());
            // Ensure current directory node exists.
            dirs.entry(cur.clone()).or_default();

            // For intermediate dirs we keep walking; final file is handled below.
            if i == parts.len() - 2 {
                // Next iteration would be the file; stop here.
                break;
            }
        }

        // Add file to its containing directory (cur now holds the full dir path).
        let dir_node = dirs.entry(cur).or_default();
        let file_name = parts[parts.len() - 1].to_string();
        dir_node.files.insert(file_name);
    }

    // Build lib.rs from root node.
    let mut lib_out = String::from("//! Auto-generated module tree for c2rs crate.\n\n");
    if let Some(root) = dirs.get("") {
        // First, modules for files directly under src/.
        for m in &root.files {
            lib_out.push_str(&format!("mod {};\n", m));
        }
        // Then, modules for top-level directories.
        for d in &root.subdirs {
            lib_out.push_str(&format!("mod {};\n", d));
        }
    }
    lib_out.push('\n');

    // Build mod.rs content for every non-root directory.
    let mut subdir_mods: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();
    for (dir, node) in dirs {
        if dir.is_empty() {
            continue;
        }
        let mut content = String::from("//! Auto-generated submodule declarations.\n\n");
        for f in &node.files {
            content.push_str(&format!("mod {};\n", f));
        }
        for d in &node.subdirs {
            content.push_str(&format!("mod {};\n", d));
        }
        subdir_mods.insert(dir, content);
    }

    (lib_out, subdir_mods)
}

/// Run with a pre-computed scan. Emits skeleton crate + IR→Rust into src/*.rs.
pub fn run_with_scan(config: &Config, scan: &ProjectScan) -> Result<(), Error> {
    if config.dry_run {
        return Ok(());
    }
    let mapping = file_mapping_from_scan(scan);
    let out_dir = &config.output;
    emit_rust_project(scan, &mapping, out_dir, config.emit_map)?;
    Ok(())
}

/// Result of running `cargo build` in out_dir (when !fix && !dry_run).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct BuildResult {
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
}

/// Context passed to the fix provider when cargo build failed (for LLM prompt).
#[derive(Debug, Clone)]
pub struct BuildFixContext {
    pub build_stderr: String,
    pub out_dir: PathBuf,
    pub meta_dir: PathBuf,
}

/// Provider that returns a unified-diff patch string for a failed build (e.g. via Ollama).
pub trait BuildFixProvider: Send + Sync {
    fn generate_patch(&self, ctx: &BuildFixContext) -> Result<String, Error>;
}

/// Max changed lines in a single patch (reject if exceeded).
pub const PATCH_MAX_LINES: u32 = 500;
/// Max files touched in a single patch (reject if exceeded).
pub const PATCH_MAX_FILES: u32 = 10;

/// Validates a unified diff: no `extern "C"`, and within line/file limits.
pub fn validate_patch(patch: &str) -> Result<(), Error> {
    if patch.contains("extern \"C\"") || patch.contains("extern \"c\"") {
        return Err(Error::Codegen("patch must not contain extern \"C\"".into()));
    }
    let mut files = std::collections::HashSet::new();
    let mut lines_changed: u32 = 0;
    for line in patch.lines() {
        if line.starts_with("--- ") || line.starts_with("+++ ") {
            let path = line
                .trim_start_matches("--- ")
                .trim_start_matches("+++ ")
                .trim();
            let path = path.strip_prefix("a/").unwrap_or(path);
            if !path.is_empty() && !path.starts_with("/dev/null") {
                files.insert(path.to_string());
            }
        }
        if (line.starts_with('+') || line.starts_with('-'))
            && !line.starts_with("+++")
            && !line.starts_with("---")
        {
            lines_changed = lines_changed.saturating_add(1);
        }
    }
    if files.len() as u32 > PATCH_MAX_FILES {
        return Err(Error::Codegen(format!(
            "patch touches {} files (max {})",
            files.len(),
            PATCH_MAX_FILES
        )));
    }
    if lines_changed > PATCH_MAX_LINES {
        return Err(Error::Codegen(format!(
            "patch changes {} lines (max {})",
            lines_changed, PATCH_MAX_LINES
        )));
    }
    Ok(())
}

/// Applies a unified diff to out_dir (patch -p1 from out_dir).
pub fn apply_patch(out_dir: &Path, patch: &str) -> Result<(), Error> {
    let patch_file = out_dir.join("c2rs.meta").join(".tmp.patch");
    let contents = if patch.ends_with('\n') {
        patch.to_string()
    } else {
        format!("{}\n", patch)
    };
    std::fs::write(&patch_file, contents).map_err(|e| Error::Codegen(e.to_string()))?;
    let out = std::process::Command::new("patch")
        .args(["-p1", "-i", patch_file.to_str().unwrap()])
        .current_dir(out_dir)
        .output()
        .map_err(|e| Error::Codegen(e.to_string()))?;
    let _ = std::fs::remove_file(&patch_file);
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(Error::Codegen(format!("patch failed: {}", stderr)));
    }
    Ok(())
}

/// Run cargo build in out_dir and return BuildResult.
pub fn run_cargo_build(out_dir: &Path) -> Result<BuildResult, Error> {
    let out = std::process::Command::new("cargo")
        .arg("build")
        .current_dir(out_dir)
        .output()
        .map_err(|e| Error::Codegen(e.to_string()))?;
    Ok(BuildResult {
        success: out.status.success(),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    })
}

/// Run the fix loop: get patch from provider, validate, apply, save to patches/iter_N.patch, rebuild; repeat until success or max_iter.
pub fn run_fix_loop(
    out_dir: &Path,
    meta_dir: &Path,
    mut build_result: BuildResult,
    fix_provider: &dyn BuildFixProvider,
    max_iter: u32,
) -> Result<BuildResult, Error> {
    let patches_dir = meta_dir.join("patches");
    std::fs::create_dir_all(&patches_dir).map_err(|e| Error::Codegen(e.to_string()))?;
    for iter in 0..max_iter {
        let ctx = BuildFixContext {
            build_stderr: build_result.stderr.clone(),
            out_dir: out_dir.to_path_buf(),
            meta_dir: meta_dir.to_path_buf(),
        };
        let patch = fix_provider.generate_patch(&ctx)?;
        validate_patch(&patch)?;
        let patch_path = patches_dir.join(format!("iter_{}.patch", iter));
        std::fs::write(&patch_path, &patch).map_err(|e| Error::Codegen(e.to_string()))?;
        apply_patch(out_dir, &patch)?;
        build_result = run_cargo_build(out_dir)?;
        if build_result.success {
            return Ok(build_result);
        }
    }
    Ok(build_result)
}

/// Full pipeline: scan → (if --emit-ir) C→IR → skeleton + IR→Rust → cargo build; on build failure and --fix, run fix loop. All metadata under c2rs.meta/.
pub fn run(config: &Config, fix_provider: Option<&dyn BuildFixProvider>) -> Result<(), Error> {
    let mut scan = scan_project(&config.input)?;

    if !config.filter.is_empty() || !config.exclude.is_empty() || config.max_files.is_some() {
        let mut selected: Vec<String> = scan
            .c_files
            .iter()
            .filter(|c| include_file(c, &config.filter, &config.exclude))
            .cloned()
            .collect();
        selected.sort();
        if let Some(max) = config.max_files {
            if selected.len() > max {
                selected.truncate(max);
            }
        }
        scan.c_files = selected;
    }

    if config.dry_run {
        let _ = file_mapping_from_scan(&scan);
        return Ok(());
    }

    let out_dir = &config.output;
    let meta_dir = out_dir.join("c2rs.meta");
    std::fs::create_dir_all(&meta_dir).map_err(|e| Error::Codegen(e.to_string()))?;

    if config.emit_ir {
        extract::extract_ir_for_project(&config.input, out_dir, &scan)?;
    }

    run_with_scan(config, &scan)?;

    let max_iter = config.max_iter.unwrap_or(5);
    let mut build_result = if config.dry_run {
        None
    } else {
        Some(run_cargo_build(out_dir)?)
    };

    if let Some(ref mut result) = build_result {
        if !result.success && config.fix {
            if let Some(provider) = fix_provider {
                *result = run_fix_loop(
                    out_dir,
                    &meta_dir,
                    std::mem::take(result),
                    provider,
                    max_iter,
                )?;
            }
        }
    }

    if let Some(ref result) = build_result {
        let result_json =
            serde_json::to_string_pretty(result).map_err(|e| Error::Codegen(e.to_string()))?;
        std::fs::write(meta_dir.join("build_result.json"), result_json)
            .map_err(|e| Error::Codegen(e.to_string()))?;
        if !result.success {
            return Err(Error::Codegen(format!(
                "cargo build failed: {}",
                result.stderr
            )));
        }
    }

    Ok(())
}

/// Core errors.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("parse error: {0}")]
    Parse(String),
    #[error("codegen error: {0}")]
    Codegen(String),
    #[error("scan error: {0}")]
    Scan(String),
    #[error("extract error: {0}")]
    Extract(String),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::io::Write;

    #[test]
    fn c_to_ir_stub_returns_unit() {
        let ir = c_to_ir("int main() {}", "main.c").unwrap();
        assert_eq!(ir.source_path, "main.c");
        assert!(ir.functions.is_empty());
    }

    #[test]
    fn ir_to_rust_empty_unit() {
        let ir = TranslationUnit {
            source_path: "x.c".into(),
            functions: vec![],
        };
        let s = ir_to_rust(&ir).unwrap();
        assert!(s.contains("empty unit"));
    }

    #[test]
    fn scan_project_c_and_h_and_include_dirs() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let a = root.join("a");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(a.join("inc")).unwrap();
        fs::File::create(a.join("foo.c"))
            .unwrap()
            .write_all(b"void foo() {}")
            .unwrap();
        fs::File::create(a.join("bar.c"))
            .unwrap()
            .write_all(b"void bar() {}")
            .unwrap();
        fs::File::create(a.join("inc").join("common.h"))
            .unwrap()
            .write_all(b"#define X 1")
            .unwrap();

        let scan = scan_project(root).unwrap();
        assert_eq!(scan.c_files, ["a/bar.c", "a/foo.c"]);
        assert_eq!(scan.h_files, ["a/inc/common.h"]);
        assert_eq!(scan.include_dirs, ["a/inc"]);
    }

    #[test]
    fn file_mapping_c_to_rs_same_name() {
        let scan = ProjectScan {
            c_files: vec!["main.c".into(), "sub/util.c".into()],
            h_files: vec![],
            include_dirs: vec![],
        };
        let mapping = file_mapping_from_scan(&scan);
        assert_eq!(mapping.get("main.c").unwrap(), "main.rs");
        assert_eq!(mapping.get("sub/util.c").unwrap(), "sub/util.rs");
    }

    #[test]
    fn emit_rust_project_c2rs_meta_and_rs_source_comment() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::File::create(root.join("x.c"))
            .unwrap()
            .write_all(b"void x() {}")
            .unwrap();

        let scan = scan_project(root).unwrap();
        let mapping = file_mapping_from_scan(&scan);
        let out = tempfile::tempdir().unwrap();
        emit_rust_project(&scan, &mapping, out.path(), true).unwrap();

        let meta = out.path().join("c2rs.meta");
        let mapping_path = meta.join("mapping.json");
        let mapping_json: MappingJson =
            serde_json::from_str(&fs::read_to_string(mapping_path).unwrap()).unwrap();
        assert_eq!(mapping_json.files.get("x.c").unwrap(), "x.rs");

        let scan_path = meta.join("scan.json");
        let scan_back: ProjectScan =
            serde_json::from_str(&fs::read_to_string(scan_path).unwrap()).unwrap();
        assert_eq!(scan_back.c_files, scan.c_files);

        let src = out.path().join("src");
        for entry in fs::read_dir(src).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if path.extension().is_some_and(|e| e == "rs") {
                let content = fs::read_to_string(&path).unwrap();
                assert!(
                    !content.contains("extern \"C\""),
                    "generated {} must not contain extern \"C\"",
                    path.display()
                );
                if path.file_name().unwrap() != "lib.rs" {
                    assert!(
                        content.contains("Source:") || content.contains("source:"),
                        "generated {} must have source comment",
                        path.display()
                    );
                }
            }
        }
    }

    #[test]
    fn validate_patch_accepts_clean_diff() {
        let patch = "--- a/src/foo.rs\n+++ b/src/foo.rs\n@@ -1,3 +1,3 @@\n fn x() {}\n-fn y() {}\n+fn z() {}\n";
        assert!(validate_patch(patch).is_ok());
    }

    #[test]
    fn validate_patch_rejects_extern_c() {
        let patch = "--- a/src/x.rs\n+++ b/src/x.rs\n@@ -1,1 +1,1 @@\nextern \"C\" { fn f(); }";
        assert!(validate_patch(patch).is_err());
        let err = validate_patch(patch).unwrap_err().to_string();
        assert!(err.contains("extern \"C\""));
    }

    #[test]
    fn validate_patch_rejects_too_many_lines() {
        let mut patch = String::from("--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1,1 +1,1 @@\n");
        for _ in 0..=PATCH_MAX_LINES {
            patch.push_str("+x\n");
        }
        assert!(validate_patch(&patch).is_err());
        let err = validate_patch(&patch).unwrap_err().to_string();
        assert!(err.contains("lines"));
    }

    #[test]
    fn validate_patch_rejects_too_many_files() {
        let mut patch = String::new();
        for i in 0..=PATCH_MAX_FILES as usize {
            patch.push_str(&format!(
                "--- a/src/f{}.rs\n+++ b/src/f{}.rs\n@@ -1,1 +1,1 @@\n-x\n+y\n",
                i, i
            ));
        }
        assert!(validate_patch(&patch).is_err());
        let err = validate_patch(&patch).unwrap_err().to_string();
        assert!(err.contains("files"));
    }

    #[test]
    fn apply_patch_modifies_file() {
        let out = tempfile::tempdir().unwrap();
        let src = out.path().join("src");
        fs::create_dir_all(&src).unwrap();
        let meta = out.path().join("c2rs.meta");
        fs::create_dir_all(&meta).unwrap();
        let foo = src.join("foo.rs");
        fs::write(&foo, "fn old() {}\n").unwrap();
        let patch =
            "--- a/src/foo.rs\n+++ b/src/foo.rs\n@@ -1,1 +1,1 @@\n-fn old() {}\n+fn new() {}\n";
        apply_patch(out.path(), patch).unwrap();
        let content = fs::read_to_string(&foo).unwrap();
        assert!(
            content.contains("fn new()"),
            "apply_patch should change content: {}",
            content
        );
    }
}
