# IR→Rust type mapping (MVP)

Rust is generated **only from IR**; no direct C text is used.

## Type mapping

| C type   | Rust type | Note |
|----------|-----------|------|
| `int`    | `i32`     | Fixed 32-bit for portability; platform strategy could use `c_int` later. |
| `float`  | `f32`     | |
| `double` | `f64`     | |
| `void`   | `()`      | |
| `_Bool` / `bool` | `bool` | |
| `T*`     | `*mut T`  | Inner type `T` mapped recursively. |
| Unsupported / named | `()` | Reason recorded in `mapping.json` `unsupported`. |

## mapping.json

- **files**: `c_file_rel` → `rs_file_rel`
- **functions**: `c_function_name` → `rs_function_name` (MVP: same name)
- **unsupported**: list of reasons for Unsupported nodes (e.g. `stmt:for:...`, `expr:...`)
