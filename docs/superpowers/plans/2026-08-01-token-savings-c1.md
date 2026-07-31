# Extreme Token Savings C1 — 多块编辑 · 区间读 · 阈值默认 · schema 瘦身 · Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 攻掉支柱③前三大 token 浪费点——`edit_file` 升级多块 search/replace（唯一匹配、全有或全无）+ `read_file` 区间读，让大文件编辑从「读写整文件」降到「定位+片段+多块」；压缩阈值默认从 budget 推导让无损 L1 默认生效；内置工具 schema 文案压缩 -40% 并加防膨胀回归测试。

**Architecture:** 方案 A 延续——零新 crate。`deepseeknova-tools/src/fs.rs` 就地升级读写工具（snippet 验证机制不变：区间读仍按**整文件**注册 snippet，只对模型返回片段）；`deepseeknova-runtime` 装配处推导压缩阈值；`deepseeknova-tools` 全量 schema 文案压缩 + 新增总字符数回归测试。

**Tech Stack:** Rust（serde/async-trait/tokio），既有 crate：tools/runtime/config。

**基线：** worktree `.worktrees/feat-token-c1` @ `84652a1`（=main）。`make check` 基线绿（先跑一次确认）。

---

## File Map

| 文件 | 动作 | 职责 |
|---|---|---|
| `crates/deepseeknova-tools/src/fs.rs` | Modify | `read_file` 区间读；`edit_file` 多块 + 唯一匹配 |
| `crates/deepseeknova-tools/src/lib.rs` | Modify | schema 总字符数回归测试 |
| `crates/deepseeknova-tools/src/{grep,shell,memory,web_fetch,todo}.rs` + graph 工具 | Modify | schema 文案压缩 |
| `crates/deepseeknova-runtime/src/lib.rs` | Modify | 压缩阈值从 budget 推导（build_agent + coordinator 两处） |
| `crates/deepseeknova-config/src/lib.rs` | Modify | `compaction_threshold_tokens` 文档注释补推导说明 |
| `CHANGELOG.md` | Modify | 多块/唯一匹配 breaking 标注 + 阈值默认说明 |

**关键既有事实（已核，勿再猜）**：
- `edit_file` 现用 `original.find(&search)`（首个匹配）→ 改唯一匹配；参数 `EditFileArgs { path, search, replace, snippet_id: Option<String> }`，`snippet_id` 必填（`ok_or_else` 强制）。
- snippet 机制：`read_file` 读**整文件**（`fs::read_to_string`，1MB 上限）后 `tracker.register(path, &content)`；`edit_file` 读整文件后 `tracker.validate(snip_id, &original)`（整文件 hash 比对）。**区间读必须仍按整文件注册**，否则 edit 校验永远 STALE。
- `tracker.register(&mut self, path, content) -> String`、`validate(&self, id, current) -> Result<(),String>`。
- 阈值装配：coordinator 在 `runtime/lib.rs` ~L798 `runner.with_compaction_threshold(threshold)`；`Agent::with_compaction_threshold(Option<u32>)`（agent.rs L158）——实现者 grep 出 build_agent 内的 agent 侧装配点。
- `all_builtin_tools()` / `all_builtin_tools_with_sandbox(sandbox)` 在 `tools/src/lib.rs` L35/L41。

**钉死语义**：多块=全有或全无（任一块 0 命中或 ≥2 命中 → 整次失败，指明块号+命中数，零半改）；单块也改唯一匹配（微型 breaking）；区间读缺省=现行为；阈值 None+budget→max_total/2，显式优先，budget 关→None。

---

## Task 0: 基线确认

- [ ] **Step 1: 跑基线** — Run `cd /Users/ze/.gemini/antigravity/scratch/DPronix/.worktrees/feat-token-c1 && git branch --show-current`（须为 `feat/token-savings-c1`）+ `cargo test -p deepseeknova-tools 2>&1 | grep 'test result'`。Expected: 全绿。记录 fs.rs 相关既有测试数作对照。

---

## Task 1: `read_file` 区间读

**Files:** Modify `crates/deepseeknova-tools/src/fs.rs`

- [ ] **Step 1: 写失败测试** — 在 fs.rs 末尾 `#[cfg(test)] mod tests`（若无则新建）加：

```rust
    #[tokio::test]
    async fn read_file_ranged_returns_only_slice() {
        let dir = std::env::temp_dir().join(format!("dnv-c1-read-{}", std::process::id()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let f = dir.join("big.txt");
        let body: String = (1..=10).map(|i| format!("line{i}\n")).collect();
        tokio::fs::write(&f, &body).await.unwrap();

        let ctx = test_ctx(&dir); // 见 Step 2 说明：复用本模块既有测试上下文构造
        let tool = ReadFileTool;
        let args = format!(r#"{{"path":"big.txt","start_line":3,"end_line":5}}"#);
        let out = tool.execute(&ctx, &args).await.unwrap();
        // 只含 line3..line5，不含 line1/line2/line6
        assert!(out.contains("line3") && out.contains("line5"));
        assert!(!out.contains("line1") && !out.contains("line6"));
        // 仍带 snippet 标记
        assert!(out.contains("[SNIPPET ID:"));
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn read_file_full_still_default() {
        let dir = std::env::temp_dir().join(format!("dnv-c1-readfull-{}", std::process::id()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let f = dir.join("s.txt");
        tokio::fs::write(&f, "a\nb\nc\n").await.unwrap();
        let ctx = test_ctx(&dir);
        let out = ReadFileTool.execute(&ctx, r#"{"path":"s.txt"}"#).await.unwrap();
        assert!(out.contains('a') && out.contains('b') && out.contains('c'));
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
```

> **Step 2 说明（测试上下文）**：本模块若已有 ToolContext 构造 helper（grep `fn test_ctx\|ToolContext {` in fs.rs tests），复用之；若无，按 `deepseeknova_core::tool::ToolContext` 的实际字段构造一个指向 `dir` 的最小 ctx（workspace_root=dir、默认 security 全授权、未取消的 CancellationToken）。以既有其它 tools 测试的构造方式为准。

- [ ] **Step 2: 跑测试确认失败** — Run `cargo test -p deepseeknova-tools read_file_ranged 2>&1 | tail -3`。Expected: 编译错 `unknown field start_line` 或断言失败。

- [ ] **Step 3: 实现** — `ReadFileArgs` 加可选字段：

```rust
#[derive(Deserialize)]
struct ReadFileArgs {
    path: String,
    #[serde(default)]
    start_line: Option<usize>,
    #[serde(default)]
    end_line: Option<usize>,
}
```

`execute` 中读到 `content` 后（`let content = fs::read_to_string(&path).await?;` 之后、snippet 注册**之前**）插入区间切片。**snippet 仍按整文件 `content` 注册**（保证 edit 校验兼容）；只有返回给模型的正文换成区间：

```rust
        // 区间读（1-based 闭区间）：只裁剪返回给模型的正文，snippet 仍按整文件注册。
        let (display, range_note) = match (parsed.start_line, parsed.end_line) {
            (None, None) => (content.clone(), String::new()),
            (s, e) => {
                let lines: Vec<&str> = content.lines().collect();
                let total = lines.len();
                let start = s.unwrap_or(1).max(1);
                if start > total {
                    anyhow::bail!(
                        "start_line {start} exceeds file length ({total} lines)"
                    );
                }
                let end = e.unwrap_or(total).min(total); // end 超尾则截到尾（宽松）
                if end < start {
                    anyhow::bail!("end_line {end} is before start_line {start}");
                }
                let slice: String = lines[start - 1..end]
                    .iter()
                    .enumerate()
                    .map(|(i, l)| format!("{}: {}\n", start + i, l)) // 带行号前缀
                    .collect();
                (
                    slice,
                    format!("[Lines {start}-{end} of {total}]\n"),
                )
            }
        };

        let mut tracker = crate::snippet::global_tracker().lock().await;
        let snippet_id = tracker.register(&path.to_string_lossy(), &content);
        drop(tracker);

        Ok(format!(
            "{}{}\n\n[SNIPPET ID: {}]\n[Snippet generated from: {}]\n",
            range_note,
            display.trim_end(),
            snippet_id,
            path.display()
        ))
```

（删除原先的 `Ok(format!(...))` 整文件返回块，用上面替代。）

schema 加两个可选参数 + 省 token 引导，description 换为：

```rust
            description: "读取文件内容。大文件建议先用 grep/search_code 定位，再用 \
                 start_line/end_line 只读需要的区间，最后用 edit_file 多块替换——避免整文件进上下文。"
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "文件路径（绝对或相对）。" },
                    "start_line": { "type": "integer", "description": "可选。起始行（1-based，含）。省略=从头。" },
                    "end_line": { "type": "integer", "description": "可选。结束行（1-based，含）。省略=到尾。" }
                },
                "required": ["path"]
            }),
```

- [ ] **Step 4: 跑测试确认通过** — Run `cargo test -p deepseeknova-tools read_file 2>&1 | grep 'test result'`。Expected: 全过。

- [ ] **Step 5: fmt + clippy + 提交**

```bash
cargo fmt -p deepseeknova-tools
cargo clippy -p deepseeknova-tools --all-targets -- -D warnings
git add crates/deepseeknova-tools/src/fs.rs
git commit -m "feat(tools/fs): ranged read_file (start_line/end_line) with line-numbered slices"
```

---

## Task 2: `edit_file` 多块 + 唯一匹配

**Files:** Modify `crates/deepseeknova-tools/src/fs.rs`

- [ ] **Step 1: 写失败测试**（fs.rs tests 内追加）：

```rust
    async fn seed_edit(dir: &std::path::Path, body: &str) -> (String, ToolContext) {
        tokio::fs::write(dir.join("e.rs"), body).await.unwrap();
        let ctx = test_ctx(dir);
        // 先 read_file 拿 snippet_id（整文件）
        let out = ReadFileTool.execute(&ctx, r#"{"path":"e.rs"}"#).await.unwrap();
        let sid = out
            .split("[SNIPPET ID: ")
            .nth(1)
            .unwrap()
            .split(']')
            .next()
            .unwrap()
            .to_string();
        (sid, ctx)
    }

    #[tokio::test]
    async fn edit_file_multi_block_all_or_nothing() {
        let dir = std::env::temp_dir().join(format!("dnv-c1-edit-{}", std::process::id()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let (sid, ctx) = seed_edit(&dir, "AAA\nBBB\nCCC\n").await;
        let args = format!(
            r#"{{"path":"e.rs","snippet_id":"{sid}","edits":[{{"search":"AAA","replace":"XXX"}},{{"search":"CCC","replace":"ZZZ"}}]}}"#
        );
        let out = EditFileTool::new().execute(&ctx, &args).await.unwrap();
        assert!(out.contains("2"));
        let after = tokio::fs::read_to_string(dir.join("e.rs")).await.unwrap();
        assert_eq!(after, "XXX\nBBB\nZZZ\n");
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn edit_file_ambiguous_match_fails_whole_call() {
        let dir = std::env::temp_dir().join(format!("dnv-c1-editamb-{}", std::process::id()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let (sid, ctx) = seed_edit(&dir, "dup\ndup\nkeep\n").await;
        // 第 1 块唯一 OK，第 2 块 "dup" 命中 2 处 → 整次失败，文件不变
        let args = format!(
            r#"{{"path":"e.rs","snippet_id":"{sid}","edits":[{{"search":"keep","replace":"k2"}},{{"search":"dup","replace":"d2"}}]}}"#
        );
        let res = EditFileTool::new().execute(&ctx, &args).await;
        assert!(res.is_err(), "ambiguous block must fail whole call");
        let after = tokio::fs::read_to_string(dir.join("e.rs")).await.unwrap();
        assert_eq!(after, "dup\ndup\nkeep\n", "no partial edit");
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }

    #[tokio::test]
    async fn edit_file_single_block_backcompat_unique() {
        let dir = std::env::temp_dir().join(format!("dnv-c1-edit1-{}", std::process::id()));
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let (sid, ctx) = seed_edit(&dir, "hello world\n").await;
        let args = format!(
            r#"{{"path":"e.rs","snippet_id":"{sid}","search":"world","replace":"rust"}}"#
        );
        EditFileTool::new().execute(&ctx, &args).await.unwrap();
        let after = tokio::fs::read_to_string(dir.join("e.rs")).await.unwrap();
        assert_eq!(after, "hello rust\n");
        let _ = tokio::fs::remove_dir_all(&dir).await;
    }
```

- [ ] **Step 2: 跑测试确认失败** — Run `cargo test -p deepseeknova-tools edit_file_multi 2>&1 | tail -3`。Expected: 编译错 `unknown field edits`。

- [ ] **Step 3: 实现** — `EditFileArgs` 改为兼容单块与多块：

```rust
#[derive(Deserialize)]
struct EditBlock {
    search: String,
    replace: String,
}

#[derive(Deserialize)]
struct EditFileArgs {
    path: String,
    #[serde(default)]
    snippet_id: Option<String>,
    // 单块（向后兼容）
    #[serde(default)]
    search: Option<String>,
    #[serde(default)]
    replace: Option<String>,
    // 多块
    #[serde(default)]
    edits: Vec<EditBlock>,
}

impl EditFileArgs {
    /// 归一为块列表：优先 edits；否则用顶层 search/replace 组一个单块。
    fn blocks(&self) -> anyhow::Result<Vec<EditBlock>> {
        if !self.edits.is_empty() {
            return Ok(self
                .edits
                .iter()
                .map(|b| EditBlock {
                    search: b.search.clone(),
                    replace: b.replace.clone(),
                })
                .collect());
        }
        match (&self.search, &self.replace) {
            (Some(s), Some(r)) => Ok(vec![EditBlock {
                search: s.clone(),
                replace: r.clone(),
            }]),
            _ => anyhow::bail!("provide either `edits: [...]` or both `search` and `replace`"),
        }
    }
}
```

`execute` 中 snippet 校验之后、原 `if let Some(pos) = original.find(...)` 整段替换为多块唯一匹配应用：

```rust
        let blocks = parsed.blocks()?;

        // 逐块校验：每块必须在当前内容中唯一命中（0 或 ≥2 → 整次失败，零半改）。
        // 先在不可变副本上算出所有替换点，再一次性构建结果，保证原子性。
        let mut working = original.clone();
        for (i, b) in blocks.iter().enumerate() {
            let count = working.matches(&b.search).count();
            if count == 0 {
                anyhow::bail!(
                    "edit block #{} not found: search text has 0 matches (must be exactly 1)",
                    i + 1
                );
            }
            if count > 1 {
                anyhow::bail!(
                    "edit block #{} ambiguous: search text has {} matches (must be exactly 1); add surrounding context to disambiguate",
                    i + 1,
                    count
                );
            }
            // 唯一命中：应用（replacen 只替 1 处，等价于唯一替换）
            working = working.replacen(&b.search, &b.replace, 1);
        }

        // 原子写
        let tmp_path = path.with_extension(
            path.extension()
                .map(|e| format!("{}.tmp", e.to_string_lossy()))
                .unwrap_or_else(|| "tmp".to_string()),
        );
        let mut tmp = fs::File::create(&tmp_path).await?;
        tmp.write_all(working.as_bytes()).await?;
        tmp.flush().await?;
        fs::rename(&tmp_path, &path).await?;

        Ok(format!(
            "applied {} edit block(s) to {}",
            blocks.len(),
            path.display()
        ))
```

> 注：原「search not found → 候选行提示」的宽松恢复分支被上面的按块报错取代（错误信息已含命中数指引）。若既有测试依赖旧「replaced 1 occurrence」文案，一并更新为新文案。

schema 更新（多块 + 唯一匹配语义 + 保留 snippet_id 必填）：

```rust
            description: "在文件中按 SEARCH/REPLACE 做精确替换。可传 edits 数组一次改多处；\
                 每个 search 必须在文件中唯一命中（0 或多处命中则整次失败、不产生半改），\
                 SEARCH 须逐字匹配含空白缩进。必须先 read_file 并回传其 snippet_id。"
                .to_string(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "要编辑的文件路径。" },
                    "edits": {
                        "type": "array",
                        "description": "多块替换：[{search, replace}, ...]，按顺序各替换一处唯一匹配。单处编辑也可直接用顶层 search/replace。",
                        "items": {
                            "type": "object",
                            "properties": {
                                "search": { "type": "string" },
                                "replace": { "type": "string" }
                            },
                            "required": ["search", "replace"]
                        }
                    },
                    "search": { "type": "string", "description": "单块模式：要查找的唯一文本。" },
                    "replace": { "type": "string", "description": "单块模式：替换文本。" },
                    "snippet_id": { "type": "string", "description": "先前 read_file 返回的 snippet_id（必填）。" }
                },
                "required": ["path", "snippet_id"]
            }),
```

- [ ] **Step 4: 跑测试确认通过** — Run `cargo test -p deepseeknova-tools edit_file 2>&1 | grep 'test result'`。Expected: 全过（含既有 edit 测试；若既有测试断言旧文案/首个匹配语义，更新为唯一匹配语义并在提交信息注明）。

- [ ] **Step 5: fmt + clippy + 提交**

```bash
cargo fmt -p deepseeknova-tools
cargo clippy -p deepseeknova-tools --all-targets -- -D warnings
git add crates/deepseeknova-tools/src/fs.rs
git commit -m "feat(tools/fs): multi-block edit_file with all-or-nothing unique-match semantics"
```


---

## Task 3: 压缩阈值从 budget 推导

**Files:** Modify `crates/deepseeknova-runtime/src/lib.rs`、`crates/deepseeknova-config/src/lib.rs`

- [ ] **Step 1: 定位装配点** — Run `grep -n 'with_compaction_threshold\|compaction_threshold_tokens' crates/deepseeknova-runtime/src/lib.rs`。确认两处：build_agent 内的 agent 侧、coordinator 侧（~L798）。两处都要应用推导。

- [ ] **Step 2: 写失败测试**（runtime `mod tests`，比对既有 build_agent 测试的断言风格；若 Agent 不暴露 threshold getter，用一个薄 helper 测推导函数本身）：

在 runtime lib.rs 加一个 crate 内纯函数并测它：

```rust
/// 压缩阈值推导：显式配置优先；否则 budget 启用时取 max_total_tokens/2；都没有则 None。
fn derive_compaction_threshold(config: &Config) -> Option<u32> {
    if let Some(explicit) = config.agent.compaction_threshold_tokens {
        return Some(explicit);
    }
    if config.budget.enabled {
        return Some((config.budget.max_total_tokens / 2) as u32);
    }
    None
}
```

测试：

```rust
    #[test]
    fn compaction_threshold_derives_from_budget() {
        let mut c = Config::default(); // budget 默认启用、max_total=128000
        assert_eq!(derive_compaction_threshold(&c), Some(64_000));
        c.agent.compaction_threshold_tokens = Some(32_000);
        assert_eq!(derive_compaction_threshold(&c), Some(32_000)); // 显式优先
        c.agent.compaction_threshold_tokens = None;
        c.budget.enabled = false;
        assert_eq!(derive_compaction_threshold(&c), None); // budget 关 → None
    }
```

- [ ] **Step 3: 跑测试确认失败** — Run `cargo test -p deepseeknova-runtime compaction_threshold_derives 2>&1 | tail -3`。Expected: 编译错 `cannot find function derive_compaction_threshold`。

- [ ] **Step 4: 实现** — 加入上面的 `derive_compaction_threshold`；在 build_agent 内 agent 侧装配处，把直接读 `config.agent.compaction_threshold_tokens` 改为 `derive_compaction_threshold(config)`。coordinator 侧（~L798）同理：

```rust
    // 原：if let Some(threshold) = config.agent.compaction_threshold_tokens {
    //         runner = runner.with_compaction_threshold(threshold); }
    if let Some(threshold) = derive_compaction_threshold(config) {
        runner = runner.with_compaction_threshold(Some(threshold));
    }
```

build_agent 内 agent 侧同款（以该处实际调用形态为准，把值源换成 `derive_compaction_threshold(config)`）。

> 若 coordinator 那处的 `config` 变量名不同/借用形态不同，按实际调整；语义不变。

- [ ] **Step 5: config 文档注释** — `crates/deepseeknova-config/src/lib.rs` 中 `compaction_threshold_tokens` 字段的 `///` 注释补一句：

```rust
    /// Token budget for conversation history before compaction triggers.
    /// 留空（None）且 `[budget] enabled=true` 时，运行时按 `budget.max_total_tokens / 2`
    /// 推导（默认 128000 → 64000）；显式设置优先；budget 关闭则不压缩。
    #[serde(default)]
    pub compaction_threshold_tokens: Option<u32>,
```

（保留字段原有属性，仅改注释文本。）

- [ ] **Step 6: 验证 + 提交**

Run `cargo test -p deepseeknova-runtime 2>&1 | grep 'test result'` 全绿；`cargo clippy -p deepseeknova-runtime --all-targets -- -D warnings` 干净。

```bash
cargo fmt -p deepseeknova-runtime -p deepseeknova-config
git add crates/deepseeknova-runtime/src/lib.rs crates/deepseeknova-config/src/lib.rs
git commit -m "feat(runtime): derive compaction threshold from budget when unset (default-on lossless L1)"
```

---

## Task 4: 工具 schema 文案压缩 + 防膨胀回归测试

**Files:** Modify `crates/deepseeknova-tools/src/lib.rs`（回归测试）+ 各工具文件（文案）

顺序：先建回归测试（记录压缩前基线数字），再压缩，最后收紧上限——TDD 反向：测试先绿（宽松上限）→ 压缩 → 收紧上限到压缩后值。

- [ ] **Step 1: 建回归测试并测出基线** — 在 `crates/deepseeknova-tools/src/lib.rs` 末尾加：

```rust
#[cfg(test)]
mod schema_budget {
    use super::*;

    /// 全量内置工具 schema 序列化后的总字符数上限。schema 属稳定前缀，
    /// 每次缓存 MISS 全额重付——加此上限防止文案慢性膨胀（支柱③）。
    /// 收紧准则：压缩后取实测值 + ~10% 余量。
    const MAX_SCHEMA_CHARS: usize = 100_000; // Step 1 临时宽松值，Step 3 收紧

    #[test]
    fn builtin_tool_schemas_stay_within_budget() {
        let tools = all_builtin_tools();
        let total: usize = tools
            .iter()
            .map(|t| {
                let s = t.schema();
                s.name.len()
                    + s.description.len()
                    + serde_json::to_string(&s.parameters).map(|j| j.len()).unwrap_or(0)
            })
            .sum();
        println!("BUILTIN_SCHEMA_TOTAL_CHARS = {total}");
        assert!(
            total <= MAX_SCHEMA_CHARS,
            "schema total {total} exceeds budget {MAX_SCHEMA_CHARS}"
        );
    }
}
```

Run `cargo test -p deepseeknova-tools builtin_tool_schemas -- --nocapture 2>&1 | grep -E 'BUILTIN_SCHEMA_TOTAL_CHARS|test result'`。**记下压缩前 total**（记为 `BEFORE`）。

- [ ] **Step 2: 逐文件压缩文案** — 对 `fs.rs`（Task 1/2 已顺带优化 read/edit，本步覆盖 write_file/move_file）、`grep.rs`、`shell.rs`、`memory.rs`、`web_fetch.rs`、`todo.rs` 及 graph 工具（`graph_tools.rs`）、`delegate.rs` 的每个 `description` 与参数 `description`：
  - 删冗余定语、合并重复语义、统一简洁中/英风格（跟随各文件既有语言）。
  - **保留行为引导**（如 read_file「先定位再区间读」、edit_file「唯一匹配」这类是信息不是废话）。
  - 不改工具 `name`、不改参数结构、不改 `required`——只动 description 文本。
  逐文件改完各跑 `cargo test -p deepseeknova-tools` 确保无回归。

- [ ] **Step 3: 收紧上限并测降幅** — 再跑 Step 1 命令记 `AFTER`；确认 `AFTER <= BEFORE * 0.6`（降幅 ≥40%）。把 `MAX_SCHEMA_CHARS` 改为 `AFTER` 上浮 ~10% 的定值（如 AFTER=42000 → 46000）。重跑测试通过。

- [ ] **Step 4: clippy + fmt + 提交**

Run `cargo clippy -p deepseeknova-tools --all-targets -- -D warnings` 干净。

```bash
cargo fmt -p deepseeknova-tools
git add crates/deepseeknova-tools/src/
git commit -m "perf(tools): slim tool-schema prose (~40% smaller) + schema-size regression guard"
```

在提交信息或 PR 描述记录 `BEFORE`/`AFTER` 数字（验收 §4）。

---

## Task 5: 全量验收 + CHANGELOG

**Files:** Modify `CHANGELOG.md`

- [ ] **Step 1: CHANGELOG** — `[Unreleased]` 下：`### ⚠ Breaking` 补一条，`### Added`/`### Changed` 各补：

```markdown
### ⚠ Breaking

- `edit_file` 语义收紧：SEARCH 须在文件中**唯一**命中（旧版替换首个匹配）；0 或多处命中
  时整次调用失败、不产生半改。多处编辑请用新的 `edits: [{search, replace}, ...]` 数组。

### Added

- `read_file` 支持 `start_line`/`end_line` 区间读，只把需要的行送入上下文（省 token）。
- `edit_file` 支持 `edits` 多块数组，一次调用原子地替换多处（全有或全无）。

### Changed

- `compaction_threshold_tokens` 留空时运行时按 `budget.max_total_tokens / 2` 推导，
  让无损的 L1 结果截断默认生效；显式配置与 `[budget] enabled=false` 时行为不变。
- 内置工具 schema 文案精简约 40%，降低每次缓存未命中的固定 token 开销。
```

- [ ] **Step 2: 全量 check** — Run `cd .../feat-token-c1 && make check`。Expected: exit 0。

- [ ] **Step 3: desktop 回归** — Run `export PATH="/Users/ze/.nvm/versions/node/v20.20.2/bin:$PATH" && make check-desktop`（无桌面改动，回归确认）。Expected: exit 0。

- [ ] **Step 4: token 对比估算（写入 PR 描述）** — 以「编辑 500 行文件中 2 处」为基准场景，估算：
  - 改前：read_file 整文件（~500 行 ≈ 2500+ token）+ 若用 write_file 回写整文件（再 ~2500 token）。
  - 改后：grep/search_code 定位（~数十 token）+ read_file 区间 40 行（~200 token）+ edit_file 2 块（~100 token）。
  给出量级对比（预期读+写合计降一个数量级）。此为估算，非精测。

- [ ] **Step 5: 提交 + 整体终审**

```bash
git add CHANGELOG.md
git commit -m "docs(changelog): C1 token savings (ranged read, multi-edit, threshold default, schema slim)"
```

---

## 验收清单（对照 spec §4）

| spec 验收项 | 落点 |
|---|---|
| 多块编辑：3 处全成功；1 处歧义→全次失败指明块号；单块兼容 | T2 三测试 |
| 区间读：读 L100–L140 只返回该区间；片段 snippet 通过 staleness 验证 | T1 测试 + snippet 按整文件注册（校验兼容） |
| 阈值推导：默认 64K / 显式优先 / budget 关→None | T3 测试 |
| schema 体积降幅 ≥40% + 回归上限生效 | T4（BEFORE/AFTER 记录） |
| 模拟任务 token 量级对比 | T5 Step 4 |
| make check + check-desktop 全绿；零行为开关（#2 有配置逃生舱） | T5 Step 2/3 |

## 自审记录（fresh-eyes 后确认 3 点）

1. **区间读 × snippet 兼容性**（最高风险点）：区间读仍按**整文件**注册 snippet，edit_file 校验读整文件——两侧都是整文件 hash，区间读不破坏既有 read-then-edit 契约。已在 T1 Step 3 显式设计 + T2 seed_edit 用整文件读拿 snippet 验证。
2. **多块原子性**：先在 `working` 副本上逐块校验+应用，全部通过才落盘一次；任一块 0/≥2 命中即 bail，磁盘零改动（T2 ambiguous 测试锁死「no partial edit」）。
3. **单块唯一匹配 breaking**：既有 edit_file 测试若断言「首个匹配/replaced 1 occurrence」需更新为唯一匹配语义——T2 Step 4 已提示实现者更新并在提交注明；CHANGELOG T5 标注。
