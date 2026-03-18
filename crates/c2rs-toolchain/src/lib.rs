//! C→IR and IR→Rust deterministic toolchain.
//!
//! All Rust output is generated from IR only; no `extern "C"` in generated code.
//! C→IR extract lives in c2rs-core::extract; full pipeline in c2rs_core::run.

pub mod deepseek;
pub mod ollama;
pub use c2rs_core::extract;

use c2rs_core::{ir_to_rust, TranslationUnit};
use std::path::Path;
use tracing::info;

/// Run C→IR (stub: delegates to core).
pub fn c_to_ir_file(c_path: &Path) -> anyhow::Result<TranslationUnit> {
    let content = std::fs::read_to_string(c_path)?;
    let path_str = c_path.to_string_lossy();
    c2rs_core::c_to_ir(&content, &path_str).map_err(anyhow::Error::msg)
}

/// Serialize IR to JSON.
pub fn ir_to_json(ir: &TranslationUnit) -> anyhow::Result<String> {
    serde_json::to_string_pretty(ir).map_err(Into::into)
}

/// Deserialize IR from JSON.
pub fn ir_from_json(json: &str) -> anyhow::Result<TranslationUnit> {
    serde_json::from_str(json).map_err(Into::into)
}

/// Run IR→Rust and return Rust source (no extern "C").
pub fn ir_to_rust_file(ir: &TranslationUnit) -> anyhow::Result<String> {
    info!("Generating Rust from IR for {}", ir.source_path);
    ir_to_rust(ir).map_err(|e| anyhow::anyhow!("{}", e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use c2rs_core::Function;

    #[test]
    fn ir_roundtrip_json() {
        let ir = TranslationUnit {
            source_path: "a.c".into(),
            functions: vec![Function {
                c_name: "foo".into(),
                rust_name: "foo".into(),
                params: vec![],
            }],
        };
        let json = ir_to_json(&ir).unwrap();
        let back = ir_from_json(&json).unwrap();
        assert_eq!(back.source_path, ir.source_path);
        assert_eq!(back.functions.len(), 1);
    }
}
