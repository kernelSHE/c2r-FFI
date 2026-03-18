//! C→IR extractor: run clang AST dump, parse JSON, lower to ModuleIR.
//! Writes c2rs.meta/ast/<stem>.json and c2rs.meta/ir/<stem>.json.

use crate::ir::{ConstIR, ExprIR, FunctionIR, LiteralIR, ModuleIR, ParamIR, StmtIR, TypeIR};
use crate::{Error, ProjectScan};
use serde_json::Value;
use std::path::{Path, PathBuf};
use tracing::{info, warn};

/// Run clang -Xclang -ast-dump=json -fsyntax-only for one .c file; write JSON to ast_path.
pub fn run_clang_ast_dump(
    c_path: &Path,
    include_dirs: &[String],
    ast_path: &Path,
) -> Result<(), Error> {
    let mut cmd = std::process::Command::new("clang");
    cmd.args(["-Xclang", "-ast-dump=json", "-fsyntax-only"]);
    for inc in include_dirs {
        cmd.arg("-I").arg(inc);
    }
    cmd.arg(c_path);
    let out = cmd.output().map_err(|e| Error::Extract(e.to_string()))?;
    if !out.status.success() {
        return Err(Error::Extract(format!(
            "clang failed: {}",
            String::from_utf8_lossy(&out.stderr)
        )));
    }
    if let Some(parent) = ast_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| Error::Extract(e.to_string()))?;
    }
    std::fs::write(ast_path, &out.stdout).map_err(|e| Error::Extract(e.to_string()))?;
    Ok(())
}

/// Parse qualType string to TypeIR (MVP: int/bool/float/double/void + pointers/arrays + named).
fn qual_type_to_type_ir(qual: &str) -> TypeIR {
    let mut s = qual.trim();

    // Drop leading qualifiers.
    loop {
        if let Some(rest) = s.strip_prefix("const ") {
            s = rest.trim_start();
        } else if let Some(rest) = s.strip_prefix("volatile ") {
            s = rest.trim_start();
        } else {
            break;
        }
    }

    // Strip pointer stars at the end.
    let mut ptr_depth = 0usize;
    while let Some(rest) = s.strip_suffix('*') {
        ptr_depth += 1;
        s = rest.trim_end();
    }

    // Handle C-style arrays like "T [N]" or "T [ ]".
    let mut array_len: Option<u32> = None;
    if let Some(idx) = s.find('[') {
        let (base, rest) = s.split_at(idx);
        let inside = rest
            .trim()
            .trim_start_matches('[')
            .trim_end_matches(']')
            .trim();
        if let Ok(n) = inside.parse::<u32>() {
            array_len = Some(n);
        }
        s = base.trim();
    }

    let base = if s == "int"
        || s == "unsigned"
        || s.starts_with("unsigned int")
        || s == "long"
        || s == "long long"
        || s == "short"
        || s == "unsigned long"
        || s == "unsigned long long"
        || s == "unsigned short"
    {
        TypeIR::Int
    } else if s == "_Bool" || s == "bool" {
        TypeIR::Bool
    } else if s == "float" {
        TypeIR::Float
    } else if s == "double" {
        TypeIR::Double
    } else if s == "void" {
        TypeIR::Void
    } else {
        // For typedefs / struct names etc, keep the name for mapping; codegen will choose a
        // placeholder Rust type but we still record unsupported:type:named.
        TypeIR::Named(s.to_string())
    };

    let mut ty = base;
    if let Some(n) = array_len {
        ty = TypeIR::Array(Box::new(ty), Some(n));
    }
    for _ in 0..ptr_depth {
        ty = TypeIR::Ptr(Box::new(ty));
    }
    ty
}

/// Parse function return type from "int (int, int)" -> TypeIR::Int.
fn parse_function_return_type(qual: &str) -> TypeIR {
    if let Some(paren) = qual.find('(') {
        return qual_type_to_type_ir(qual[..paren].trim());
    }
    qual_type_to_type_ir(qual)
}

fn node_kind(n: &Value) -> Option<&str> {
    n.get("kind").and_then(|k| k.as_str())
}

fn node_inner(n: &Value) -> Option<&Vec<Value>> {
    n.get("inner").and_then(|i| i.as_array())
}

fn node_name(n: &Value) -> Option<String> {
    n.get("name").and_then(|v| v.as_str()).map(String::from)
}

fn node_qual_type(n: &Value) -> Option<&str> {
    n.get("type")
        .and_then(|t| t.get("qualType"))
        .and_then(|q| q.as_str())
}

fn node_opcode(n: &Value) -> Option<&str> {
    n.get("opcode").and_then(|o| o.as_str())
}

/// Debug string for a node (for Unsupported).
fn node_debug(n: &Value) -> String {
    let k = node_kind(n).unwrap_or("?");
    let name = node_name(n)
        .map(|s| format!(" name={}", s))
        .unwrap_or_default();
    let op = n
        .get("opcode")
        .and_then(|o| o.as_str())
        .map(|o| format!(" op={}", o))
        .unwrap_or_default();
    format!("{}{}{}", k, name, op)
}

fn expr_from_ast(n: &Value) -> ExprIR {
    match node_kind(n) {
        Some("IntegerLiteral") => {
            let val = n
                .get("value")
                .and_then(|v| v.as_str())
                .and_then(|s| s.parse::<i64>().ok())
                .unwrap_or(0);
            ExprIR::Literal(LiteralIR::Int(val))
        }
        Some("FloatingLiteral") => {
            let val = n
                .get("value")
                .and_then(|v| v.as_str())
                .map(String::from)
                .unwrap_or_else(|| "0.0".into());
            ExprIR::Literal(LiteralIR::Float(val))
        }
        Some("DeclRefExpr") => {
            let name = n
                .get("referencedDecl")
                .and_then(|r| r.get("name"))
                .and_then(|v| v.as_str())
                .map(String::from)
                .unwrap_or_else(|| "?".into());
            ExprIR::Var(name)
        }
        Some("BinaryOperator") => {
            let op = node_opcode(n).unwrap_or("?").to_string();
            let empty: Vec<Value> = vec![];
            let inner = node_inner(n).unwrap_or(&empty);
            if inner.len() >= 2 {
                let left = Box::new(expr_from_ast(&inner[0]));
                let right = Box::new(expr_from_ast(&inner[1]));
                ExprIR::Binary { op, left, right }
            } else {
                ExprIR::Unsupported {
                    kind: "BinaryOperator".into(),
                    debug: node_debug(n),
                }
            }
        }
        Some("CallExpr") => {
            let empty: Vec<Value> = vec![];
            let inner = node_inner(n).unwrap_or(&empty);
            let (callee, args) = if inner.is_empty() {
                ("?".into(), vec![])
            } else {
                let callee_expr = expr_from_ast(&inner[0]);
                let callee_name = match &callee_expr {
                    ExprIR::Var(s) => s.clone(),
                    _ => "?".into(),
                };
                let args = inner[1..].iter().map(expr_from_ast).collect();
                (callee_name, args)
            };
            ExprIR::Call { callee, args }
        }
        Some("ArraySubscriptExpr") => {
            let empty: Vec<Value> = vec![];
            let inner = node_inner(n).unwrap_or(&empty);
            if inner.len() >= 2 {
                let base = Box::new(expr_from_ast(&inner[0]));
                let index = Box::new(expr_from_ast(&inner[1]));
                ExprIR::Subscript { base, index }
            } else {
                ExprIR::Unsupported {
                    kind: "ArraySubscriptExpr".into(),
                    debug: node_debug(n),
                }
            }
        }
        Some("ImplicitCastExpr")
        | Some("ParenExpr")
        | Some("CStyleCastExpr")
        | Some("UnaryOperator")
        | Some("MemberExpr") => {
            if let Some(inner) = node_inner(n).and_then(|i| i.first()) {
                return expr_from_ast(inner);
            }
            ExprIR::Unsupported {
                kind: node_kind(n).unwrap_or("?").into(),
                debug: node_debug(n),
            }
        }
        other => {
            warn!("Unsupported expr: {}", other.unwrap_or("?"));
            ExprIR::Unsupported {
                kind: other.unwrap_or("unknown").into(),
                debug: node_debug(n),
            }
        }
    }
}

fn stmt_from_ast(n: &Value) -> StmtIR {
    match node_kind(n) {
        Some("DeclStmt") => {
            let empty: Vec<Value> = vec![];
            let inner = node_inner(n).unwrap_or(&empty);
            if let Some(var) = inner.iter().find(|c| node_kind(c) == Some("VarDecl")) {
                let name = node_name(var).unwrap_or_else(|| "?".into());
                let ty = node_qual_type(var)
                    .map(qual_type_to_type_ir)
                    .unwrap_or(TypeIR::Int);
                let init = node_inner(var).and_then(|a| a.first()).map(expr_from_ast);
                return StmtIR::VarDecl { name, ty, init };
            }
            StmtIR::Unsupported {
                kind: "DeclStmt".into(),
                debug: node_debug(n),
            }
        }
        Some("BinaryOperator") if node_opcode(n) == Some("=") => {
            let empty: Vec<Value> = vec![];
            let inner = node_inner(n).unwrap_or(&empty);
            if inner.len() >= 2 {
                let lhs = &inner[0];
                let target = lhs
                    .get("referencedDecl")
                    .and_then(|r| r.get("name"))
                    .and_then(|v| v.as_str())
                    .map(String::from)
                    .unwrap_or_else(|| "?".into());
                let value = expr_from_ast(&inner[1]);
                return StmtIR::Assign { target, value };
            }
            StmtIR::Unsupported {
                kind: "Assign".into(),
                debug: node_debug(n),
            }
        }
        Some("ReturnStmt") => {
            let val = node_inner(n).and_then(|i| i.first()).map(expr_from_ast);
            StmtIR::Return(val)
        }
        Some("CompoundStmt") => {
            let empty: Vec<Value> = vec![];
            let body: Vec<StmtIR> = node_inner(n)
                .unwrap_or(&empty)
                .iter()
                .map(stmt_from_ast)
                .collect();
            if body.is_empty() {
                StmtIR::Unsupported {
                    kind: "CompoundStmt".into(),
                    debug: "empty".into(),
                }
            } else if body.len() == 1 {
                body.into_iter().next().unwrap()
            } else {
                StmtIR::Unsupported {
                    kind: "CompoundStmt".into(),
                    debug: format!("block with {} stmts (no Block in IR)", body.len()),
                }
            }
        }
        Some("IfStmt") => {
            let empty: Vec<Value> = vec![];
            let inner = node_inner(n).unwrap_or(&empty);
            let (cond, then_body, else_body) = if inner.len() >= 2 {
                let cond = expr_from_ast(&inner[0]);
                let then_stmt = stmt_from_ast(&inner[1]);
                let empty_inner: Vec<Value> = vec![];
                let then_body = if node_kind(&inner[1]) == Some("CompoundStmt") {
                    node_inner(&inner[1])
                        .unwrap_or(&empty_inner)
                        .iter()
                        .map(stmt_from_ast)
                        .collect()
                } else {
                    vec![then_stmt]
                };
                let else_body = inner.get(2).map(|e| {
                    if node_kind(e) == Some("CompoundStmt") {
                        node_inner(e)
                            .unwrap_or(&empty_inner)
                            .iter()
                            .map(stmt_from_ast)
                            .collect()
                    } else {
                        vec![stmt_from_ast(e)]
                    }
                });
                (cond, then_body, else_body)
            } else {
                return StmtIR::Unsupported {
                    kind: "IfStmt".into(),
                    debug: node_debug(n),
                };
            };
            StmtIR::If {
                cond,
                then_body,
                else_body,
            }
        }
        Some("WhileStmt") => {
            let empty: Vec<Value> = vec![];
            let inner = node_inner(n).unwrap_or(&empty);
            if inner.len() >= 2 {
                let cond = expr_from_ast(&inner[0]);
                let empty_inner: Vec<Value> = vec![];
                let body = if node_kind(&inner[1]) == Some("CompoundStmt") {
                    node_inner(&inner[1])
                        .unwrap_or(&empty_inner)
                        .iter()
                        .map(stmt_from_ast)
                        .collect()
                } else {
                    vec![stmt_from_ast(&inner[1])]
                };
                return StmtIR::While { cond, body };
            }
            StmtIR::Unsupported {
                kind: "WhileStmt".into(),
                debug: node_debug(n),
            }
        }
        Some("NullStmt") => StmtIR::Unsupported {
            kind: "NullStmt".into(),
            debug: "empty statement".into(),
        },
        other => {
            if let Some(inner) = node_inner(n) {
                if inner.len() == 1
                    && node_kind(&inner[0]).map(|k| k.starts_with("Expr")) == Some(true)
                {
                    return StmtIR::Expr(expr_from_ast(&inner[0]));
                }
            }
            warn!("Unsupported stmt: {}", other.unwrap_or("?"));
            StmtIR::Unsupported {
                kind: other.unwrap_or("unknown").into(),
                debug: node_debug(n),
            }
        }
    }
}

fn function_from_ast(n: &Value, _source_path: &str) -> Option<FunctionIR> {
    if node_kind(n) != Some("FunctionDecl") {
        return None;
    }
    if n.get("isImplicit")
        .and_then(|b| b.as_bool())
        .unwrap_or(false)
    {
        return None;
    }
    let name = node_name(n).unwrap_or_else(|| "?".into());
    let qual = node_qual_type(n).unwrap_or("void");
    let return_type = parse_function_return_type(qual);
    let empty: Vec<Value> = vec![];
    let mut params = Vec::new();
    let mut body_stmts = Vec::new();
    let mut saw_body = false;
    for c in node_inner(n).unwrap_or(&empty) {
        match node_kind(c) {
            Some("ParmVarDecl") => {
                let pname = node_name(c).unwrap_or_else(|| "?".into());
                let pty = node_qual_type(c)
                    .map(qual_type_to_type_ir)
                    .unwrap_or(TypeIR::Int);
                params.push(ParamIR {
                    name: pname,
                    ty: pty,
                });
            }
            Some("CompoundStmt") => {
                saw_body = true;
                let empty_inner: Vec<Value> = vec![];
                body_stmts = node_inner(c)
                    .unwrap_or(&empty_inner)
                    .iter()
                    .map(stmt_from_ast)
                    .collect();
            }
            _ => {}
        }
    }
    // Skip pure prototypes (declarations without a body). We only want true definitions.
    if !saw_body {
        return None;
    }
    Some(FunctionIR {
        name,
        params,
        return_type,
        body: body_stmts,
    })
}

fn global_from_ast(n: &Value) -> Option<crate::ir::GlobalVarIR> {
    if node_kind(n) != Some("VarDecl") {
        return None;
    }
    // Only handle file-scope (top-level) vars; we don't currently inspect storageClass deeply.
    let name = node_name(n).unwrap_or_else(|| "?".into());
    if name.is_empty() || name == "?" {
        return None;
    }
    let qual = node_qual_type(n).unwrap_or("int");
    let ty = qual_type_to_type_ir(qual);
    let mut init_expr = None;
    if let Some(inner) = node_inner(n) {
        if let Some(first) = inner.first() {
            // Many globals are simple literal/array initializers; best-effort lowering here.
            init_expr = Some(expr_from_ast(first));
        }
    }
    let is_const = qual.contains("const");
    let is_static = qual.contains("static");
    Some(crate::ir::GlobalVarIR {
        name,
        ty,
        init: init_expr,
        is_const,
        is_static,
    })
}

fn const_from_vardecl(n: &Value) -> Option<ConstIR> {
    if node_kind(n) != Some("VarDecl") {
        return None;
    }
    let name = node_name(n).unwrap_or_else(|| "?".into());
    if name.is_empty() || name == "?" {
        return None;
    }
    let qual = node_qual_type(n).unwrap_or("int");
    if !qual.contains("const") {
        return None;
    }
    // Only handle simple integer literals for now.
    let mut value: Option<i64> = None;
    if let Some(inner) = node_inner(n) {
        if let Some(first) = inner.first() {
            if node_kind(first) == Some("IntegerLiteral") {
                if let Some(s) = first.get("value").and_then(|v| v.as_str()) {
                    if let Ok(parsed) = s.parse::<i64>() {
                        value = Some(parsed);
                    }
                }
            }
        }
    }
    let val = value?;
    Some(ConstIR {
        name,
        ty: TypeIR::Int,
        value: val,
    })
}

fn enum_consts_from_ast(n: &Value) -> Vec<ConstIR> {
    let mut out = Vec::new();
    if node_kind(n) != Some("EnumDecl") {
        return out;
    }
    let empty: Vec<Value> = vec![];
    let inner = node_inner(n).unwrap_or(&empty);
    for c in inner {
        if node_kind(c) == Some("EnumConstantDecl") {
            let name = node_name(c).unwrap_or_else(|| "?".into());
            if name.is_empty() || name == "?" {
                continue;
            }
            // Try to parse an explicit integer value; otherwise fall back to 0.
            let mut val: Option<i64> = None;
            if let Some(enum_inner) = node_inner(c) {
                if let Some(first) = enum_inner.first() {
                    if node_kind(first) == Some("IntegerLiteral") {
                        if let Some(s) = first.get("value").and_then(|v| v.as_str()) {
                            if let Ok(parsed) = s.parse::<i64>() {
                                val = Some(parsed);
                            }
                        }
                    }
                }
            }
            out.push(ConstIR {
                name,
                ty: TypeIR::Int,
                value: val.unwrap_or(0),
            });
        }
    }
    out
}

/// Parse clang AST JSON string into ModuleIR.
pub fn ast_json_to_module(ast_json: &str, source_path: &str) -> Result<ModuleIR, Error> {
    let root: Value = serde_json::from_str(ast_json).map_err(|e| Error::Extract(e.to_string()))?;
    let mut functions = Vec::new();
    let mut globals = Vec::new();
    let mut consts = Vec::new();
    let empty: Vec<Value> = vec![];
    let inner = root
        .get("inner")
        .and_then(|i| i.as_array())
        .unwrap_or(&empty);
    for node in inner {
        match node_kind(node) {
            Some("FunctionDecl") => {
                if let Some(f) = function_from_ast(node, source_path) {
                    functions.push(f);
                }
            }
            Some("VarDecl") => {
                // Prefer treating simple const ints as constants.
                if let Some(c) = const_from_vardecl(node) {
                    consts.push(c);
                } else if let Some(g) = global_from_ast(node) {
                    globals.push(g);
                }
            }
            Some("EnumDecl") => {
                consts.extend(enum_consts_from_ast(node));
            }
            _ => {}
        }
    }
    Ok(ModuleIR {
        source_path: source_path.to_string(),
        globals,
        consts,
        functions,
    })
}

/// For each .c in scan: run clang, write ast to out_dir/c2rs.meta/ast/<stem>.json,
/// lower to ModuleIR, write to out_dir/c2rs.meta/ir/<stem>.json.
pub fn extract_ir_for_project(
    project_root: &Path,
    out_dir: &Path,
    scan: &ProjectScan,
) -> Result<(), Error> {
    let meta = out_dir.join("c2rs.meta");
    let ast_dir = meta.join("ast");
    let ir_dir = meta.join("ir");
    std::fs::create_dir_all(&ast_dir).map_err(|e| Error::Extract(e.to_string()))?;
    std::fs::create_dir_all(&ir_dir).map_err(|e| Error::Extract(e.to_string()))?;

    let include_dirs: Vec<PathBuf> = std::iter::once(project_root.to_path_buf())
        .chain(scan.include_dirs.iter().map(|s| project_root.join(s)))
        .collect();
    let include_strs: Vec<String> = include_dirs
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();

    for c_rel in &scan.c_files {
        let c_path = project_root.join(c_rel);
        if !c_path.exists() {
            return Err(Error::Extract(format!(
                "c file not found: {}",
                c_path.display()
            )));
        }
        let stem = Path::new(c_rel)
            .with_extension("")
            .to_string_lossy()
            .into_owned();
        let ast_path = ast_dir.join(format!("{}.json", stem.replace('/', "_")));
        let ir_path = ir_dir.join(format!("{}.json", stem.replace('/', "_")));

        info!("extract IR: {} -> {}", c_rel, ir_path.display());
        run_clang_ast_dump(&c_path, &include_strs, &ast_path)?;
        let ast_content =
            std::fs::read_to_string(&ast_path).map_err(|e| Error::Extract(e.to_string()))?;
        let module = ast_json_to_module(&ast_content, c_rel)?;
        let ir_json =
            serde_json::to_string_pretty(&module).map_err(|e| Error::Extract(e.to_string()))?;
        std::fs::write(&ir_path, ir_json).map_err(|e| Error::Extract(e.to_string()))?;
    }
    Ok(())
}
