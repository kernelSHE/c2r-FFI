//! Large-project validation infrastructure.
//!
//! Goal: track per-file progress through stages, summarize unsupported features, and emit reports
//! without requiring full conversion success.

use crate::codegen;
use crate::extract;
use crate::ir::{ExprIR, ModuleIR, StmtIR, TypeIR};
use crate::{Error, MappingJson, ProjectScan};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::info;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Stage {
    Scanned,
    AstGenerated,
    LoweredToIr,
    RustGenerated,
    RustChecked,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FileStageStatus {
    pub scanned: bool,
    pub ast_generated: bool,
    pub lowered_to_ir: bool,
    pub rust_generated: bool,
    pub rust_checked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failed_stage: Option<Stage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FileUnsupportedStats {
    /// classification -> count
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub counts: BTreeMap<String, u32>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct FileValidationResult {
    /// C file path relative to project root (forward slashes).
    pub c_file: String,
    /// Rust file path relative to crate root (e.g. "src/foo.rs").
    pub rs_file: String,
    pub stages: FileStageStatus,
    #[serde(default)]
    pub unsupported: FileUnsupportedStats,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UnsupportedSummary {
    /// classification -> count
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub counts: BTreeMap<String, u32>,
    /// top N entries (classification, count)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub top: Vec<(String, u32)>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ValidationReport {
    pub project_root: String,
    pub out_dir: String,
    pub generated_at_unix: u64,

    pub filter: Vec<String>,
    pub exclude: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max_files: Option<usize>,
    pub jobs: usize,
    pub per_file_check: bool,

    pub total_files_selected: usize,
    pub totals_by_failed_stage: BTreeMap<String, u32>,

    pub unsupported: UnsupportedSummary,

    #[serde(default)]
    pub files: Vec<FileValidationResult>,
}

#[derive(Debug, Clone)]
pub struct ValidateConfig {
    pub project_root: PathBuf,
    pub out_dir: PathBuf,
    pub filter: Vec<String>,
    pub exclude: Vec<String>,
    pub max_files: Option<usize>,
    pub per_file_check: bool,
    pub jobs: usize,
    pub top_n: usize,
}

impl Default for ValidateConfig {
    fn default() -> Self {
        Self {
            project_root: PathBuf::new(),
            out_dir: PathBuf::new(),
            filter: vec![],
            exclude: vec![],
            max_files: None,
            per_file_check: false,
            jobs: 1,
            top_n: 20,
        }
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn stem_for_rel(c_rel: &str) -> String {
    Path::new(c_rel)
        .with_extension("")
        .to_string_lossy()
        .into_owned()
        .replace('/', "_")
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

fn bump(map: &mut BTreeMap<String, u32>, key: impl Into<String>, by: u32) {
    *map.entry(key.into()).or_insert(0) += by;
}

fn collect_ir_unsupported(module: &ModuleIR) -> BTreeMap<String, u32> {
    fn walk_ty(ty: &TypeIR, out: &mut BTreeMap<String, u32>) {
        match ty {
            TypeIR::Ptr(inner) | TypeIR::Array(inner, _) => walk_ty(inner, out),
            TypeIR::Unsupported { kind, .. } => bump(out, format!("ir:type:{}", kind), 1),
            TypeIR::Named(_)
            | TypeIR::Void
            | TypeIR::Int
            | TypeIR::Bool
            | TypeIR::Float
            | TypeIR::Double => {}
        }
    }

    fn walk_expr(e: &ExprIR, out: &mut BTreeMap<String, u32>) {
        match e {
            ExprIR::Binary { left, right, .. } => {
                walk_expr(left, out);
                walk_expr(right, out);
            }
            ExprIR::Call { args, .. } => {
                for a in args {
                    walk_expr(a, out);
                }
            }
            ExprIR::ToBool(inner) => walk_expr(inner, out),
            ExprIR::Subscript { base, index } | ExprIR::CheckedSubscript { base, index } => {
                walk_expr(base, out);
                walk_expr(index, out);
            }
            ExprIR::Unsupported { kind, .. } => bump(out, format!("ir:expr:{}", kind), 1),
            ExprIR::Literal(_) | ExprIR::Var(_) => {}
        }
    }

    fn walk_stmt(s: &StmtIR, out: &mut BTreeMap<String, u32>) {
        match s {
            StmtIR::VarDecl { ty, init, .. } => {
                walk_ty(ty, out);
                if let Some(e) = init {
                    walk_expr(e, out);
                }
            }
            StmtIR::Assign { value, .. } => walk_expr(value, out),
            StmtIR::If {
                cond,
                then_body,
                else_body,
            } => {
                walk_expr(cond, out);
                for st in then_body {
                    walk_stmt(st, out);
                }
                if let Some(eb) = else_body {
                    for st in eb {
                        walk_stmt(st, out);
                    }
                }
            }
            StmtIR::While { cond, body } => {
                walk_expr(cond, out);
                for st in body {
                    walk_stmt(st, out);
                }
            }
            StmtIR::Return(Some(e)) | StmtIR::Expr(e) => walk_expr(e, out),
            StmtIR::Unsupported { kind, .. } => bump(out, format!("ir:stmt:{}", kind), 1),
            StmtIR::Return(None) => {}
        }
    }

    let mut out = BTreeMap::new();
    for f in &module.functions {
        walk_ty(&f.return_type, &mut out);
        for p in &f.params {
            walk_ty(&p.ty, &mut out);
        }
        for st in &f.body {
            walk_stmt(st, &mut out);
        }
    }
    out
}

fn collect_codegen_unsupported(reasons: &[String]) -> BTreeMap<String, u32> {
    let mut out = BTreeMap::new();
    for r in reasons {
        let parts: Vec<&str> = r.split(':').collect();
        let key = match parts.as_slice() {
            [a, b, ..] => format!("codegen:{}:{}", a, b),
            [a] => format!("codegen:{}", a),
            _ => "codegen:unknown".to_string(),
        };
        bump(&mut out, key, 1);
    }
    out
}

fn top_n(map: &BTreeMap<String, u32>, n: usize) -> Vec<(String, u32)> {
    let mut v: Vec<(String, u32)> = map.iter().map(|(k, &c)| (k.clone(), c)).collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    v.truncate(n);
    v
}

fn write_text(path: &Path, s: &str) -> Result<(), Error> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Error::Codegen(e.to_string()))?;
    }
    std::fs::write(path, s).map_err(|e| Error::Codegen(e.to_string()))?;
    Ok(())
}

fn write_report_files(out_dir: &Path, report: &ValidationReport) -> Result<(), Error> {
    let reports_dir = out_dir.join("c2rs.meta").join("reports");
    std::fs::create_dir_all(&reports_dir).map_err(|e| Error::Codegen(e.to_string()))?;
    let json = serde_json::to_string_pretty(report).map_err(|e| Error::Codegen(e.to_string()))?;
    write_text(&reports_dir.join("validation_report.json"), &json)?;
    let md = render_markdown(report);
    write_text(&reports_dir.join("validation_report.md"), &md)?;
    Ok(())
}

pub fn load_report(out_dir: &Path) -> Result<ValidationReport, Error> {
    let path = out_dir
        .join("c2rs.meta")
        .join("reports")
        .join("validation_report.json");
    let s = std::fs::read_to_string(&path).map_err(|e| Error::Codegen(e.to_string()))?;
    serde_json::from_str(&s).map_err(|e| Error::Codegen(e.to_string()))
}

pub fn render_markdown(report: &ValidationReport) -> String {
    let mut s = String::new();
    s.push_str("# Validation Report\n\n");
    s.push_str(&format!("- project_root: `{}`\n", report.project_root));
    s.push_str(&format!("- out_dir: `{}`\n", report.out_dir));
    s.push_str(&format!(
        "- selected_files: `{}`\n",
        report.total_files_selected
    ));
    s.push_str(&format!("- jobs: `{}`\n", report.jobs));
    s.push_str(&format!(
        "- per_file_check: `{}`\n\n",
        report.per_file_check
    ));

    s.push_str("## Failed stage totals\n\n");
    if report.totals_by_failed_stage.is_empty() {
        s.push_str("- (none)\n");
    } else {
        for (k, v) in &report.totals_by_failed_stage {
            s.push_str(&format!("- {}: {}\n", k, v));
        }
    }

    s.push_str("\n## Unsupported Top\n\n");
    if report.unsupported.top.is_empty() {
        s.push_str("- (none)\n");
    } else {
        for (k, v) in &report.unsupported.top {
            s.push_str(&format!("- {}: {}\n", k, v));
        }
    }

    s.push_str("\n## Files\n\n");
    for f in &report.files {
        let failed = f
            .stages
            .failed_stage
            .map(|st| format!("{:?}", st))
            .unwrap_or_else(|| "none".into());
        s.push_str(&format!(
            "- `{}` → `{}` (failed_stage: {})\n",
            f.c_file, f.rs_file, failed
        ));
    }
    s
}

fn cargo_check(out_dir: &Path) -> Result<(bool, String), Error> {
    let out = std::process::Command::new("cargo")
        .args(["check", "-q"])
        .current_dir(out_dir)
        .output()
        .map_err(|e| Error::Codegen(e.to_string()))?;
    let ok = out.status.success();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    Ok((ok, stderr))
}

fn rustc_check_isolated(rs_file: &Path) -> Result<(bool, String), Error> {
    let tmp = tempfile::tempdir().map_err(|e| Error::Codegen(e.to_string()))?;
    let root = tmp.path();
    std::fs::create_dir_all(root.join("src")).map_err(|e| Error::Codegen(e.to_string()))?;
    write_text(
        &root.join("Cargo.toml"),
        r#"[package]
name = "filecheck"
version = "0.1.0"
edition = "2021"
"#,
    )?;
    let content = std::fs::read_to_string(rs_file).map_err(|e| Error::Codegen(e.to_string()))?;
    write_text(&root.join("src/target.rs"), &content)?;
    write_text(
        &root.join("src/lib.rs"),
        "#[path = \"target.rs\"] mod target;\n",
    )?;
    let out = std::process::Command::new("cargo")
        .args(["check", "-q"])
        .current_dir(root)
        .output()
        .map_err(|e| Error::Codegen(e.to_string()))?;
    let ok = out.status.success();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    Ok((ok, stderr))
}

fn parse_rustc_stderr_files(stderr: &str) -> Vec<String> {
    // rustc format includes: " --> src/foo.rs:12:34"
    let mut out = Vec::new();
    for line in stderr.lines() {
        let t = line.trim();
        let rest = t.strip_prefix("-->").map(str::trim).unwrap_or(t);
        if let Some((path, _)) = rest.split_once(':') {
            if path.ends_with(".rs") {
                out.push(path.trim().to_string());
            }
        }
    }
    out.sort();
    out.dedup();
    out
}

fn write_scaffold(out_dir: &Path, rs_paths: &[String]) -> Result<(), Error> {
    let src_dir = out_dir.join("src");
    std::fs::create_dir_all(&src_dir).map_err(|e| Error::Codegen(e.to_string()))?;

    // Write Cargo.toml
    write_text(
        &out_dir.join("Cargo.toml"),
        r#"[package]
name = "converted"
version = "0.1.0"
edition = "2021"

[workspace]
"#,
    )?;

    // Build module tree similar to emit_rust_project (top-level + one-level subdir mod.rs).
    let mut top_level: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut subdirs: std::collections::BTreeMap<String, std::collections::BTreeSet<String>> =
        std::collections::BTreeMap::new();

    for p in rs_paths {
        let p = p.strip_prefix("src/").unwrap_or(p);
        let p = p.strip_suffix(".rs").unwrap_or(p);
        let parts: Vec<&str> = p.split('/').collect();
        if parts.len() == 1 {
            top_level.insert(parts[0].to_string());
        } else {
            let (dir, name) = (parts[0], parts[parts.len() - 1]);
            subdirs
                .entry(dir.to_string())
                .or_default()
                .insert(name.to_string());
        }
    }

    let mut lib_out = String::from("//! Auto-generated module tree for validation.\n\n");
    for m in &top_level {
        lib_out.push_str(&format!("mod {};\n", m));
    }
    for dir in subdirs.keys() {
        lib_out.push_str(&format!("mod {};\n", dir));
    }
    lib_out.push('\n');
    write_text(&src_dir.join("lib.rs"), &lib_out)?;

    for (dir, mods) in subdirs {
        let mut content = String::from("//! Auto-generated submodule declarations.\n\n");
        for m in &mods {
            content.push_str(&format!("mod {};\n", m));
        }
        let mod_rs = src_dir.join(dir).join("mod.rs");
        write_text(&mod_rs, &content)?;
    }

    Ok(())
}

fn write_meta_scan_mapping(
    out_dir: &Path,
    scan: &ProjectScan,
    mapping: &HashMap<String, String>,
) -> Result<(), Error> {
    let meta_dir = out_dir.join("c2rs.meta");
    std::fs::create_dir_all(&meta_dir).map_err(|e| Error::Codegen(e.to_string()))?;
    let scan_json =
        serde_json::to_string_pretty(scan).map_err(|e| Error::Codegen(e.to_string()))?;
    write_text(&meta_dir.join("scan.json"), &scan_json)?;
    let mapping_json = MappingJson {
        files: mapping.clone(),
        functions: HashMap::new(),
        unsupported: Vec::new(),
    };
    let mapping_str =
        serde_json::to_string_pretty(&mapping_json).map_err(|e| Error::Codegen(e.to_string()))?;
    write_text(&meta_dir.join("mapping.json"), &mapping_str)?;
    Ok(())
}

pub fn validate_project(config: &ValidateConfig) -> Result<ValidationReport, Error> {
    let scan_full = crate::scan_project(&config.project_root)?;
    let mut selected: Vec<String> = scan_full
        .c_files
        .iter()
        .filter(|c| include_file(c, &config.filter, &config.exclude))
        .cloned()
        .collect();
    selected.sort();
    if let Some(max) = config.max_files {
        selected.truncate(max);
    }

    let scan = ProjectScan {
        c_files: selected.clone(),
        h_files: scan_full.h_files.clone(),
        include_dirs: scan_full.include_dirs.clone(),
    };

    let mapping = crate::file_mapping_from_scan(&scan);
    let rs_paths: Vec<String> = mapping.values().map(|v| format!("src/{}", v)).collect();

    let out_dir = config.out_dir.clone();
    let meta_dir = out_dir.join("c2rs.meta");
    let ast_dir = meta_dir.join("ast");
    let ir_dir = meta_dir.join("ir");
    std::fs::create_dir_all(&ast_dir).map_err(|e| Error::Codegen(e.to_string()))?;
    std::fs::create_dir_all(&ir_dir).map_err(|e| Error::Codegen(e.to_string()))?;
    std::fs::create_dir_all(meta_dir.join("reports")).map_err(|e| Error::Codegen(e.to_string()))?;

    write_meta_scan_mapping(&out_dir, &scan, &mapping)?;
    write_scaffold(&out_dir, &rs_paths)?;

    let include_dirs: Vec<PathBuf> = std::iter::once(config.project_root.to_path_buf())
        .chain(
            scan.include_dirs
                .iter()
                .map(|s| config.project_root.join(s)),
        )
        .collect();
    let include_strs: Vec<String> = include_dirs
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();

    let jobs = config.jobs.max(1);
    let queue = Arc::new(Mutex::new(scan.c_files.clone()));
    let results: Arc<Mutex<Vec<FileValidationResult>>> = Arc::new(Mutex::new(Vec::new()));
    let unsupported_total: Arc<Mutex<BTreeMap<String, u32>>> =
        Arc::new(Mutex::new(BTreeMap::new()));
    let failed_totals: Arc<Mutex<BTreeMap<String, u32>>> = Arc::new(Mutex::new(BTreeMap::new()));

    let project_root = config.project_root.clone();
    let out_dir_arc = Arc::new(out_dir.clone());
    let include_strs = Arc::new(include_strs);
    let mapping_arc = Arc::new(mapping);

    let mut threads = Vec::new();
    for _ in 0..jobs {
        let queue = Arc::clone(&queue);
        let results = Arc::clone(&results);
        let unsupported_total = Arc::clone(&unsupported_total);
        let failed_totals = Arc::clone(&failed_totals);
        let project_root = project_root.clone();
        let out_dir = Arc::clone(&out_dir_arc);
        let include_strs = Arc::clone(&include_strs);
        let mapping = Arc::clone(&mapping_arc);
        let per_file_check = config.per_file_check;

        threads.push(std::thread::spawn(move || loop {
            let c_rel = {
                let mut q = queue.lock().unwrap();
                q.pop()
            };
            let Some(c_rel) = c_rel else { break };

            let rs_rel = mapping.get(&c_rel).cloned().unwrap_or_else(|| {
                c_rel
                    .strip_suffix(".c")
                    .map(|s| format!("{}.rs", s))
                    .unwrap_or_else(|| format!("{}.rs", c_rel))
            });
            let rs_file = format!("src/{}", rs_rel);

            let mut file = FileValidationResult {
                c_file: c_rel.clone(),
                rs_file: rs_file.clone(),
                stages: FileStageStatus {
                    scanned: true,
                    ..Default::default()
                },
                unsupported: FileUnsupportedStats::default(),
            };

            let stem = stem_for_rel(&c_rel);
            let ast_path = out_dir
                .join("c2rs.meta")
                .join("ast")
                .join(format!("{}.json", stem));
            let ir_path = out_dir
                .join("c2rs.meta")
                .join("ir")
                .join(format!("{}.json", stem));
            let c_path = project_root.join(&c_rel);

            // AST
            if let Err(e) = extract::run_clang_ast_dump(&c_path, &include_strs, &ast_path) {
                file.stages.failed_stage = Some(Stage::AstGenerated);
                file.stages.error = Some(e.to_string());
                bump(&mut failed_totals.lock().unwrap(), "ast_generated", 1);
                results.lock().unwrap().push(file);
                continue;
            }
            file.stages.ast_generated = true;

            // IR
            let ast_json = match std::fs::read_to_string(&ast_path) {
                Ok(s) => s,
                Err(e) => {
                    file.stages.failed_stage = Some(Stage::LoweredToIr);
                    file.stages.error = Some(e.to_string());
                    bump(&mut failed_totals.lock().unwrap(), "lowered_to_ir", 1);
                    results.lock().unwrap().push(file);
                    continue;
                }
            };
            let module = match extract::ast_json_to_module(&ast_json, &c_rel) {
                Ok(m) => m,
                Err(e) => {
                    file.stages.failed_stage = Some(Stage::LoweredToIr);
                    file.stages.error = Some(e.to_string());
                    bump(&mut failed_totals.lock().unwrap(), "lowered_to_ir", 1);
                    results.lock().unwrap().push(file);
                    continue;
                }
            };
            file.stages.lowered_to_ir = true;
            let ir_unsupported = collect_ir_unsupported(&module);

            if let Ok(json) = serde_json::to_string_pretty(&module) {
                let _ = std::fs::write(&ir_path, json);
            }

            // Rust gen
            let module = match crate::passes::run_safe_passes_v1(module, &out_dir.join("c2rs.meta"))
            {
                Ok(m) => m,
                Err(e) => {
                    file.stages.failed_stage = Some(Stage::RustGenerated);
                    file.stages.error = Some(e.to_string());
                    bump(&mut failed_totals.lock().unwrap(), "rust_generated", 1);
                    results.lock().unwrap().push(file);
                    continue;
                }
            };
            let is_main = rs_rel.ends_with("main.rs");
            let cg = codegen::module_ir_to_rust_with_main(&module, is_main);
            let cg_unsupported = collect_codegen_unsupported(&cg.unsupported_reasons);

            let mut combined = BTreeMap::new();
            for (k, v) in ir_unsupported {
                bump(&mut combined, k, v);
            }
            for (k, v) in cg_unsupported {
                bump(&mut combined, k, v);
            }
            file.unsupported.counts = combined.clone();
            {
                let mut total = unsupported_total.lock().unwrap();
                for (k, v) in combined {
                    bump(&mut total, k, v);
                }
            }

            let rs_path = out_dir.join(&rs_file);
            if let Some(parent) = rs_path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            if let Err(e) = std::fs::write(&rs_path, cg.rust) {
                file.stages.failed_stage = Some(Stage::RustGenerated);
                file.stages.error = Some(e.to_string());
                bump(&mut failed_totals.lock().unwrap(), "rust_generated", 1);
                results.lock().unwrap().push(file);
                continue;
            }
            file.stages.rust_generated = true;

            // Rust check (per file mode only; crate-level check happens after join).
            if per_file_check {
                match rustc_check_isolated(&rs_path) {
                    Ok((ok, stderr)) => {
                        file.stages.rust_checked = ok;
                        if !ok {
                            file.stages.failed_stage = Some(Stage::RustChecked);
                            file.stages.error = Some(stderr);
                            bump(&mut failed_totals.lock().unwrap(), "rust_checked", 1);
                        }
                    }
                    Err(e) => {
                        file.stages.rust_checked = false;
                        file.stages.failed_stage = Some(Stage::RustChecked);
                        file.stages.error = Some(e.to_string());
                        bump(&mut failed_totals.lock().unwrap(), "rust_checked", 1);
                    }
                }
            }

            results.lock().unwrap().push(file);
        }));
    }
    for t in threads {
        let _ = t.join();
    }

    let mut files = results.lock().unwrap().clone();
    files.sort_by(|a, b| a.c_file.cmp(&b.c_file));

    // Crate-level rust check for all files when not per-file.
    if !config.per_file_check {
        info!("validation: cargo check (crate-level)");
        let (ok, stderr) = cargo_check(&out_dir)?;
        if ok {
            for f in &mut files {
                if f.stages.rust_generated {
                    f.stages.rust_checked = true;
                }
            }
        } else {
            let bad = parse_rustc_stderr_files(&stderr);
            for f in &mut files {
                if f.stages.rust_generated {
                    let full = f.rs_file.as_str();
                    let stripped = full.strip_prefix("src/").unwrap_or(full);
                    if bad.iter().any(|p| p == full || p == stripped) {
                        f.stages.rust_checked = false;
                        f.stages.failed_stage = Some(Stage::RustChecked);
                        f.stages.error = Some(stderr.clone());
                    } else {
                        f.stages.rust_checked = true;
                    }
                }
            }
            bump(
                &mut failed_totals.lock().unwrap(),
                "rust_checked",
                bad.len() as u32,
            );
        }
    }

    let unsupported_counts = unsupported_total.lock().unwrap().clone();
    let report = ValidationReport {
        project_root: config.project_root.to_string_lossy().into_owned(),
        out_dir: out_dir.to_string_lossy().into_owned(),
        generated_at_unix: now_unix(),
        filter: config.filter.clone(),
        exclude: config.exclude.clone(),
        max_files: config.max_files,
        jobs,
        per_file_check: config.per_file_check,
        total_files_selected: scan.c_files.len(),
        totals_by_failed_stage: failed_totals.lock().unwrap().clone(),
        unsupported: UnsupportedSummary {
            top: top_n(&unsupported_counts, config.top_n),
            counts: unsupported_counts,
        },
        files,
    };

    write_report_files(&out_dir, &report)?;
    Ok(report)
}

pub fn render_report_only(out_dir: &Path) -> Result<(), Error> {
    let report = load_report(out_dir)?;
    let reports_dir = out_dir.join("c2rs.meta").join("reports");
    std::fs::create_dir_all(&reports_dir).map_err(|e| Error::Codegen(e.to_string()))?;
    let md = render_markdown(&report);
    write_text(&reports_dir.join("validation_report.md"), &md)?;
    Ok(())
}
