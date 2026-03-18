# C2RS 工具评估与路线图

## 一、工具是否满足「C→IR→Rust 转换工具」定位

### 1.1 结论：**满足核心定位，具备可扩展骨架**

| 维度 | 状态 | 说明 |
|------|------|------|
| **C→IR** | ✅ 已实现 | Clang AST JSON → ModuleIR（函数 + 部分语句/表达式 + 全局/常量 v2） |
| **IR→Rust** | ✅ 已实现 | 确定性 codegen，无 `extern "C"`，支持 main→c_main、括号/ToBool/下标 |
| **管线闭环** | ✅ 已实现 | scan → extract_ir → emit_rust → cargo build；可选 fix loop |
| **大项目验证** | ✅ 已实现 | --validate-only、阶段状态、Unsupported TopN、JSON/MD 报告 |
| **Agent 修复** | ✅ 已实现 | BuildFixProvider 抽象，Ollama 实现，patch 校验/应用/重试 |

当前限制（与「完善度」相关，见第四节）：

- IR 覆盖有限：Switch/For/ConditionalOperator/复杂类型等仍为 Unsupported → `todo!()`
- 宏常量未从预处理抽取（inName/min2 等依赖头文件宏）
- 多文件符号解析、跨 TU 类型未做

---

## 二、之前 10 项任务的满足情况

根据对话中实现的条目整理为下表（任务卡 8 = Agent 自动修复）。

| # | 任务概要 | 状态 | 备注 |
|---|----------|------|------|
| 1 | **c2rs-agent**：build 失败时调用 LLM 生成并应用补丁 | ✅ | Ollama（qwen2.5-coder:32b），unified diff，禁止 extern "C"，大小限制，重试与 patch 保存 |
| 2 | **IR 安全化 pass**：normalize_bool、array_index | ✅ | passes 报告写 meta/pass_reports/，有单测 |
| 3 | **自动化验收与 CI**：E2E、禁止 extern C、unsafe 预算、GitHub Actions | ✅ | mini1/2/3、extern C 检查、C2RS_UNSAFE_BUDGET、ci.yml |
| 4 | **大项目验证**：--validate-only/report-only、filter/exclude/max-files、per-file-check、jobs | ✅ | ValidationReport、阶段追踪、Unsupported 统计与 TopN |
| 5 | **libexpat core 能 cargo check**（优先 expat/lib/*.c） | 🟡 部分 | 模块树、ToBool/括号、ArraySubscript、去重定义、globals/consts 已做；仍缺宏常量、Switch 等，需继续迭代 |
| 6 | **多级目录模块树**：expat/lib/x.c → src/expat/lib/mod.rs + x.rs | ✅ | build_mod_tree 已支持嵌套目录 |
| 7 | **链式比较 / ToBool**：比较表达式不再加 != 0；Binary 括号 | ✅ | codegen 已改 |
| 8 | **Agent 自动修复（任务卡 8）**：可接 DeepSeek API | ✅ | 已接：`--fix-provider deepseek`，环境变量 `DEEPSEEK_API_KEY`，见第三节 |
| 9 | **跳过函数原型**：仅保留有 body 的 FunctionDecl | ✅ | extract 中 saw_body 判断 |
| 10 | **常量 IR 层**：ConstIR、Enum/VarDecl→consts、pub const codegen | ✅ | 宏常量未接，需 clang -E -dM 后续补 |

整体：**8 项完全满足，2 项部分满足（5 与 8）**；5 依赖更多 IR/宏支持，8 只需新增 DeepSeek 实现并接入 CLI。

---

## 三、任务卡 8：接入 DeepSeek API 完善 Agent

### 3.1 设计要点

- **保持 BuildFixProvider 抽象**：core 只依赖 `generate_patch(ctx) -> Result<String>`，不关心具体 LLM。
- **新增实现**：在 `c2rs-toolchain` 中增加「DeepSeek 兼容」的 HTTP 调用（OpenAI 风格 `POST /v1/chat/completions`），实现与现有 `OllamaProvider` 同级的 `DeepSeekProvider`（或通用 `OpenAICompatibleProvider`）。
- **CLI**：增加 `--fix-provider ollama|deepseek` 与 DeepSeek 专用参数（`--deepseek-url`、`--deepseek-model`、`DEEPSEEK_API_KEY`），在 `--fix` 时根据 provider 选择 Ollama 或 DeepSeek。

### 3.2 实现清单（建议）

1. **c2rs-toolchain**
   - 新增 `deepseek.rs`（或 `openai_compat.rs`）：
     - `DeepSeekConfig { base_url, model, api_key, timeout_secs }`
     - `DeepSeekProvider` 实现 `OllamaProvider` trait（或新 trait `LlmFixProvider: generate(prompt) -> Result<String>`），内部发 `POST .../chat/completions`，从 `choices[0].message.content` 取文本。
   - 若保留「只依赖 OllamaProvider」的 agent：可让 `DeepSeekProvider` 实现与 `HttpOllamaProvider` 相同的 trait（`OllamaProvider`），这样 agent 无需改代码，仅 CLI 构造不同 provider。

2. **c2rs-agent**
   - 若 trait 统一为「生成一段文本」：无需改；若 DeepSeek 用新 trait，则 `BuildFixProvider` 内部可接受 `Arc<dyn LlmFixProvider>` 或通过枚举分发。

3. **c2rs-cli**
   - 参数：`--fix-provider ollama|deepseek`，`--deepseek-url`，`--deepseek-model`，从 env `DEEPSEEK_API_KEY` 读 key。
   - 当 `--fix-provider deepseek` 时，构造 `DeepSeekProvider` 并传入 `OllamaFixProvider::new(Arc::new(deepseek_provider))`（若 DeepSeek 实现 `OllamaProvider`）。

4. **安全**
   - API Key 仅从环境变量 `DEEPSEEK_API_KEY` 读取，不写进配置文件或日志。

**已实现**：`c2rs-toolchain/src/deepseek.rs`（DeepSeekProvider 实现 OllamaProvider），CLI 支持 `--fix-provider ollama|deepseek`、`--deepseek-url`、`--deepseek-model`。使用示例：

```bash
export DEEPSEEK_API_KEY=your_key
c2rs /path/to/c_project -o /tmp/out --fix --fix-provider deepseek --max-iter 5
```

---

## 四、与现有 C→Rust 工具的差异及可完善点

### 4.1 与 c2rust（Immunant）等对比

| 维度 | c2rust | 本工具 (c2rs) |
|------|--------|----------------|
| **目标语义** | 功能等价、保留 unsafe、可交叉验证 C/Rust 执行 | 不生成 `extern "C"`，偏向「纯 Rust 库」、可读可维护 |
| **IR** | 自有 AST + CBOR，与 Clang 深度绑定 | 自研 ModuleIR（JSON），Clang 仅作 AST 源，IR 更薄、易扩展 |
| **覆盖度** | C99 为主，大量语法/类型有映射 | 子集：函数/部分控制流/部分类型/全局与常量 v2，其余 Unsupported |
| **大项目策略** | 单次全量转换 | 分阶段验证（filter/exclude、阶段状态、TopN），先可观测再扩 IR |
| **修复与 LLM** | 无内置 LLM | 内置 Agent 修复回路（Ollama/可接 DeepSeek），仅修不替生成 |
| **输出约束** | 无硬性「无 extern C」 | 硬约束：无 extern "C"、patch 校验、unsafe 预算可配 |

### 4.2 本工具需完善的功能（按优先级）

**P0（阻塞 libexpat 等 core 通过 check）**

- **宏/预处理常量**：`clang -E -dM` 解析，将 `#define NAME 123` 注入 `ConstIR`，消除 inName/min2/UTF8_cval* 等 E0425。
- **SwitchStmt → IR + codegen**：至少映射为 if-else 链或 `match` 雏形，减少 `todo!()`。
- **ConditionalOperator（三元）**：ExprIR 分支 + codegen 为 `if cond { a } else { b }`。

**P1（提升可用性与语义）**

- **ForStmt**：降为 While 或专用 IR，避免 Unsupported。
- **CompoundStmt 多语句**：当前多句被合为 Unsupported，可扩展 StmtIR 的 Block 或顺序列表。
- **跨文件/头文件**：类型与常量可见性（多 TU 或单 TU + 已包含头文件 的宏/枚举汇总）。

**P2（与生态和体验）**

- **DeepSeek API**：见第三节，完善任务卡 8。
- **更多类型**：结构体/union 的声明与成员访问占位，减少 `TypeIR::Named`/Unsupported 的随意映射。
- **Clang 编译选项**：`--clang-extra` 或 `-D` 传递，便于带宏的工程（如 xmlparse.c 的 XML_GE）。

**P3（长期）**

- **Refactoring 与安全化**：类似 c2rust 的 refactor 脚本，在 IR 或 Rust 侧做 unsafe→safe 的辅助。
- **测试与回归**：将 libexpat/musl 的「可编译子集」固化为回归用例，随 IR 扩展而扩大通过范围。

---

## 五、总结

- **定位**：工具已满足「C→IR→Rust」转换管线与「大项目分阶段验证」的设定；Agent 修复（任务卡 8）已实现 Ollama，接上 DeepSeek 即可双后端。
- **10 项任务**：8 项完全满足，2 项部分（libexpat 全绿依赖后续 IR/宏；Agent 需补 DeepSeek）。
- **与 c2rust 等**：差异在「无 extern C、薄 IR、验证驱动、内置 LLM 修复」；完善方向集中在宏常量、Switch/三元/For、多语句块与 DeepSeek 接入。
