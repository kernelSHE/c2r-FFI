# C2RS 架构说明

## 1. 管线（Pipeline）

**入口**：`c2rs_core::run(config)`，由 CLI 在创建好 `out_dir` 后调用。

**步骤**（顺序固定）：

1. **Scan**  
   扫描项目根目录，得到 `.c` / `.h` 列表与 include 候选；结果写 `out_dir/c2rs.meta/scan.json`。

2. **Skeleton crate**  
   生成独立可编 crate：`Cargo.toml`、`src/lib.rs`（mod 树）、`src/*.rs`（占位或来自 IR）。

3. **若 `--emit-ir`**  
   C→IR：对每个 `.c` 调 clang 生成 AST JSON → 写入 `c2rs.meta/ast/<stem>.json`；解析为 ModuleIR → 写入 `c2rs.meta/ir/<stem>.json`。  
   实现：`c2rs_core::extract::extract_ir_for_project`。

4. **若非 `--dry-run`**  
   IR→Rust 写回：若存在 `c2rs.meta/ir/<stem>.json`，则从 IR 生成对应 `src/*.rs`；否则保留占位。  
   `c2rs.meta/mapping.json` 含 files/functions/unsupported。

5. **若未开启 `--fix` 且非 `--dry-run`**  
   在 `out_dir` 执行 `cargo build`，结果写 `c2rs.meta/build_result.json`；失败则 `run()` 返回错误。

转换**严格**为 C → IR → Rust；Rust 仅从 IR 生成。Rust 输出中**禁止**出现 `extern "C"`（见 RULE.md）。

---

## 2. IR（中间表示）

- IR 为可序列化的数据结构（`serde`），格式为 JSON。
- 至少包含：
  - 翻译单元与源路径（用于结构保持）；
  - 函数列表（C 名、Rust 名、参数等），用于生成 Rust 与生成 `mapping.json`。
- 扩展时保持向后兼容或版本化。

---

## 3. Agent 修复回路（Fix Loop）

- **LLM 仅用于修复**：在 IR→Rust 生成完成后，若出现编译错误、风格问题等，可由 Agent（`c2rs-agent`）调用 LLM 做**局部修复**。
- **禁止**用 LLM 替代 IR→Rust 的确定性生成；即「生成」与「修复」职责分离，修复回路可审计。

---

## 4. 结构保持策略

- **文件级**：N 个 `.c` 文件 ⇒ N 个同名 `.rs` 文件，路径尽量一致（例如 `src/a.c` → `src/a.rs`）。
- **符号级**：函数一一对应，通过 `mapping.json` 记录 C 符号 ↔ Rust 符号，便于追踪与调试。

---

## 5. 各 Crate 职责（骨架）

| Crate           | 职责 |
|-----------------|------|
| `c2rs-core`     | IR 定义、C→IR/IR→Rust 核心逻辑（占位可在此） |
| `c2rs-toolchain`| 文件级 C→IR、IR 序列化/反序列化、IR→Rust 编排 |
| `c2rs-agent`    | LLM 修复回路（仅修复，不替代生成） |
| `c2rs-cli`      | 命令行入口，调用 toolchain 与可选的 agent |
| `c2rs-tests`    | 集成测试与 fixture（含「无 extern C」等约束测试） |

---

## 6. 大项目验证流程（Large Project Validation）

目标：面向 musl libc 这类**大型 C 项目**，提供“分阶段验证能力”，优先获得可观测性（coverage / 失败分布 / unsupported TopN），而不是立即追求一次性全量转换成功。

### 6.1 验证模式（CLI）

`c2rs` 增加验证相关参数（不影响默认转换流程）：

- `--validate-only`：运行验证管线并输出报告（不会因为某个文件失败而直接中断；失败会记录在报告里）
- `--report-only`：不重新跑验证，只从 `out_dir` 里已有的 JSON 报告重新生成 Markdown
- `--filter PATTERN`：仅包含路径含 PATTERN 的 `.c` 文件（可重复）
- `--exclude PATTERN`：排除路径含 PATTERN 的 `.c` 文件（可重复）
- `--max-files N`：限制参与验证的 `.c` 文件数量（在 filter/exclude 后生效）
- `--per-file-check`：对每个生成的 Rust 文件做隔离 `cargo check`（更慢，但错误归因更精确）
- `--jobs N`：并行处理的 worker 数

### 6.2 每文件阶段状态（Per-file Stages）

验证对每个 `.c` 文件记录阶段状态：

- `scanned`
- `ast_generated`（clang ast json 已生成）
- `lowered_to_ir`（AST 已降到 IR）
- `rust_generated`（IR 已生成 Rust 文件）
- `rust_checked`（Rust 通过 check；crate-level 或 per-file）
- `failed_stage`：若失败，记录失败发生在哪一阶段（不会跳过统计）

### 6.3 Unsupported 统计与 TopN

验证会从 IR 与 codegen 的 unsupported 信息中做分类计数，并生成 TopN 汇总，用于指导后续优先级：

- `ir:type:*` / `ir:stmt:*` / `ir:expr:*`
- `codegen:*:*`

### 6.4 报告输出位置

所有验证输出写入：

- `out_dir/c2rs.meta/reports/validation_report.json`
- `out_dir/c2rs.meta/reports/validation_report.md`
