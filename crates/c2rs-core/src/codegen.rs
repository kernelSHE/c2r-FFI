//! IR→Rust codegen: generate Rust source from ModuleIR only (never from C text).
//!
//! ## Type mapping (MVP, documented)
//! - C `int` → `i32` (platform: fixed 32-bit; use i32 for portability).
//! - C `float` → `f32`, C `double` → `f64`.
//! - C `void` → `()`.
//! - C `_Bool`/`bool` → `bool`.
//! - Pointers → `*mut T` (inner type mapped recursively).
//! - Unsupported/Named types → `()` with reason in mapping.json.
//!
//! Unsupported IR nodes emit `todo!("...")` and are listed in mapping.json `unsupported`.

use crate::ir::{ExprIR, LiteralIR, ModuleIR, StmtIR, TypeIR};

/// Result of codegen: Rust source + list of Unsupported reasons for mapping.json.
#[derive(Debug, Default)]
pub struct CodegenResult {
    pub rust: String,
    pub unsupported_reasons: Vec<String>,
}

fn rust_type(ty: &TypeIR, unsupported: &mut Vec<String>) -> String {
    match ty {
        TypeIR::Void => "()".into(),
        TypeIR::Int => "i32".into(),
        TypeIR::Bool => "bool".into(),
        TypeIR::Float => "f32".into(),
        TypeIR::Double => "f64".into(),
        TypeIR::Ptr(inner) => format!("*mut {}", rust_type(inner, unsupported)),
        TypeIR::Array(inner, Some(n)) => format!("[{}; {}]", rust_type(inner, unsupported), n),
        TypeIR::Array(inner, None) => {
            unsupported.push("type:array:incomplete".into());
            format!("[{}; 0]", rust_type(inner, unsupported))
        }
        TypeIR::Named(n) => {
            unsupported.push(format!("type:named:{}", n));
            // Use i32 as a placeholder so code type-checks; mapping still records unsupported.
            "i32".into()
        }
        TypeIR::Unsupported { kind, debug } => {
            unsupported.push(format!("type:{}:{}", kind, debug));
            // Fallback placeholder type.
            "i32".into()
        }
    }
}

fn slice_form_for_checked(
    e: &ExprIR,
    array_vars: &std::collections::HashSet<String>,
    unsupported: &mut Vec<String>,
) -> String {
    match e {
        ExprIR::Var(name) if array_vars.contains(name) => format!("&{}[..]", sanitize_ident(name)),
        _ => rust_expr(e, unsupported, array_vars),
    }
}

fn rust_expr(
    e: &ExprIR,
    unsupported: &mut Vec<String>,
    array_vars: &std::collections::HashSet<String>,
) -> String {
    match e {
        ExprIR::Literal(LiteralIR::Int(i)) => i.to_string(),
        ExprIR::Literal(LiteralIR::Bool(b)) => b.to_string(),
        ExprIR::Literal(LiteralIR::Float(s)) => format!("{}_f32", s),
        ExprIR::Literal(LiteralIR::Str(s)) => format!("{:?}", s),
        ExprIR::Var(name) => {
            // Many C projects use macro/enum constants (e.g. `XML_ROLE_TEXT_DECL`) that we
            // don't currently lower into IR. If we emit them as bare identifiers, Rust fails
            // with "cannot find value". Prefer a compiling placeholder.
            if looks_like_c_constant(name) {
                unsupported.push(format!("const:{}", name));
                "0i32".into()
            } else {
                sanitize_ident(name)
            }
        }
        ExprIR::Binary { op, left, right } => {
            let l = rust_expr(left, unsupported, array_vars);
            let r = rust_expr(right, unsupported, array_vars);
            let op_rust = match op.as_str() {
                "+" => "+",
                "-" => "-",
                "*" => "*",
                "/" => "/",
                "%" => "%",
                "==" => "==",
                "!=" => "!=",
                "<" => "<",
                ">" => ">",
                "<=" => "<=",
                ">=" => ">=",
                _ => {
                    unsupported.push(format!("expr:binary:{}", op));
                    "+"
                }
            };
            // Always parenthesize binary expressions to avoid precedence surprises and
            // Rust "comparison operators cannot be chained" parse errors when combining
            // comparisons with arithmetic/logical operators.
            format!("({} {} {})", l, op_rust, r)
        }
        ExprIR::Call { callee, args } => {
            let args_rust: Vec<_> = args
                .iter()
                .map(|a| rust_expr(a, unsupported, array_vars))
                .collect();
            format!("{}({})", sanitize_ident(callee), args_rust.join(", "))
        }
        ExprIR::ToBool(inner) => {
            // If inner is already a comparison (==, !=, <, >, <=, >=), use it directly;
            // otherwise compare with 0 to emulate C truthiness.
            match &**inner {
                ExprIR::Binary { op, .. } if crate::ir::is_comparison_op(op) => {
                    rust_expr(inner, unsupported, array_vars)
                }
                _ => format!("(({}) != 0)", rust_expr(inner, unsupported, array_vars)),
            }
        }
        ExprIR::Subscript { base, index } => format!(
            "{}[{}]",
            rust_expr(base, unsupported, array_vars),
            rust_expr(index, unsupported, array_vars)
        ),
        ExprIR::CheckedSubscript { base, index } => format!(
            "{}.get({}).unwrap()",
            slice_form_for_checked(base, array_vars, unsupported),
            rust_expr(index, unsupported, array_vars)
        ),
        ExprIR::Unsupported { kind, debug } => {
            unsupported.push(format!("expr:{}:{}", kind, debug));
            format!("todo!(\"expr {}: {}\")", kind, escape_quote(debug))
        }
    }
}

/// Collect names of variables that are assigned to in the body.
fn assigned_vars(body: &[StmtIR]) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    for s in body {
        match s {
            StmtIR::Assign { target, .. } => {
                out.insert(target.clone());
            }
            StmtIR::If {
                then_body,
                else_body,
                ..
            } => {
                out.extend(assigned_vars(then_body));
                if let Some(eb) = else_body {
                    out.extend(assigned_vars(eb));
                }
            }
            StmtIR::While { body: b, .. } => {
                out.extend(assigned_vars(b));
            }
            _ => {}
        }
    }
    out
}

/// Collect names of variables declared as array type (for CheckedSubscript slice form).
fn array_vars(body: &[StmtIR]) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    for s in body {
        match s {
            StmtIR::VarDecl { name, ty, .. } => {
                if matches!(ty, TypeIR::Array(..)) {
                    out.insert(name.clone());
                }
            }
            StmtIR::If {
                then_body,
                else_body,
                ..
            } => {
                out.extend(array_vars(then_body));
                if let Some(eb) = else_body {
                    out.extend(array_vars(eb));
                }
            }
            StmtIR::While { body: b, .. } => {
                out.extend(array_vars(b));
            }
            _ => {}
        }
    }
    out
}

fn rust_stmt(
    s: &StmtIR,
    unsupported: &mut Vec<String>,
    mutable_vars: &std::collections::HashSet<String>,
    array_vars: &std::collections::HashSet<String>,
) -> String {
    match s {
        StmtIR::VarDecl { name, ty, init } => {
            let ty_rust = rust_type(ty, unsupported);
            let name_rust = sanitize_ident(name);
            let mut_ = if mutable_vars.contains(name) {
                "mut "
            } else {
                ""
            };
            match init {
                Some(init_expr) => format!(
                    "    let {}{}: {} = {};\n",
                    mut_,
                    name_rust,
                    ty_rust,
                    rust_expr(init_expr, unsupported, array_vars)
                ),
                None => format!("    let {}{}: {};\n", mut_, name_rust, ty_rust),
            }
        }
        StmtIR::Assign { target, value } => {
            // If we failed to recover a real LHS name (target "?" becomes "x"),
            // don't emit a bogus assignment to an undeclared variable.
            if target.is_empty() || target == "?" {
                format!(
                    "    let _ = {};\n",
                    rust_expr(value, unsupported, array_vars)
                )
            } else {
                format!(
                    "    {} = {};\n",
                    sanitize_ident(target),
                    rust_expr(value, unsupported, array_vars)
                )
            }
        }
        StmtIR::If {
            cond,
            then_body,
            else_body,
        } => {
            let c = rust_expr(cond, unsupported, array_vars);
            let mut block = format!("    if {} {{\n", c);
            for st in then_body {
                block.push_str(&rust_stmt(st, unsupported, mutable_vars, array_vars));
            }
            block.push_str("    }");
            if let Some(else_b) = else_body {
                block.push_str(" else {\n");
                for st in else_b {
                    block.push_str(&rust_stmt(st, unsupported, mutable_vars, array_vars));
                }
                block.push_str("    }");
            }
            block.push('\n');
            block
        }
        StmtIR::While { cond, body } => {
            let c = rust_expr(cond, unsupported, array_vars);
            let mut block = format!("    while {} {{\n", c);
            for st in body {
                block.push_str(&rust_stmt(st, unsupported, mutable_vars, array_vars));
            }
            block.push_str("    }\n");
            block
        }
        StmtIR::Return(Some(e)) => {
            format!("    return {};\n", rust_expr(e, unsupported, array_vars))
        }
        StmtIR::Return(None) => "    return;\n".into(),
        StmtIR::Expr(e) => format!("    {};\n", rust_expr(e, unsupported, array_vars)),
        StmtIR::Unsupported { kind, debug } => {
            unsupported.push(format!("stmt:{}:{}", kind, debug));
            format!("    todo!(\"stmt {}: {}\");\n", kind, escape_quote(debug))
        }
    }
}

fn sanitize_ident(s: &str) -> String {
    if s.is_empty() || s == "?" {
        return "x".into();
    }
    if s.starts_with("__") || s == "crate" || s == "super" || s == "Self" {
        return format!("r#{}", s);
    }
    s.to_string()
}

fn looks_like_c_constant(name: &str) -> bool {
    let mut saw_upper = false;
    for ch in name.chars() {
        if ch.is_ascii_uppercase() {
            saw_upper = true;
            continue;
        }
        if ch.is_ascii_digit() || ch == '_' {
            continue;
        }
        return false;
    }
    saw_upper
}

fn escape_quote(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

fn default_expr_for_type(ty: &TypeIR, unsupported: &mut Vec<String>) -> String {
    match ty {
        TypeIR::Void => "()".into(),
        TypeIR::Int => "0i32".into(),
        TypeIR::Bool => "false".into(),
        TypeIR::Float => "0.0_f32".into(),
        TypeIR::Double => "0.0_f64".into(),
        TypeIR::Ptr(_) => "std::ptr::null_mut()".into(),
        TypeIR::Array(inner, Some(n)) => {
            let def = default_expr_for_type(inner, unsupported);
            format!("[{}; {}]", def, n)
        }
        TypeIR::Array(..) | TypeIR::Named(_) | TypeIR::Unsupported { .. } => {
            unsupported.push("default_expr:unsupported_type".into());
            "0i32".into()
        }
    }
}

/// Generate Rust source for one module (one .rs file). Only from IR.
/// When is_main_module is true, a function named "main" returning int is emitted as c_main() -> i32 plus fn main() { let _ = c_main(); }.
pub fn module_ir_to_rust_with_main(module: &ModuleIR, is_main_module: bool) -> CodegenResult {
    let mut unsupported_reasons: Vec<String> = Vec::new();
    let mut out = String::new();
    out.push_str("//! Generated from IR (source: ");
    out.push_str(&escape_quote(&module.source_path));
    out.push_str("). No FFI.\n\n");

    // File-scope constants.
    for c in &module.consts {
        let name = sanitize_ident(&c.name);
        let ty_rust = rust_type(&c.ty, &mut unsupported_reasons);
        out.push_str(&format!("pub const {}: {} = {};\n", name, ty_rust, c.value));
    }
    if !module.consts.is_empty() {
        out.push('\n');
    }

    // File-scope globals (v2: minimal support). We emit simple pub const/static
    // declarations so that references from functions compile, even if initializers
    // are often just default values.
    for g in &module.globals {
        let name = sanitize_ident(&g.name);
        let ty_rust = rust_type(&g.ty, &mut unsupported_reasons);
        let init = if let Some(init_expr) = &g.init {
            rust_expr(
                init_expr,
                &mut unsupported_reasons,
                &std::collections::HashSet::new(),
            )
        } else {
            default_expr_for_type(&g.ty, &mut unsupported_reasons)
        };
        if g.is_const {
            out.push_str(&format!("pub const {}: {} = {};\n", name, ty_rust, init));
        } else {
            out.push_str(&format!(
                "pub static mut {}: {} = {};\n",
                name, ty_rust, init
            ));
        }
    }
    if !module.globals.is_empty() {
        out.push('\n');
    }

    for func in &module.functions {
        let mutable_vars = assigned_vars(&func.body);
        let array_vars = array_vars(&func.body);
        let ret_ty = rust_type(&func.return_type, &mut unsupported_reasons);
        let params: Vec<String> = func
            .params
            .iter()
            .map(|p| {
                format!(
                    "{}: {}",
                    sanitize_ident(&p.name),
                    rust_type(&p.ty, &mut unsupported_reasons)
                )
            })
            .collect();
        let name = sanitize_ident(&func.name);
        let (pub_name, add_rust_main) =
            if is_main_module && func.name == "main" && matches!(func.return_type, TypeIR::Int) {
                ("c_main".to_string(), true)
            } else {
                (name.clone(), false)
            };
        out.push_str(&format!(
            "pub fn {}({}) -> {} {{\n",
            pub_name,
            params.join(", "),
            ret_ty
        ));
        if func.body.is_empty() {
            out.push_str(&format!(
                "    {}\n",
                default_expr_for_type(&func.return_type, &mut unsupported_reasons)
            ));
        } else {
            for st in &func.body {
                out.push_str(&rust_stmt(
                    st,
                    &mut unsupported_reasons,
                    &mutable_vars,
                    &array_vars,
                ));
            }
        }
        out.push_str("}\n\n");
        if add_rust_main {
            out.push_str("fn main() { let _ = c_main(); }\n");
        }
    }

    CodegenResult {
        rust: out,
        unsupported_reasons,
    }
}

/// Generate Rust source for one module. Entry point name unchanged.
pub fn module_ir_to_rust(module: &ModuleIR) -> CodegenResult {
    module_ir_to_rust_with_main(module, false)
}

/// Collect all function names from a module for mapping (c_name -> rs_name).
/// When is_main_module, C "main" is mapped to Rust "c_main".
pub fn function_mapping_from_module_with_main(
    module: &ModuleIR,
    is_main_module: bool,
) -> std::collections::HashMap<String, String> {
    module
        .functions
        .iter()
        .map(|f| {
            let rs_name = if is_main_module && f.name == "main" {
                "c_main".to_string()
            } else {
                sanitize_ident(&f.name)
            };
            (f.name.clone(), rs_name)
        })
        .collect()
}

/// Collect all function names from a module for mapping (c_name -> rs_name, MVP: same).
pub fn function_mapping_from_module(
    module: &ModuleIR,
) -> std::collections::HashMap<String, String> {
    function_mapping_from_module_with_main(module, false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{FunctionIR, ParamIR, TypeIR};

    #[test]
    fn codegen_simple_func() {
        let m = ModuleIR {
            source_path: "x.c".into(),
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
                body: vec![StmtIR::Return(Some(ExprIR::Binary {
                    op: "+".into(),
                    left: Box::new(ExprIR::Var("a".into())),
                    right: Box::new(ExprIR::Var("b".into())),
                }))],
            }],
        };
        let r = module_ir_to_rust(&m);
        assert!(r.rust.contains("pub fn add"));
        assert!(r.rust.contains("i32"));
        assert!(r.rust.contains("return (a + b)"));
        assert!(!r.rust.contains("extern \"C\""));
    }
}
