# DeepseekNova — 架构设计文档

> **文档性质**：本文件是**架构设计记录（Architecture Design Record）**，**非 Agent 操作指令**。Agent 在本项目中的工作指令以 [AGENTS.md](AGENTS.md) 为唯一准绳。文中标注 **[规划中/未实现]** 的能力尚不存在，不得以现行能力口吻引用或依赖。

## 一、项目概览

DeepseekNova 是一个 Rust 编写的 AI Agent 框架，包含 21 个 crate。

### 命名现状（权威说明）

> 本节是项目命名的**单一权威说明**，其他文档涉及命名时以本节为准。

| 项 | 现状 | 说明 |
|----|------|------|
| **正式名** | `DeepseekNova` | 仓库名、crate 前缀（`deepseeknova-*`）、环境变量前缀（`DEEPSEEKNOVA_`）均使用此名 |
| **工作区目录名** | `DPronix` | 本地检出目录沿用早期名称，仅为路径名，不代表项目名 |
| **历史路径** | `crates/reasonix-*` → `crates/dpronix-*` → `crates/deepnova-*` → `crates/deepseeknova-*` | 完整重命名链：`reasonix` → `DPronix`（提交 `69509d5`）→ `DeepNova`（提交 `8a95226`）→ `DeepseekNova`（提交 `c5336db`）；另外 `.dpronix/` 已在提交 `7319692` 迁移至 `.deepseeknova/`，环境变量前缀从 `DPRONIX_` 统一为 `DEEPSEEKNOVA_` |

重命名工作已全部完成，当前代码与配置中不再使用旧名称。

**Git 历史分析注意**：由于上述重命名，依赖 `git log` 的历史统计（如热点分析）必须启用 rename 跟踪，否则同一文件会以新旧两个路径重复计入。本仓库通过以下本地配置合并更名前后的路径（clone 后需执行一次，详见 `BUILDING.md`）：

```bash
git config log.follow true    # git log -- <path> 自动跟随重命名
git config diff.renames true  # diff/log 统计启用重命名检测
```

### Crate 结构

> **单一真相源**：21 个 crate 的权威清单以 [AGENTS.md §2「项目简介」](AGENTS.md) 为准，本文档不再重复维护 crate 列表，仅记录分层设计意图。

设计上，全部 crate 按职责分为四层：

| 层 | 职责 |
|----|------|
| **核心层** | 类型定义、事件总线、工作区索引、持久化 |
| **能力层** | LLM 接入、工具实现、协议桥接、安全隔离 |
| **编排层** | Agent 主循环、多 Agent 协调、追踪、组合根 |
| **前端层** | CLI、TUI、HTTP API |

各 crate 与层的具体对应关系见 AGENTS.md 权威清单中每个 crate 的职责注释。

---

## 二、GOAP 规划器 — 决策机制

> **状态**：**已裁撤（B0）**。GOAP/Swarm 实验实现随 `deepseeknova-orch` crate 删除；本节仅作历史设计记录保留，历史实现见删除提交之前的 git 历史。多智能体能力由 `deepseeknova-agent` 的 delegate/子代理路径提供（见长任务与多智能体引擎 spec）。

### 概述

GOAP（Goal-Oriented Action Planning）是 `deepseeknova-orch` crate 的核心组件。它将用户自然语言目标分解为可执行的动作 DAG（有向无环图），然后按依赖顺序执行。

### 工作流程

```
用户目标 (Goal)
    │
    ▼
┌─────────────────────┐
│  1. Decompose       │  LLM 将目标分解为 Action 列表
│  (使用 thinking mode) │  输出: JSON { actions, dependencies }
└─────────┬───────────┘
          │
          ▼
┌─────────────────────┐
│  2. Schedule        │  拓扑就绪调度（依赖满足即就绪，非 A*）
│  (依赖图拓扑排序)      │  ready_actions() = 依赖已满足的动作
└─────────┬───────────┘
          │
          ▼
┌─────────────────────┐
│  3. Execute         │  按 ready 列表执行，支持并行委派
│  (with retry)       │  失败 → 指数退避重试 (base_delay × 2^attempt)
└─────────┬───────────┘
          │
          ▼
┌─────────────────────┐
│  4. Replan          │  如果动作全部 Blocked → 标记 Plan 为 Failed
│  (adaptive)         │  否则继续直到所有动作 Completed
└─────────────────────┘
```

### 数据结构

**Goal**: 自然语言描述 + 约束列表 + 成功标准列表

**Action**: 
- `id`: 唯一标识
- `name`: 动作名称（动词-名词格式，如 `create_file`）
- `preconditions`: 前置条件（自然语言事实）
- `effects`: 完成后的效果
- `cost`: 估算复杂度（1-10，越低优先级越高）
- `tool`: 使用的工具（如 `edit_file`, `bash`）
- `delegatable`: 是否可委派给子 Agent

**Plan**: 
- `actions`: Action 列表
- `dependencies`: `HashMap<action_id, Vec<dependency_action_ids>>` — DAG 依赖图
- `status`: Draft → InProgress → Completed / Failed

### 调度算法

调度使用 `ready_actions()` 函数，它是基于依赖图的拓扑排序：

1. 遍历所有动作
2. 跳过已完成（Completed）和进行中（InProgress）的
3. 检查该动作的所有依赖是否已完成
4. 如果全部依赖完成 → 该动作 ready

如果所有未完成的动作都处于 Blocked 状态，说明依赖图存在死锁，Plan 标记为 Failed。

### 重试机制

`execute_action_with_retry` 实现指数退避重试：

```
attempt 0: 立即执行
attempt 1: 等待 base_delay × 2^0 + jitter
attempt 2: 等待 base_delay × 2^1 + jitter
attempt 3: 等待 base_delay × 2^2 + jitter
```

jitter 为 0-50% 的 delay 随机值，避免 thundering herd。

### 解析策略

LLM 输出的 Plan 支持两种格式：

1. **JSON（首选）**: 从 LLM 输出中提取 JSON 块（支持 ```` ```json ```` 包裹或裸 JSON），解析为 `actions` + `dependencies`
2. **YAML-like（降级）**: 逐行解析 `## Actions` / `## Dependencies` 区段

如果两种解析都失败，创建一个单动作 Plan 直接执行原始目标。

---

## 三、四层记忆架构

### 概述

DeepseekNova 的记忆系统受 Hermes Agent 的闭环学习系统启发，由四个层次组成：

```
┌─────────────────────────────────────────────────┐
│  层 1: 短期记忆 (ShortTerm)                       │
│  当前对话上下文窗口 — WorkingMemory               │
│  不持久化，会话结束即消失                           │
├─────────────────────────────────────────────────┤
│  层 2: 任务记忆 (Task)                            │
│  会话历史、项目进展、决策记录                        │
│  存储: SQLite FTS5 (MemoryStore)                  │
├─────────────────────────────────────────────────┤
│  层 3: 技能记忆 (Skill)                            │
│  从经验中提炼的可复用模式                           │
│  存储: .deepseeknova/skills/*.md (Markdown+YAML)  │
├─────────────────────────────────────────────────┤
│  层 4: 用户画像 (UserProfile)                     │
│  用户偏好、编码风格、技术栈选择                      │
│  存储: UserProfile (HashMap, 可导出)               │
└─────────────────────────────────────────────────┘
```

### 各层详解

#### 层 1: 短期记忆 — WorkingMemory

- **存储**: 内存中的 `Vec<Message>`
- **操作**: `add_message()`, `rewind(n)`, `pin(msg)`, `clear()`
- **特点**: 对话上下文窗口，pin 的消息不会被 rewind 截断

#### 层 2: 任务记忆 — MemoryStore (SQLite FTS5)

- **存储**: SQLite FTS5 全文检索引擎
- **Schema**: 
  ```sql
  CREATE VIRTUAL TABLE memory_fts USING fts5(
    content, tags, category, source,
    created_at UNINDEXED, importance UNINDEXED, id UNINDEXED,
    tokenize = 'porter unicode61'
  );
  ```
- **搜索**: BM25 排序，支持全文 MATCH 查询和分类过滤
- **Upsert**: 先 DELETE 同 ID 再 INSERT，支持更新

#### 层 3: 技能记忆 — SkillManager

- **存储**: `.deepseeknova/skills/*.md`（Markdown + YAML frontmatter，兼容 agentskills.io）
- **自动提取**: 当任务满足以下条件时触发提取：
  - 工具调用次数 ≥ 5
  - 执行步骤数 ≥ 3
  - 任务结果为 Success 或 PartialSuccess
- **使用统计**: 每个 skill 记录 `use_count` 和 `success_count`，用于排序和推荐
- **匹配**: `find_matching_skills(query)` 按 name / tags / triggers / body 全文匹配

#### 层 4: 用户画像 — UserProfile

- **存储**: `HashMap<String, ProfileEntry>`
- **观察机制**: `observe(key, value, category)` — 首次观察 confidence=0.3，每次重复观察 +0.15（上限 1.0）
- **分类**: 语言偏好 / 框架偏好 / 代码风格 / 沟通风格 / 项目上下文 / 工作习惯 / 技能水平
- **导出**: `export()` → `Vec<ProfileEntry>`，支持跨会话迁移

### 记忆生命周期

记忆条目有自己的生命周期阶段：

```
Candidate ──(recall ≥1)──▶ Verified ──(recall ≥3 AND age >7d AND importance ≥0.5)──▶ Permanent
    │                           │
    └──(age >30d, recall=0,     └──(importance <0.2)──▶ Archived
        importance <0.3)──▶ Archived                        │
                                          ▲                  │
                                          └──(reinforce +0.5)──┘
```

**衰减机制**:
- 每日衰减率: 0.02（默认）
- 最近 7 天内被召回的记忆衰减率减半
- Permanent 阶段不衰减
- importance 降到 0.1 以下自动归档

**强化机制**:
- 用户显式提及的记忆可通过 `reinforce(boost)` 提升 importance
- 从 Archived 状态可以通过强化恢复到 Candidate

### Recall Engine — 跨层召回

每轮对话开始时，`RecallEngine` 执行三层召回：

1. **记忆搜索**: 从 MemoryStore 中按用户 query 做 FTS5 全文检索（最多 10 条）
2. **技能匹配**: 从 SkillManager 中按 name/tags/triggers/body 匹配（最多 3 个）
3. **用户画像**: 从 UserProfile 生成分类摘要

三部分合并为一个 context block，注入到 system prompt 中。

### 层间冲突解决

当不同层的记忆产生矛盾时（例如用户之前偏好 Python，现在改用 Rust）：

1. **UserProfile 自动更新**: `observe()` 检测到同 key 的新 value 时，直接更新（保留 times_observed 累加）
2. **旧记忆衰减**: 旧的任务记忆如果不再被召回，importance 会逐渐降低直至归档
3. **显式纠正**: 用户可以说"忘掉这个"，触发 `MemoryStore.delete()`

---

## 四、Swarm 协调 — 多 Agent 集群

> **状态**：**已裁撤（B0）**。GOAP/Swarm 实验实现随 `deepseeknova-orch` crate 删除；本节仅作历史设计记录保留，历史实现见删除提交之前的 git 历史。多智能体能力由 `deepseeknova-agent` 的 delegate/子代理路径提供（见长任务与多智能体引擎 spec）。

### Queen-Led 层级架构

```
       Queen Agent (planning + coordination)
      /         |           \
  Worker 1   Worker 2   Worker 3
   (code)    (test)     (review)
      \         |           /
       Shared Memory & Results
```

### 角色

| 角色 | 职责 | 默认模型 |
|------|------|---------|
| Queen | 规划、分解任务、综合结果 | deepseek-v4-pro |
| Worker | 执行被委派的动作 | deepseek-v4-flash |
| Reviewer | 验证工作产物 | deepseek-v4-pro |
| Researcher | 收集信息 | deepseek-v4-flash |

### 任务复杂度路由

| 复杂度 | 示例 | thinking | reasoning_effort |
|--------|------|----------|-------------------|
| Trivial | 格式转换、字段提取 | off | disabled |
| Normal | 标准 Agent 任务 | on | high |
| Critical | 安全相关或高风险决策 | on | max |

### 执行流程

1. **Phase 1 — 分解**: Queen 将 Plan 中 `delegatable=true` 的动作分解为 SwarmTask
2. **Phase 2 — 分配**: `select_worker()` 按动作名称匹配角色（含 "test"/"review" → Reviewer，含 "research" → Researcher，其他 → Worker）
3. **Phase 3 — 并发执行**: 使用 `tokio::sync::Semaphore` 控制最大并发数（默认 5），每个 Worker 独立流式执行
4. **Phase 4 — 综合**: 如果 `consensus_required=true`，Queen 将所有 Worker 的输出汇总并生成最终结果

### 通信机制

Swarm 成员通过 `SwarmMessage` 进行异步通信：

| 消息类型 | 用途 |
|---------|------|
| TaskAssignment | Queen → Worker 分配任务 |
| TaskResult | Worker → Queen 汇报结果 |
| StatusUpdate | 任意 → 全体 状态广播 |
| Question | Worker → Queen 请求澄清 |
| Coordination | Worker ↔ Worker 直接协作 |

通信使用 `tokio::sync::mpsc` 通道，容量 256。

### 进度追踪

`ProgressTracker` 是线程安全的（`Arc<RwLock<TrackerState>>`），前端（HTTP API / TUI）可实时查询：

- `start(goal, config)` — 开始编排
- `register_plan(plan)` — 注册动作列表
- `mark_started(action_id, worker)` — 标记开始
- `mark_completed(action_id, output)` — 标记完成
- `mark_failed(action_id, reason)` — 标记失败
- `record_retry(action_id)` — 记录重试
- `report()` — 生成完整进度报告（可序列化为 JSON 给前端）

---

## 五、Agent Federation — 跨实例联邦调度 [规划中/未实现]

### 概述

> **状态**：[规划中/未实现]（见 §十 P5）。原先位于 `deepseeknova-orch` 的类型定义与接口预留已随该 crate 删除（B0），本节仅为规划性记录，不可作为现行能力使用。

Agent Federation 允许多个 DeepseekNova 实例跨进程/跨机器协作。

### 设计原则

1. **去中心化**: 没有全局 Queen，各实例平等
2. **能力声明**: 每个实例声明自己的工具集和模型配置
3. **任务路由**: 按能力匹配分发任务
4. **结果合并**: 请求方负责合并联邦结果

### 预留接口

- `SwarmAgent` 的 `provider` 字段（`Arc<dyn Runner>`）可以指向远程实例
- `SwarmMessage` 的 `from`/`to` 字段支持跨实例路由
- `SwarmMessageType::Coordination` 用于跨实例直接通信

---

## 六、内置 Skill 设计

### 设计理念

DeepseekNova 的 Skill 不是静态文档，而是 **可执行的认知框架** — 每个 Skill 定义了一种思考方式和工作流程。

### 内置 Skill 列表（.deepseeknova/skills/）

| Skill | 触发条件 | 核心流程 |
|-------|---------|---------|
| `brainstorming` | 新功能/设计决策前 | 重述意图 → 提问 → 探索2+方案 → 推荐 → 定义完成标准 |
| `systematic-debugging` | Bug/测试失败/异常行为 | 复现 → 观察 → 假设 → 验证 → 修复根因 → 验证 |
| `test-driven-development` | 实现功能或修复Bug | Red(写失败测试) → Green(最小实现) → Refactor(重构) |
| `verification-before-completion` | 声称完成/修复/通过前 | 推导检查项 → 运行 → 读取输出 → 如实报告 |
| `writing-plans` | 多步骤任务规划 | 摘要 → 分组变更 → 测试计划 → 假设 |

每个 Skill 文件格式：

```markdown
---
name: skill-name
description: 何时使用
tools_allowed:
  - read_file
  - grep
---
# Skill Title

具体的工作流程和红旗清单...
```

### 计划中的 Skill（DESIGN.md 记录）[已落地]

以下 Skill 已落地为 `crates/deepseeknova-skills/builtin/*.md`，可直接在工作流程中引用：

| Skill | 状态 | 能力 |
|-------|------|------|
| `frontend-developer` | ✅ 已落地 | UI/UX 设计和代码生成 |
| `coding-copilot` | ✅ 已落地 | 多语言编码助手 |
| `loop-engineering` | ✅ 已落地 | 生成→评估→改进循环 |
| `first-principles` | ✅ 已落地 | 第一性原理推理 |
| `adversarial-review` | ✅ 已落地 | 对抗式审查 |
| `dna-spec` | ✅ 已落地 | Agent 工作规范（DNA Spec） |

---

## 七、项目后置产出 (Post-Project Artifacts) [已实现]

> **状态**：[已实现]（见 §十 P3）。生成能力位于 `deepseeknova-core::artifacts`
> （wiki/cards/distill，含测试），CLI 入口为 `deepseeknova artifacts wiki|cards`；
> 记忆沉淀由 runtime 的蒸馏钩子（启发式）与 `DistillationEngine`（库级）提供。

### 流程

```
项目完成 → 询问用户 → 选择性生成：
  ├── 📖 Repo Wiki     — 项目知识库
  ├── 🎴 知识卡片      — 关键决策和经验的可视化卡片
  └── 🧠 记忆沉淀      — 将经验存入长期记忆
```

### 1. Repo Wiki
- 自动从对话历史和代码变更中提取：
  - 架构决策记录 (ADR)
  - API 文档
  - 组件依赖图
  - 变更日志
- 输出: Markdown 文件，可推送到 GitHub Wiki

### 2. 知识卡片
- 每张卡片包含：
  - 标题 + 标签
  - 核心知识点
  - 代码示例
  - 关联卡片
- 输出: HTML 卡片或 Markdown

### 3. 记忆沉淀
- 提取本次项目中的：
  - 用户偏好（编码风格、技术栈选择）
  - 有效模式（什么方法奏效）
  - 失败教训（什么方法不奏效）
  - 项目上下文（用于后续项目）
- 存入 DeepseekNova 的长期记忆系统

---

## 八、Agent 工作规范 (DNA Spec) [规划中/未实现]

> **状态**：[规划中/未实现]（见 §十 P4）。本规范尚未接入运行时 system prompt，属于设计目标而非现行工作指令；现行 Agent 指令见 [AGENTS.md](AGENTS.md)。

```
Phase 1: 理解 (Understand)
  ├── 明确用户意图
  ├── 澄清模糊需求
  └── 确认成功标准

Phase 2: 规划 (Plan)
  ├── 拆解任务为可验证的子任务
  ├── 选择合适的工具和 Skill
  └── 输出执行计划

Phase 3: 执行 (Execute)
  ├── 按计划执行
  ├── 每步产出可验证的中间结果
  └── 遇到阻塞即时反馈

Phase 4: 验证 (Verify)
  ├── 自动运行测试
  ├── 对抗式审查（依赖 adversarial-review skill，见 §六，规划中/未实现）
  └── 与成功标准对比

Phase 5: 沉淀 (Distill) ← 这是大多数 Agent 缺失的
  ├── 提炼可复用的 Skill
  ├── 更新记忆
  ├── 询问是否生成 Wiki/知识卡片（依赖 §七 后置产出，规划中/未实现）
  └── 记录项目经验
```

**核心原则:**
- **可验证性**: 每个产出必须有验证标准
- **可追溯性**: 每个决策都有上下文
- **可复用性**: 每次工作的经验都要沉淀
- **诚实性**: 不确定就说不确定，不编造

---

## 九、安全架构

### 深度防御层

```
┌─────────────────────────────────────────────────┐
│  Layer 1: 沙箱执行 (Sandbox)                     │
│  macOS: Seatbelt (sandbox-exec)                  │
│  Linux: bubblewrap (bwrap)                       │
│  Windows: NoOpSandbox (无隔离，待实现)              │
├─────────────────────────────────────────────────┤
│  Layer 2: 权限门控 (Permission)                    │
│  Allow / Ask / Deny — 12 条独立规则               │
├─────────────────────────────────────────────────┤
│  Layer 3: 安全策略 (Security)                      │
│  路径白名单/黑名单、命令前缀过滤、域名限制            │
├─────────────────────────────────────────────────┤
│  Layer 4: 资源限额 (ResourceLimits)               │
│  max_files / max_file_size / max_tool_calls       │
│  max_execution_time / max_output_bytes            │
├─────────────────────────────────────────────────┤
│  Layer 5: 审计日志 (AuditLog)                     │
│  TracingAuditLogger — 安全事件全程记录             │
└─────────────────────────────────────────────────┘
```

### 恶意命令黑名单

`deepseeknova-permission` 内置以下模式检测：

- `rm -rf /*` — 递归删除根目录
- `mkfs` — 格式化文件系统
- `dd if=` — 磁盘写入
- `:(){ :|:& };:` — Fork bomb
- `chmod -r 777 /` — 权限篡改
- `> /dev/sda` — 设备写入
- `shutdown` — 关机

---

## 十、执行优先级

| 优先级 | 任务 | 预计工作量 |
|--------|------|-----------|
| P0 | ~~重命名 DPronix → DeepseekNova~~ ✅ | ~~机械替换~~ 已完成 |
| P1 | 实现自动记忆系统 | ✅ 已完成（记忆引擎 + 召回 + 蒸馏 + 任务-文件关联） |
| P2 | 内置 5 个 Skill | ✅ 已完成（6 个 builtin skill md） |
| P3 | 项目后置产出 | ✅ 已完成（artifacts 库 + `artifacts wiki/cards` CLI） |
| P4 | Agent 工作规范 | ✅ 部分完成（dna-spec skill 已落地；system prompt 接入待定） |
| P5 | Agent Federation 协议 | ⏳ 待实现（跨实例通信，需先出协议设计） |
| P6 | Windows 沙箱隔离 | ⏳ 待实现（Job Object / AppContainer） |
