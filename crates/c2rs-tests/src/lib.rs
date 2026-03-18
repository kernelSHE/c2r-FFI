//! Integration tests and fixtures for C→IR→Rust.
//!
//! Structure: N .c => N .rs, mapping.json for symbol tracking.

#[cfg(test)]
use c2rs_core::{ir_to_rust, Function, TranslationUnit};
use std::path::Path;

/// Placeholder: ensure one .c yields one .rs path.
pub fn expected_rs_path(c_path: &Path) -> std::path::PathBuf {
    c_path.with_extension("rs")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn unsafe_budget() -> usize {
        std::env::var("C2RS_UNSAFE_BUDGET")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(0)
    }

    fn count_unsafe_in_dir(out_dir: &Path) -> usize {
        fn count_in_file(path: &std::path::Path) -> usize {
            let s = std::fs::read_to_string(path).unwrap_or_default();
            s.matches("unsafe").count()
        }

        let mut total = 0usize;
        let src_dir = out_dir.join("src");
        if src_dir.exists() {
            for entry in std::fs::read_dir(src_dir).unwrap() {
                let path = entry.unwrap().path();
                if path.extension().is_some_and(|e| e == "rs") {
                    total += count_in_file(&path);
                }
            }
        }
        total
    }

    fn assert_unsafe_within_budget(out_dir: &Path) {
        let budget = unsafe_budget();
        let n = count_unsafe_in_dir(out_dir);
        assert!(
            n <= budget,
            "unsafe budget exceeded: {} > {} (set C2RS_UNSAFE_BUDGET to override)",
            n,
            budget
        );
    }

    #[test]
    fn structure_one_c_one_rs() {
        let c = Path::new("foo/bar.c");
        let rs = expected_rs_path(c);
        assert_eq!(rs.file_name().unwrap(), "bar.rs");
    }

    #[test]
    fn ir_to_rust_no_extern_c() {
        let ir = TranslationUnit {
            source_path: "test.c".into(),
            functions: vec![Function {
                c_name: "foo".into(),
                rust_name: "foo".into(),
                params: vec![],
            }],
        };
        let rust_code = ir_to_rust(&ir).unwrap();
        assert!(!rust_code.contains("extern \"C\""));
    }

    #[test]
    fn mini1_converted_project_builds() {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mini1 = manifest_dir
            .join("../../examples/c/mini1")
            .canonicalize()
            .unwrap();
        if !mini1.exists() {
            eprintln!("skip: examples/c/mini1 not found");
            return;
        }
        let out = tempfile::tempdir().unwrap();
        let config = c2rs_core::Config {
            input: mini1.clone(),
            output: out.path().to_path_buf(),
            emit_ir: false,
            emit_map: true,
            fix: false,
            max_iter: None,
            dry_run: false,
            force: false,
            filter: vec![],
            exclude: vec![],
            max_files: None,
        };
        c2rs_core::run(&config, None).unwrap();
        assert!(out.path().join("Cargo.toml").exists());
        assert!(out.path().join("src/lib.rs").exists());
        let meta = out.path().join("c2rs.meta");
        assert!(meta.join("mapping.json").exists());
        assert!(meta.join("scan.json").exists());
        let mapping: c2rs_core::MappingJson =
            serde_json::from_str(&fs::read_to_string(meta.join("mapping.json")).unwrap()).unwrap();
        assert_eq!(mapping.files.get("main.c"), Some(&"main.rs".to_string()));
        assert_eq!(mapping.files.get("util.c"), Some(&"util.rs".to_string()));
        for entry in fs::read_dir(out.path().join("src")).unwrap() {
            let path = entry.unwrap().path();
            if path.extension().is_some_and(|e| e == "rs") {
                let content = fs::read_to_string(&path).unwrap();
                assert!(
                    !content.contains("extern \"C\""),
                    "{} must not contain extern \"C\"",
                    path.display()
                );
            }
        }
        let status = std::process::Command::new("cargo")
            .arg("build")
            .current_dir(out.path())
            .status()
            .unwrap();
        assert!(
            status.success(),
            "cargo build in converted project must succeed"
        );

        no_extern_c_in_dir(out.path());
        assert_unsafe_within_budget(out.path());

        let main_rs = fs::read_to_string(out.path().join("src/main.rs")).unwrap();
        assert!(main_rs.contains("main.c"), "main.rs must document source");
        let scan: c2rs_core::ProjectScan =
            serde_json::from_str(&fs::read_to_string(meta.join("scan.json")).unwrap()).unwrap();
        assert_eq!(scan.c_files.len(), 2);
        assert!(scan.h_files.iter().any(|h| h == "util.h"));
    }

    #[test]
    fn mini2_full_pipeline_emit_ir_meta_and_build() {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mini2 = manifest_dir
            .join("../../examples/c/mini2")
            .canonicalize()
            .unwrap();
        if !mini2.exists() {
            eprintln!("skip: examples/c/mini2 not found");
            return;
        }
        let out = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(out.path()).unwrap();
        let config = c2rs_core::Config {
            input: mini2.clone(),
            output: out.path().to_path_buf(),
            emit_ir: true,
            emit_map: true,
            fix: false,
            max_iter: None,
            dry_run: false,
            force: false,
            filter: vec![],
            exclude: vec![],
            max_files: None,
        };
        c2rs_core::run(&config, None).unwrap();

        let meta = out.path().join("c2rs.meta");
        assert!(
            meta.join("scan.json").exists(),
            "c2rs.meta/scan.json must exist"
        );
        assert!(
            meta.join("mapping.json").exists(),
            "c2rs.meta/mapping.json must exist"
        );
        assert!(meta.join("ast").exists(), "c2rs.meta/ast must exist");
        assert!(meta.join("ir").exists(), "c2rs.meta/ir must exist");
        assert!(
            meta.join("build_result.json").exists(),
            "c2rs.meta/build_result.json must exist"
        );

        let ir_dir = meta.join("ir");
        let ir_files: Vec<_> = fs::read_dir(&ir_dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        assert!(!ir_files.is_empty(), "at least one ir json file");
        for path in &ir_files {
            let json = fs::read_to_string(path).unwrap();
            let _module: c2rs_core::ir::ModuleIR = serde_json::from_str(&json)
                .unwrap_or_else(|e| panic!("{} must deserialize: {}", path.display(), e));
        }

        let build_result: c2rs_core::BuildResult =
            serde_json::from_str(&fs::read_to_string(meta.join("build_result.json")).unwrap())
                .unwrap();
        assert!(
            build_result.success,
            "cargo build must succeed: {}",
            build_result.stderr
        );

        no_extern_c_in_dir(out.path());
        assert_unsafe_within_budget(out.path());
    }

    #[test]
    fn mini3_full_pipeline_pass_reports_and_build() {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mini3 = manifest_dir
            .join("../../examples/c/mini3")
            .canonicalize()
            .unwrap();
        if !mini3.exists() {
            eprintln!("skip: examples/c/mini3 not found");
            return;
        }
        let out = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(out.path()).unwrap();
        let config = c2rs_core::Config {
            input: mini3.clone(),
            output: out.path().to_path_buf(),
            emit_ir: true,
            emit_map: true,
            fix: false,
            max_iter: None,
            dry_run: false,
            force: false,
            filter: vec![],
            exclude: vec![],
            max_files: None,
        };
        c2rs_core::run(&config, None).unwrap();

        let meta = out.path().join("c2rs.meta");
        assert!(
            meta.join("build_result.json").exists(),
            "build_result.json must exist"
        );
        let build_result: c2rs_core::BuildResult =
            serde_json::from_str(&fs::read_to_string(meta.join("build_result.json")).unwrap())
                .unwrap();
        assert!(
            build_result.success,
            "mini3 cargo build must succeed: {}",
            build_result.stderr
        );

        let reports = meta.join("pass_reports");
        assert!(
            reports.join("normalize_bool_v1.json").exists(),
            "pass_reports/normalize_bool_v1.json must exist"
        );
        assert!(
            reports.join("array_index_v1.json").exists(),
            "pass_reports/array_index_v1.json must exist"
        );
        no_extern_c_in_dir(out.path());
        assert_unsafe_within_budget(out.path());
    }

    #[test]
    fn validate_only_mini2_writes_reports() {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mini2 = manifest_dir
            .join("../../examples/c/mini2")
            .canonicalize()
            .unwrap();
        if !mini2.exists() {
            eprintln!("skip: examples/c/mini2 not found");
            return;
        }
        let out = tempfile::tempdir().unwrap();
        let cfg = c2rs_core::validate::ValidateConfig {
            project_root: mini2.clone(),
            out_dir: out.path().to_path_buf(),
            filter: vec![],
            exclude: vec![],
            max_files: None,
            per_file_check: false,
            jobs: 1,
            top_n: 10,
        };
        let report = c2rs_core::validate::validate_project(&cfg).unwrap();
        assert_eq!(report.total_files_selected, 1);
        assert_eq!(report.files.len(), 1);
        let f = &report.files[0];
        assert!(f.stages.scanned);
        assert!(f.stages.ast_generated);
        assert!(f.stages.lowered_to_ir);
        assert!(f.stages.rust_generated);
        assert!(f.stages.rust_checked);

        let reports = out.path().join("c2rs.meta").join("reports");
        assert!(
            reports.join("validation_report.json").exists(),
            "validation_report.json must exist"
        );
        assert!(
            reports.join("validation_report.md").exists(),
            "validation_report.md must exist"
        );

        no_extern_c_in_dir(out.path());
        assert_unsafe_within_budget(out.path());
    }

    #[test]
    fn validate_filter_exclude_max_files_applies() {
        let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mini1 = manifest_dir
            .join("../../examples/c/mini1")
            .canonicalize()
            .unwrap();
        if !mini1.exists() {
            eprintln!("skip: examples/c/mini1 not found");
            return;
        }
        let out = tempfile::tempdir().unwrap();
        let cfg = c2rs_core::validate::ValidateConfig {
            project_root: mini1.clone(),
            out_dir: out.path().to_path_buf(),
            filter: vec!["util.c".into()],
            exclude: vec!["main.c".into()],
            max_files: Some(1),
            per_file_check: false,
            jobs: 2,
            top_n: 10,
        };
        let report = c2rs_core::validate::validate_project(&cfg).unwrap();
        assert_eq!(report.total_files_selected, 1);
        assert_eq!(report.files.len(), 1);
        assert!(report.files[0].c_file.contains("util.c"));

        let reports = out.path().join("c2rs.meta").join("reports");
        assert!(reports.join("validation_report.json").exists());
        assert!(reports.join("validation_report.md").exists());
    }

    /// Agent fix loop with mock Ollama: broken crate + patch that fixes it → build succeeds and iter_0.patch exists.
    #[test]
    fn fix_loop_with_mock_ollama_repairs_broken_crate() {
        let out = tempfile::tempdir().unwrap();
        let out_dir = out.path();
        let src = out_dir.join("src");
        fs::create_dir_all(&src).unwrap();
        let meta = out_dir.join("c2rs.meta");
        fs::create_dir_all(&meta).unwrap();

        fs::write(
            out_dir.join("Cargo.toml"),
            r#"[package]
name = "broken"
version = "0.1.0"
edition = "2021"
"#,
        )
        .unwrap();
        fs::write(src.join("main.rs"), "fn main() { x(); }\n").unwrap();

        let build_result = c2rs_core::run_cargo_build(out_dir).unwrap();
        assert!(!build_result.success, "crate must fail to build");

        let patch = "--- a/src/main.rs\n+++ b/src/main.rs\n@@ -1,1 +1,1 @@\n-fn main() { x(); }\n+fn main() {}\n";
        let mock =
            c2rs_toolchain::ollama::MockOllamaProvider::new(format!("```diff\n{}\n```", patch));
        let provider = c2rs_agent::OllamaFixProvider::new(std::sync::Arc::new(mock));

        let final_result =
            c2rs_core::run_fix_loop(out_dir, &meta, build_result, &provider, 5).unwrap();
        assert!(
            final_result.success,
            "fix loop should succeed: {}",
            final_result.stderr
        );

        let iter0 = meta.join("patches").join("iter_0.patch");
        assert!(iter0.exists(), "patches/iter_0.patch must exist");
        let saved = fs::read_to_string(&iter0).unwrap();
        assert!(saved.contains("fn main() {}"));
    }

    /// Check that generated source under out_dir has no `extern "C"` (integration requirement).
    fn no_extern_c_in_dir(out_dir: &Path) {
        let src_dir = out_dir.join("src");
        let meta_dir = out_dir.join("c2rs.meta");
        for dir in [&src_dir, &meta_dir] {
            if !dir.exists() {
                continue;
            }
            let out = std::process::Command::new("grep")
                .args(["-r", "-l", "extern \"C\"", dir.to_string_lossy().as_ref()])
                .output()
                .unwrap();
            assert!(
                out.stdout.is_empty(),
                "generated sources must not contain extern \"C\"; found in: {}",
                String::from_utf8_lossy(&out.stdout)
            );
        }
    }
}
