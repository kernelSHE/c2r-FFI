//! IR (Intermediate Representation) for C→Rust pipeline.
//!
//! Serde-serializable; no extraction logic in this module, only data structures.

use serde::{Deserialize, Serialize};

/// One .c file as IR (file-scope globals + consts + functions).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModuleIR {
    /// Original .c path (relative to project root).
    pub source_path: String,
    /// File-scope globals (v2: minimal subset used by libexpat).
    #[serde(default)]
    pub globals: Vec<GlobalVarIR>,
    /// File-scope constants (enums, const ints, simple macros, etc.).
    #[serde(default)]
    pub consts: Vec<ConstIR>,
    pub functions: Vec<FunctionIR>,
}

/// File-scope global variable (v2: minimal subset).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GlobalVarIR {
    pub name: String,
    pub ty: TypeIR,
    /// Initializer expression, if we can extract it (MVP: often None, codegen uses defaults).
    pub init: Option<ExprIR>,
    pub is_const: bool,
    pub is_static: bool,
}

/// File-scope constant (compile-time value).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConstIR {
    pub name: String,
    pub ty: TypeIR,
    pub value: i64,
}

/// Function: signature + body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FunctionIR {
    pub name: String,
    pub params: Vec<ParamIR>,
    pub return_type: TypeIR,
    pub body: Vec<StmtIR>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParamIR {
    pub name: String,
    pub ty: TypeIR,
}

/// Type (MVP: int/bool/float/double minimal set + array for safe-rust passes).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "payload")]
pub enum TypeIR {
    Void,
    Int,
    Bool,
    Float,
    Double,
    Ptr(Box<TypeIR>),
    /// Fixed-size array: (element_type, length). None = incomplete type.
    Array(Box<TypeIR>, Option<u32>),
    Named(String),
    Unsupported {
        kind: String,
        debug: String,
    },
}

/// Statement (MVP: decl, assign, if, while, return, expr).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "stmt", content = "payload")]
pub enum StmtIR {
    VarDecl {
        name: String,
        ty: TypeIR,
        init: Option<ExprIR>,
    },
    Assign {
        target: String,
        value: ExprIR,
    },
    If {
        cond: ExprIR,
        then_body: Vec<StmtIR>,
        else_body: Option<Vec<StmtIR>>,
    },
    While {
        cond: ExprIR,
        body: Vec<StmtIR>,
    },
    Return(Option<ExprIR>),
    Expr(ExprIR),
    Unsupported {
        kind: String,
        debug: String,
    },
}

/// Comparison operators (result is conceptually bool in Rust).
const CMP_OPS: &[&str] = &["==", "!=", "<", ">", "<=", ">="];

/// Expression (MVP: literal, var, binary, call, subscript, ToBool for conditions).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "expr", content = "payload")]
pub enum ExprIR {
    Literal(LiteralIR),
    Var(String),
    Binary {
        op: String,
        left: Box<ExprIR>,
        right: Box<ExprIR>,
    },
    Call {
        callee: String,
        args: Vec<ExprIR>,
    },
    /// C-style condition: treat as bool (emit as != 0). Used after normalize_bool pass.
    ToBool(Box<ExprIR>),
    /// Array/slice subscript: base[index]. Unchecked.
    Subscript {
        base: Box<ExprIR>,
        index: Box<ExprIR>,
    },
    /// Bounds-visible index: emit as slice.get(i).unwrap().
    CheckedSubscript {
        base: Box<ExprIR>,
        index: Box<ExprIR>,
    },
    Unsupported {
        kind: String,
        debug: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "literal", content = "value")]
pub enum LiteralIR {
    Int(i64),
    Bool(bool),
    /// Float as string to preserve Eq (e.g. "1.5").
    Float(String),
    Str(String),
}

/// True if binary op is a comparison (result is bool).
pub fn is_comparison_op(op: &str) -> bool {
    CMP_OPS.contains(&op)
}

/// Roundtrip: IR → JSON → IR preserves structure.
pub fn ir_roundtrip(module: &ModuleIR) -> Result<ModuleIR, serde_json::Error> {
    let json = serde_json::to_string(module)?;
    serde_json::from_str(&json)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_module() -> ModuleIR {
        ModuleIR {
            source_path: "util.c".into(),
            globals: Vec::new(),
            consts: Vec::new(),
            functions: vec![FunctionIR {
                name: "add".into(),
                params: vec![
                    ParamIR {
                        name: "a".into(),
                        ty: TypeIR::Int,
                    },
                    ParamIR {
                        name: "b".into(),
                        ty: TypeIR::Int,
                    },
                ],
                return_type: TypeIR::Int,
                body: vec![
                    StmtIR::VarDecl {
                        name: "sum".into(),
                        ty: TypeIR::Int,
                        init: None,
                    },
                    StmtIR::Assign {
                        target: "sum".into(),
                        value: ExprIR::Binary {
                            op: "+".into(),
                            left: Box::new(ExprIR::Var("a".into())),
                            right: Box::new(ExprIR::Var("b".into())),
                        },
                    },
                    StmtIR::Return(Some(ExprIR::Var("sum".into()))),
                ],
            }],
        }
    }

    #[test]
    fn ir_roundtrip_preserves_structure() {
        let m = sample_module();
        let m2 = ir_roundtrip(&m).unwrap();
        assert_eq!(m, m2);
    }

    #[test]
    fn sample_module_json_deserializes_and_roundtrips() {
        let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let sample_path = manifest_dir.join("../../examples/ir/sample_module.json");
        let json = std::fs::read_to_string(&sample_path).unwrap();
        let m: ModuleIR = serde_json::from_str(&json).unwrap();
        assert_eq!(m.source_path, "util.c");
        assert_eq!(m.functions.len(), 1);
        assert_eq!(m.functions[0].name, "add");
        let m2 = ir_roundtrip(&m).unwrap();
        assert_eq!(m, m2);
    }

    #[test]
    fn ir_roundtrip_unsupported_nodes() {
        let m = ModuleIR {
            source_path: "x.c".into(),
            globals: Vec::new(),
            consts: Vec::new(),
            functions: vec![FunctionIR {
                name: "f".into(),
                params: vec![],
                return_type: TypeIR::Unsupported {
                    kind: "complex_type".into(),
                    debug: "struct Foo * const".into(),
                },
                body: vec![StmtIR::Unsupported {
                    kind: "for".into(),
                    debug: "for (int i=0; i<n; i++) { }".into(),
                }],
            }],
        };
        let m2 = ir_roundtrip(&m).unwrap();
        assert_eq!(m, m2);
    }
}
