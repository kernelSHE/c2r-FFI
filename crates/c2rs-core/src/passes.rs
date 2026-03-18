//! IR-level "safe Rust" passes: normalize conditions to bool, array indexing.
//!
//! Each pass writes a report to meta/pass_reports/<name>_v1.json.

use crate::ir::{ExprIR, ModuleIR, StmtIR};
use serde::Serialize;
use std::path::Path;

/// Report for normalize_bool pass.
#[derive(Debug, Serialize)]
pub struct NormalizeBoolReport {
    pub conditions_normalized: u32,
}

/// Report for array_index pass.
#[derive(Debug, Serialize)]
pub struct ArrayIndexReport {
    pub array_subscript_count: u32,
    pub rewritten_to_checked: u32,
    pub array_declarations: u32,
}

fn is_comparison_expr(e: &ExprIR) -> bool {
    match e {
        ExprIR::Binary { op, .. } => crate::ir::is_comparison_op(op),
        _ => false,
    }
}

fn walk_stmt_normalize_bool(s: StmtIR, count: &mut u32) -> StmtIR {
    match s {
        StmtIR::If {
            cond,
            then_body,
            else_body,
        } => {
            let new_cond = if is_comparison_expr(&cond) {
                cond
            } else {
                *count += 1;
                ExprIR::ToBool(Box::new(cond))
            };
            StmtIR::If {
                cond: new_cond,
                then_body: then_body
                    .into_iter()
                    .map(|st| walk_stmt_normalize_bool(st, count))
                    .collect(),
                else_body: else_body.map(|eb| {
                    eb.into_iter()
                        .map(|st| walk_stmt_normalize_bool(st, count))
                        .collect()
                }),
            }
        }
        StmtIR::While { cond, body } => {
            let new_cond = if is_comparison_expr(&cond) {
                cond
            } else {
                *count += 1;
                ExprIR::ToBool(Box::new(cond))
            };
            StmtIR::While {
                cond: new_cond,
                body: body
                    .into_iter()
                    .map(|st| walk_stmt_normalize_bool(st, count))
                    .collect(),
            }
        }
        other => other,
    }
}

/// Pass: wrap non-comparison conditions in ToBool so codegen emits != 0.
pub fn pass_normalize_bool(module: ModuleIR) -> (ModuleIR, NormalizeBoolReport) {
    let mut count = 0u32;
    let functions = module
        .functions
        .into_iter()
        .map(|mut f| {
            f.body = f
                .body
                .into_iter()
                .map(|s| walk_stmt_normalize_bool(s, &mut count))
                .collect();
            f
        })
        .collect();
    let report = NormalizeBoolReport {
        conditions_normalized: count,
    };
    (
        ModuleIR {
            source_path: module.source_path,
            globals: module.globals,
            consts: module.consts,
            functions,
        },
        report,
    )
}

fn count_array_declarations(body: &[StmtIR]) -> u32 {
    let mut n = 0u32;
    for s in body {
        match s {
            StmtIR::VarDecl { ty, .. } => {
                if matches!(ty, crate::ir::TypeIR::Array(..)) {
                    n += 1;
                }
            }
            StmtIR::If {
                then_body,
                else_body,
                ..
            } => {
                n += count_array_declarations(then_body);
                if let Some(eb) = else_body {
                    n += count_array_declarations(eb);
                }
            }
            StmtIR::While { body: b, .. } => n += count_array_declarations(b),
            _ => {}
        }
    }
    n
}

fn walk_expr_subscript_count(e: &ExprIR, subscripts: &mut u32, checked: &mut u32) {
    match e {
        ExprIR::Subscript { base, index } => {
            *subscripts += 1;
            walk_expr_subscript_count(base, subscripts, checked);
            walk_expr_subscript_count(index, subscripts, checked);
        }
        ExprIR::CheckedSubscript { base, index } => {
            *checked += 1;
            walk_expr_subscript_count(base, subscripts, checked);
            walk_expr_subscript_count(index, subscripts, checked);
        }
        ExprIR::Binary { left, right, .. } => {
            walk_expr_subscript_count(left, subscripts, checked);
            walk_expr_subscript_count(right, subscripts, checked);
        }
        ExprIR::ToBool(inner) => walk_expr_subscript_count(inner, subscripts, checked),
        ExprIR::Call { args, .. } => {
            for a in args {
                walk_expr_subscript_count(a, subscripts, checked);
            }
        }
        _ => {}
    }
}

fn walk_stmt_subscript_count(body: &[StmtIR], subscripts: &mut u32, checked: &mut u32) {
    for s in body {
        match s {
            StmtIR::VarDecl { init: Some(e), .. } => {
                walk_expr_subscript_count(e, subscripts, checked)
            }
            StmtIR::Assign { value, .. } => walk_expr_subscript_count(value, subscripts, checked),
            StmtIR::If {
                cond,
                then_body,
                else_body,
            } => {
                walk_expr_subscript_count(cond, subscripts, checked);
                walk_stmt_subscript_count(then_body, subscripts, checked);
                if let Some(eb) = else_body {
                    walk_stmt_subscript_count(eb, subscripts, checked);
                }
            }
            StmtIR::While { cond, body } => {
                walk_expr_subscript_count(cond, subscripts, checked);
                walk_stmt_subscript_count(body, subscripts, checked);
            }
            StmtIR::Return(Some(e)) | StmtIR::Expr(e) => {
                walk_expr_subscript_count(e, subscripts, checked);
            }
            _ => {}
        }
    }
}

fn rewrite_subscript_to_checked(e: ExprIR, rewritten: &mut u32) -> ExprIR {
    match e {
        ExprIR::Subscript { base, index } => {
            *rewritten += 1;
            ExprIR::CheckedSubscript { base, index }
        }
        ExprIR::Binary { op, left, right } => ExprIR::Binary {
            op,
            left: Box::new(rewrite_subscript_to_checked(*left, rewritten)),
            right: Box::new(rewrite_subscript_to_checked(*right, rewritten)),
        },
        ExprIR::ToBool(inner) => {
            ExprIR::ToBool(Box::new(rewrite_subscript_to_checked(*inner, rewritten)))
        }
        ExprIR::Call { callee, args } => ExprIR::Call {
            callee,
            args: args
                .into_iter()
                .map(|a| rewrite_subscript_to_checked(a, rewritten))
                .collect(),
        },
        ExprIR::CheckedSubscript { base, index } => ExprIR::CheckedSubscript {
            base: Box::new(rewrite_subscript_to_checked(*base, rewritten)),
            index: Box::new(rewrite_subscript_to_checked(*index, rewritten)),
        },
        other => other,
    }
}

fn walk_stmt_rewrite_subscript(s: StmtIR, rewritten: &mut u32) -> StmtIR {
    match s {
        StmtIR::VarDecl { name, ty, init } => StmtIR::VarDecl {
            name,
            ty,
            init: init.map(|e| rewrite_subscript_to_checked(e, rewritten)),
        },
        StmtIR::Assign { target, value } => StmtIR::Assign {
            target,
            value: rewrite_subscript_to_checked(value, rewritten),
        },
        StmtIR::If {
            cond,
            then_body,
            else_body,
        } => StmtIR::If {
            cond: rewrite_subscript_to_checked(cond, rewritten),
            then_body: then_body
                .into_iter()
                .map(|st| walk_stmt_rewrite_subscript(st, rewritten))
                .collect(),
            else_body: else_body.map(|eb| {
                eb.into_iter()
                    .map(|st| walk_stmt_rewrite_subscript(st, rewritten))
                    .collect()
            }),
        },
        StmtIR::While { cond, body } => StmtIR::While {
            cond: rewrite_subscript_to_checked(cond, rewritten),
            body: body
                .into_iter()
                .map(|st| walk_stmt_rewrite_subscript(st, rewritten))
                .collect(),
        },
        StmtIR::Return(Some(e)) => StmtIR::Return(Some(rewrite_subscript_to_checked(e, rewritten))),
        StmtIR::Expr(e) => StmtIR::Expr(rewrite_subscript_to_checked(e, rewritten)),
        other => other,
    }
}

/// Pass: count array decls/subscripts and rewrite Subscript -> CheckedSubscript.
pub fn pass_array_index(module: ModuleIR) -> (ModuleIR, ArrayIndexReport) {
    let mut array_decls = 0u32;
    let mut subscripts = 0u32;
    let mut checked = 0u32;
    for f in &module.functions {
        array_decls += count_array_declarations(&f.body);
        walk_stmt_subscript_count(&f.body, &mut subscripts, &mut checked);
    }
    let mut rewritten = 0u32;
    let functions = module
        .functions
        .into_iter()
        .map(|mut f| {
            f.body = f
                .body
                .into_iter()
                .map(|s| walk_stmt_rewrite_subscript(s, &mut rewritten))
                .collect();
            f
        })
        .collect();
    let report = ArrayIndexReport {
        array_subscript_count: subscripts + checked,
        rewritten_to_checked: rewritten,
        array_declarations: array_decls,
    };
    (
        ModuleIR {
            source_path: module.source_path,
            globals: module.globals,
            consts: module.consts,
            functions,
        },
        report,
    )
}

fn write_report(report: &impl Serialize, meta_dir: &Path, name: &str) -> Result<(), crate::Error> {
    let reports_dir = meta_dir.join("pass_reports");
    std::fs::create_dir_all(&reports_dir).map_err(|e| crate::Error::Codegen(e.to_string()))?;
    let path = reports_dir.join(name);
    let json =
        serde_json::to_string_pretty(report).map_err(|e| crate::Error::Codegen(e.to_string()))?;
    std::fs::write(path, json).map_err(|e| crate::Error::Codegen(e.to_string()))?;
    Ok(())
}

/// Run all safe-Rust v1 passes and write reports. Returns the transformed module.
pub fn run_safe_passes_v1(module: ModuleIR, meta_dir: &Path) -> Result<ModuleIR, crate::Error> {
    let (module, r1) = pass_normalize_bool(module);
    write_report(&r1, meta_dir, "normalize_bool_v1.json")?;
    let (module, r2) = pass_array_index(module);
    write_report(&r2, meta_dir, "array_index_v1.json")?;
    Ok(module)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::{FunctionIR, TypeIR};

    #[test]
    fn normalize_bool_wraps_int_condition() {
        let module = ModuleIR {
            source_path: "x.c".into(),
            globals: Vec::new(),
            consts: Vec::new(),
            functions: vec![FunctionIR {
                name: "f".into(),
                params: vec![],
                return_type: TypeIR::Int,
                body: vec![StmtIR::If {
                    cond: ExprIR::Var("x".into()),
                    then_body: vec![StmtIR::Return(Some(ExprIR::Literal(
                        crate::ir::LiteralIR::Int(1),
                    )))],
                    else_body: None,
                }],
            }],
        };
        let (out, report) = pass_normalize_bool(module);
        assert_eq!(report.conditions_normalized, 1);
        let stmt = &out.functions[0].body[0];
        if let StmtIR::If { cond, .. } = stmt {
            assert!(matches!(cond, ExprIR::ToBool(_)));
        } else {
            panic!("expected If");
        }
    }

    #[test]
    fn normalize_bool_leaves_comparison_unchanged() {
        let module = ModuleIR {
            source_path: "x.c".into(),
            globals: Vec::new(),
            consts: Vec::new(),
            functions: vec![FunctionIR {
                name: "f".into(),
                params: vec![],
                return_type: TypeIR::Int,
                body: vec![StmtIR::If {
                    cond: ExprIR::Binary {
                        op: ">".into(),
                        left: Box::new(ExprIR::Var("n".into())),
                        right: Box::new(ExprIR::Literal(crate::ir::LiteralIR::Int(0))),
                    },
                    then_body: vec![],
                    else_body: None,
                }],
            }],
        };
        let (out, report) = pass_normalize_bool(module);
        assert_eq!(report.conditions_normalized, 0);
        let stmt = &out.functions[0].body[0];
        if let StmtIR::If { cond, .. } = stmt {
            assert!(matches!(cond, ExprIR::Binary { op, .. } if op == ">"));
        } else {
            panic!("expected If");
        }
    }

    #[test]
    fn array_index_rewrites_subscript_to_checked() {
        let module = ModuleIR {
            source_path: "a.c".into(),
            globals: Vec::new(),
            consts: Vec::new(),
            functions: vec![FunctionIR {
                name: "f".into(),
                params: vec![],
                return_type: TypeIR::Int,
                body: vec![
                    StmtIR::VarDecl {
                        name: "arr".into(),
                        ty: TypeIR::Array(Box::new(TypeIR::Int), Some(10)),
                        init: None,
                    },
                    StmtIR::Expr(ExprIR::Subscript {
                        base: Box::new(ExprIR::Var("arr".into())),
                        index: Box::new(ExprIR::Literal(crate::ir::LiteralIR::Int(0))),
                    }),
                ],
            }],
        };
        let (out, report) = pass_array_index(module);
        assert_eq!(report.array_subscript_count, 1);
        assert_eq!(report.rewritten_to_checked, 1);
        assert_eq!(report.array_declarations, 1);
        let stmt = &out.functions[0].body[1];
        if let StmtIR::Expr(ExprIR::CheckedSubscript { .. }) = stmt {
        } else {
            panic!("expected CheckedSubscript");
        }
    }
}
