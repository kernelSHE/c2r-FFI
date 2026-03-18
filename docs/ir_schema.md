# IR JSON Schema (first version)

IR is produced per **.c file** (one `ModuleIR` per file). This document describes the JSON shape used by `c2rs_core::ir`. No strict JSON Schema; structure is defined by Rust types and serde.

---

## Top level: ModuleIR

One JSON object per translation unit (.c file).

| Field | Type | Description |
|-------|------|-------------|
| `source_path` | string | Original .c path (relative to project root). |
| `functions` | array of FunctionIR | All functions in this file. |

**Example:**

```json
{
  "source_path": "util.c",
  "functions": [ ... ]
}
```

---

## FunctionIR

| Field | Type | Description |
|-------|------|-------------|
| `name` | string | Function name. |
| `params` | array of ParamIR | Parameter list. |
| `return_type` | TypeIR | Return type. |
| `body` | array of StmtIR | Function body (statement list). |

---

## ParamIR

| Field | Type | Description |
|-------|------|-------------|
| `name` | string | Parameter name. |
| `ty` | TypeIR | Parameter type. |

---

## TypeIR (tagged enum)

Serialized as an object with a **`type`** key (tag) and, for non-unit variants, a **`payload`** key.

| Tag | Payload | Description |
|-----|---------|-------------|
| `Void` | (none) | void |
| `Int` | (none) | int |
| `Bool` | (none) | bool |
| `Float` | (none) | float |
| `Double` | (none) | double |
| `Ptr` | TypeIR (object) | pointer to type |
| `Named` | string | Named type (e.g. size_t). |
| `Unsupported` | `{ "kind": string, "debug": string }` | Unhandled type; original info in debug. |

**Examples:**

```json
{ "type": "Int" }
{ "type": "Ptr", "payload": { "type": "Int" } }
{ "type": "Named", "payload": "size_t" }
{ "type": "Unsupported", "payload": { "kind": "complex_type", "debug": "struct Foo * const" } }
```

---

## StmtIR (tagged enum)

Object with **`stmt`** (tag) and **`payload`** (variant content).

| Tag | Payload | Description |
|-----|---------|-------------|
| `VarDecl` | `{ "name": string, "ty": TypeIR, "init": ExprIR \| null }` | Variable declaration, optional initializer. |
| `Assign` | `{ "target": string, "value": ExprIR }` | Assignment to a variable. |
| `If` | `{ "cond": ExprIR, "then_body": StmtIR[], "else_body": StmtIR[] \| null }` | if/else. |
| `While` | `{ "cond": ExprIR, "body": StmtIR[] }` | while loop. |
| `Return` | ExprIR or null | return value. |
| `Expr` | ExprIR | Expression statement. |
| `Unsupported` | `{ "kind": string, "debug": string }` | Unhandled statement (e.g. for, switch). |

**Examples:**

```json
{ "stmt": "VarDecl", "payload": { "name": "x", "ty": { "type": "Int" }, "init": null } }
{ "stmt": "Assign", "payload": { "target": "sum", "value": { "expr": "Binary", "payload": { ... } } } }
{ "stmt": "Return", "payload": { "expr": "Var", "payload": "sum" } }
{ "stmt": "Unsupported", "payload": { "kind": "for", "debug": "for (int i=0; i<n; i++) { }" } }
```

---

## ExprIR (tagged enum)

Object with **`expr`** (tag) and **`payload`** (variant content).

| Tag | Payload | Description |
|-----|---------|-------------|
| `Literal` | LiteralIR | int/bool/string literal. |
| `Var` | string | Variable reference. |
| `Binary` | `{ "op": string, "left": ExprIR, "right": ExprIR }` | Binary operator (e.g. +, -, ==). |
| `Call` | `{ "callee": string, "args": ExprIR[] }` | Function call. |
| `Unsupported` | `{ "kind": string, "debug": string }` | Unhandled expression. |

---

## LiteralIR (tagged enum)

Object with **`literal`** (tag) and **`value`** (variant content).

| Tag | Value | Example |
|-----|--------|--------|
| `Int` | number | `{ "literal": "Int", "value": 42 }` |
| `Bool` | boolean | `{ "literal": "Bool", "value": true }` |
| `Str` | string | `{ "literal": "Str", "value": "hello" }` |

---

## Full example

See **`examples/ir/sample_module.json`**: one module `util.c` with a single function `add(a, b)` that declares `sum`, assigns `a + b`, and returns `sum`.

---

## Unsupported nodes

Any construct not yet modeled (e.g. for-loops, switch, complex types) is represented as:

- **TypeIR::Unsupported**: `{ "type": "Unsupported", "payload": { "kind": string, "debug": string } }`
- **StmtIR::Unsupported**: `{ "stmt": "Unsupported", "payload": { "kind": string, "debug": string } }`
- **ExprIR::Unsupported**: `{ "expr": "Unsupported", "payload": { "kind": string, "debug": string } }`

`kind` identifies the construct (e.g. `"for"`, `"switch"`); `debug` holds a human-readable or source snippet for later handling.
