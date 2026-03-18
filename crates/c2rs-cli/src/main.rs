//! CLI for C→IR→Rust conversion.
//!
//! Pipeline: C → IR (JSON) → Rust. No `extern "C"` in output.
//! MVP: args, logging, output dir creation; no conversion logic.

use anyhow::{Context, Result};
use c2rs_agent::OllamaFixProvider;
use c2rs_core::validate::{validate_project, ValidateConfig};
use c2rs_core::{run, BuildFixProvider, Config};
use c2rs_toolchain::deepseek::{DeepSeekConfig, DeepSeekProvider, DEFAULT_DEEPSEEK_BASE_URL, DEFAULT_DEEPSEEK_MODEL};
use c2rs_toolchain::ollama::{HttpOllamaProvider, OllamaConfig, OllamaProvider};
use clap::Parser;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::info;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(name = "c2rs", about = "C→IR→Rust conversion (no extern C)")]
struct Args {
    /// Input C project (file or directory)
    #[arg(value_name = "input_c_project")]
    input: PathBuf,

    /// Output directory for .rs and optional IR/mapping
    #[arg(short = 'o', long = "output", value_name = "out_dir", required = true)]
    output: PathBuf,

    /// Validate large C project in phases; always emit reports under out_dir/c2rs.meta/reports/.
    #[arg(long)]
    validate_only: bool,

    /// Only (re)render Markdown from existing JSON report in out_dir.
    #[arg(long)]
    report_only: bool,

    /// Include only files whose path contains PATTERN (can be repeated).
    #[arg(long, value_name = "PATTERN", action = clap::ArgAction::Append)]
    filter: Vec<String>,

    /// Exclude files whose path contains PATTERN (can be repeated).
    #[arg(long, value_name = "PATTERN", action = clap::ArgAction::Append)]
    exclude: Vec<String>,

    /// Limit number of .c files processed after filter/exclude.
    #[arg(long, value_name = "N")]
    max_files: Option<usize>,

    /// Check each generated Rust file in isolation (slower; better attribution).
    #[arg(long)]
    per_file_check: bool,

    /// Parallel jobs for per-file validation (default: 1).
    #[arg(long, value_name = "N", default_value_t = 1)]
    jobs: usize,

    /// Emit IR as JSON files
    #[arg(long)]
    emit_ir: bool,

    /// Emit mapping.json (C ↔ Rust symbol)
    #[arg(long)]
    emit_map: bool,

    /// Enable LLM fix loop (repair only)
    #[arg(long)]
    fix: bool,

    /// Max fix-loop iterations (when --fix is set)
    #[arg(long, value_name = "N")]
    max_iter: Option<u32>,

    /// Ollama model for fix loop (default: qwen2.5-coder:32b)
    #[arg(long, value_name = "MODEL")]
    ollama_model: Option<String>,

    /// Ollama API base URL (default: http://localhost:11434)
    #[arg(long, value_name = "URL")]
    ollama_url: Option<String>,

    /// LLM fix provider: ollama (local) or deepseek (API). Default when --fix: ollama.
    #[arg(long, value_name = "PROVIDER", default_value = "ollama")]
    fix_provider: String,

    /// DeepSeek API base URL (when --fix-provider=deepseek). Default: https://api.deepseek.com
    #[arg(long, value_name = "URL")]
    deepseek_url: Option<String>,

    /// DeepSeek model (when --fix-provider=deepseek). Default: deepseek-chat
    #[arg(long, value_name = "MODEL")]
    deepseek_model: Option<String>,

    /// Dry run: no writes, only log
    #[arg(long)]
    dry_run: bool,

    /// Allow using existing output directory (default: error if out_dir exists)
    #[arg(long)]
    force: bool,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env().add_directive("info".parse()?))
        .init();

    let args = Args::parse();

    if args.report_only {
        if !args.output.exists() {
            anyhow::bail!(
                "--report-only requires existing --output dir: {}",
                args.output.display()
            );
        }
        c2rs_core::validate::render_report_only(&args.output)
            .map_err(|e| anyhow::anyhow!("{}", e))?;
        return Ok(());
    }

    if args.validate_only {
        if !args.output.exists() {
            std::fs::create_dir_all(&args.output)
                .with_context(|| format!("create_dir_all {}", args.output.display()))?;
        }
        let config = ValidateConfig {
            project_root: args.input.clone(),
            out_dir: args.output.clone(),
            filter: args.filter.clone(),
            exclude: args.exclude.clone(),
            max_files: args.max_files,
            per_file_check: args.per_file_check,
            jobs: args.jobs,
            top_n: 20,
        };
        let report = validate_project(&config).map_err(|e| anyhow::anyhow!("{}", e))?;
        info!(
            "validation report written under {}/c2rs.meta/reports (files={})",
            args.output.display(),
            report.total_files_selected
        );
        return Ok(());
    }

    let config = Config {
        input: args.input.clone(),
        output: args.output.clone(),
        emit_ir: args.emit_ir,
        emit_map: args.emit_map,
        fix: args.fix,
        max_iter: args.max_iter,
        dry_run: args.dry_run,
        force: args.force,
        filter: args.filter.clone(),
        exclude: args.exclude.clone(),
        max_files: args.max_files,
    };

    info!("c2rs config: input={} output={} emit_ir={} emit_map={} fix={} max_iter={:?} dry_run={} force={}",
        config.input.display(),
        config.output.display(),
        config.emit_ir,
        config.emit_map,
        config.fix,
        config.max_iter,
        config.dry_run,
        config.force,
    );

    if !config.dry_run {
        ensure_output_dir(&config).context("output directory")?;
    } else {
        info!("dry-run: skipping output directory creation");
    }

    let fix_provider: Option<OllamaFixProvider> = if config.fix {
        let provider: Arc<dyn OllamaProvider> = if args.fix_provider.eq_ignore_ascii_case("deepseek") {
            let api_key = std::env::var("DEEPSEEK_API_KEY").unwrap_or_else(|_| String::new());
            if api_key.is_empty() {
                anyhow::bail!("DEEPSEEK_API_KEY is required when --fix-provider=deepseek");
            }
            Arc::new(DeepSeekProvider::new(DeepSeekConfig::new(
                args.deepseek_url
                    .clone()
                    .unwrap_or_else(|| DEFAULT_DEEPSEEK_BASE_URL.to_string()),
                args.deepseek_model
                    .clone()
                    .unwrap_or_else(|| DEFAULT_DEEPSEEK_MODEL.to_string()),
                api_key,
            )))
        } else {
            let mut ollama_config = OllamaConfig::default();
            if let Some(m) = &args.ollama_model {
                ollama_config.model = m.clone();
            }
            if let Some(u) = &args.ollama_url {
                ollama_config.base_url = u.clone();
            }
            Arc::new(HttpOllamaProvider::new(ollama_config))
        };
        Some(OllamaFixProvider::new(provider))
    } else {
        None
    };
    run(
        &config,
        fix_provider.as_ref().map(|p| p as &dyn BuildFixProvider),
    )
    .map_err(|e| anyhow::anyhow!("{}", e))?;
    Ok(())
}

/// Create output directory; error if it already exists unless --force.
fn ensure_output_dir(config: &Config) -> Result<()> {
    let out = &config.output;
    if out.exists() {
        if !out.is_dir() {
            anyhow::bail!(
                "output path exists and is not a directory: {}",
                out.display()
            );
        }
        if !config.force {
            anyhow::bail!(
                "output directory already exists: {} (use --force to allow)",
                out.display()
            );
        }
        info!("using existing output directory: {}", out.display());
        return Ok(());
    }
    std::fs::create_dir_all(out).with_context(|| format!("create_dir_all {}", out.display()))?;
    info!("created output directory: {}", out.display());
    Ok(())
}
