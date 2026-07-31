# deepsec 式安全扫描流水线设计（P1：scan + process + 报表）

- 日期：2026-07-31
- 状态：已确认（用户批准）
- 来源：融入 [vercel-labs/deepsec](https://github.com/vercel-labs/deepsec) 的
  agent 驱动漏洞扫描流水线（scan → process → triage → revalidate）
- 前置：模型指针体系 + 全路径计量已合入 main（Task 指针可直接消费）

## 背景与分期

deepsec 原生流水线为四阶段 + 可恢复 + 分布式。本期只落 P1 骨架的最小端到端闭环：

- **P1（本 spec）**：`scan`（regex matcher 零 AI 定位候选点）+ `process`
  （每 finding 起一次性 agent 调查判真伪）+ 报表；新建 `deepseeknova-scanner` crate
- **P2（暂缓）**：`triage`（quick 指针廉价模型分级 P0/P1/P2）、`revalidate`
  （复核降误报、查 git 历史是否已修）
- **P3（暂缓）**：可恢复断点续扫（复用 deepseeknova-checkpoint）、`--diff` PR 模式、
  外部 TOML 规则、分布式 sandbox 执行

## 架构与 crate 边界

新建 `deepseeknova-scanner`（能力层，与现有 crate 平级），依赖：
- `deepseeknova-core`（Runner / Provider trait / RunInput）
- `deepseeknova-graph`（`collect_files` + `load_gitignore` + `Lang::from_path`）
- `deepseeknova-security`（`path::secure_resolve` 约束扫描根）
- `deepseeknova-provider`（ModelRouter 取 Task 指针 provider）
- `regex`、`serde`、`tokio`（均已在 workspace.dependencies）
- **不依赖 deepseeknova-sandbox**（P1 只读分析代码，不执行外部工具）

模块划分（各文件单一职责）：

| 文件 | 职责 |
| --- | --- |
| `rule.rs` | `Rule { id, severity, lang: Option<Lang>, pattern: Regex, message }` + `builtin_rules()` 内置高信号规则集 |
| `finding.rs` | `Finding { rule_id, severity, path, line, excerpt, verdict: Option<Verdict> }`；`Severity` 枚举；`Verdict { true_positive: bool, note: String }` |
| `scan.rs` | `scan_files(root, &[Rule]) -> Vec<Finding>`：遍历 → 逐文件逐行 regex 匹配（零 AI） |
| `investigate.rs` | `investigate(&Finding, &dyn Runner) -> Option<Verdict>`：每 finding 起一次性 agent run，lenient 解析裁决 |
| `report.rs` | `ScanReport`（findings 按 severity 分组）+ markdown / JSON 渲染 |
| `lib.rs` | 编排 `run_scan(config) -> ScanReport` |

## 数据流

```
deepseeknova scan [--path .] [--format md|json] [--no-ai] [--severity-min low]
  1. discover: graph::collect_files(root) + load_gitignore → Lang::from_path 过滤支持的语言
  2. scan:     每文件逐行匹配 builtin_rules（按 rule.lang 过滤）→ Vec<Finding>（快，零 AI）
  3. process（除非 --no-ai）：每 finding 起一次性 agent（Task 指针 provider +
     read_file/grep 工具，max_steps≈5）→ 投喂调查 prompt → 解析 Verdict
  4. report:   按 severity 分组渲染，stdout（md）或 JSON
```

## CLI 集成（deepseeknova-cli）

- `cli.rs` 新增 `Scan { path: Option<String>, format: String, no_ai: bool, severity_min: String }`
  变体（复用现有 ModelArgs 风格；format 默认 "md"，severity_min 默认 "low"）
- `main.rs` 新增 Scan 分支：复用已构建的 `model_router`，取
  `provider_for(ModelRole::Task, None)` 注入一次性 agent 工厂；process 阶段 token
  经 MeteredProvider 自动计入 Task 角色（复用既成计量，无新增计量代码）
- 一次性 agent 复用 Run 单代理路径的最小装配（provider + read_file/grep 工具 +
  受限 max_steps），不接 MCP / graph / memory 扩展

## 错误处理与边界

| 场景 | 处理 |
| --- | --- |
| 规则正则编译失败 | `builtin_rules()` 内单测保证全部可编译；P1 无用户规则输入面 |
| 单文件读取失败（权限 / 非 UTF-8） | 跳过并计数，不中断整体扫描 |
| 某 finding 的 AI 调查失败 / 超时 | 该 finding `verdict = None`（未判定），报表注明，不影响其他 |
| `--no-ai` | 跳过 process，findings 全部 `verdict = None`（纯 matcher 输出） |
| 无 provider 配置但需 AI | 启动即报错，提示配 provider 或加 `--no-ai` |
| 路径逃逸 | `security::path::secure_resolve` 约束在 root 内 |

## 明确不做（YAGNI）

- triage / revalidate 阶段（P2）
- 可恢复断点续扫、`--diff` PR 模式、外部 TOML 规则、分布式 sandbox（P3）
- 桌面端 UI 集成

## 测试计划

- **rule**：`builtin_rules()` 全部正则可编译；每规则一条命中样本 + 一条负样本
- **scan**：临时目录构造含已知模式的文件 → 断言 finding 数 / 行号 / rule_id；
  gitignore 排除生效；非 UTF-8 / 超大文件跳过
- **finding / report**：severity 分组排序；md 与 json 渲染；`--no-ai` 时 verdict 全 None
- **investigate**：MockProvider 返回构造的 verdict → 断言解析；解析失败降级为 None
- **CLI**：`cargo check` + clippy；scan 子命令 e2e（`--no-ai` 跑通临时目录）
- **回归**：`make check` 全量（新增 crate，需纳入 workspace 成员）

## 内置规则集（P1 起始集，实现时按语言细化）

高信号、低误报优先（真伪由 process 阶段的 AI 调查裁定）：
- Rust：`.unwrap()` / `.expect(` / `panic!(` 于非测试路径（低 severity，量大靠 AI 降噪）
- 通用：疑似硬编码密钥模式（`api[_-]?key\s*=\s*["']`、`secret\s*=`）（高 severity）
- 通用：SQL 字符串拼接（`format!(".*SELECT.*{`）（中 severity）
- 通用：命令注入面（`Command::new` 后接变量插值 / `sh -c` + 变量）（高 severity）

具体正则在实现时逐条附命中 + 负样本测试；起始集小而准，宁缺勿滥。

## 假设与置信度

- 置信度：**高**
- 已验证可复用：graph `collect_files`/`Lang::from_path`、security `path::secure_resolve`、
  一次性 agent（Runner + Agent + read_file/grep 工具）、Task 指针 MeteredProvider 计量、
  regex/walkdir 已在 workspace deps、CLI Commands 枚举风格
- 残余风险（低）：verdict 结构化解析依赖模型输出——用 lenient 解析兜底（失败即 None），
  与 B3 review 门禁的 verdict 解析同款策略
