//! LLM-based fix loop: repair only.
//!
//! When cargo build fails, builds a prompt (stderr + file snippets + mapping + IR),
//! calls Ollama via c2rs-toolchain, parses a unified diff, validates and applies it.

mod patch;
mod prompt;

pub use patch::parse_patch_from_response;
pub use prompt::{build_prompt, collect_error_locations, file_snippet};

use c2rs_core::{BuildFixContext, BuildFixProvider, Error};
use c2rs_toolchain::ollama::{OllamaError, OllamaProvider};
use std::sync::Arc;
use tracing::info;

/// Fix provider that calls Ollama to generate a patch.
pub struct OllamaFixProvider {
    ollama: Arc<dyn OllamaProvider>,
}

impl OllamaFixProvider {
    pub fn new(ollama: Arc<dyn OllamaProvider>) -> Self {
        Self { ollama }
    }
}

impl BuildFixProvider for OllamaFixProvider {
    fn generate_patch(&self, ctx: &BuildFixContext) -> Result<String, Error> {
        info!(
            "Building prompt for LLM fix (stderr len={})",
            ctx.build_stderr.len()
        );
        let prompt = prompt::build_prompt(ctx)?;
        let response = self
            .ollama
            .generate(&prompt)
            .map_err(|e: OllamaError| Error::Codegen(e.to_string()))?;
        parse_patch_from_response(&response).ok_or_else(|| {
            Error::Codegen("LLM response did not contain a valid unified diff".into())
        })
    }
}

/// Placeholder: request LLM to fix Rust source (e.g. fix compile error).
/// Kept for backward compatibility; prefer run() with BuildFixProvider.
pub fn fix_rust_with_llm(
    _ir: &c2rs_core::TranslationUnit,
    rust_source: &str,
    _hint: &str,
) -> String {
    info!("Fix loop placeholder: would invoke LLM for repair");
    rust_source.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use c2rs_core::BuildFixContext;
    use c2rs_toolchain::ollama::MockOllamaProvider;
    use std::path::PathBuf;

    #[test]
    fn fix_loop_preserves_input_when_stub() {
        let out = fix_rust_with_llm(
            &c2rs_core::TranslationUnit {
                source_path: "x.c".into(),
                functions: vec![],
            },
            "fn main() {}",
            "compile error",
        );
        assert_eq!(out, "fn main() {}");
    }

    #[test]
    fn provider_returns_parsed_patch_from_mock() {
        let patch = "--- a/src/foo.rs\n+++ b/src/foo.rs\n@@ -1,1 +1,1 @@\n-fn x() {}\n+fn x() {}\n";
        let mock = MockOllamaProvider::new(format!("```diff\n{}\n```", patch));
        let provider = OllamaFixProvider::new(Arc::new(mock));
        let ctx = BuildFixContext {
            build_stderr: "error[E0308]: mismatched types".into(),
            out_dir: PathBuf::from("/tmp"),
            meta_dir: PathBuf::from("/tmp/c2rs.meta"),
        };
        let out = provider.generate_patch(&ctx).unwrap();
        assert!(out.contains("--- a/src/foo.rs"));
    }
}
