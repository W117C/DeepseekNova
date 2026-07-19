# DeepNova — 重命名 + 架构升级设计文档

## 一、重命名：DeepNova → DeepNova

### 范围
- 21 个 crate 目录：`deepnova-*` → `deepnova-*`
- 所有 `Cargo.toml` 中的 package name 和依赖引用
- 所有 Rust 源码中的 `deepnova_*` / `deepnova-*` 引用
- CI workflows、deny.toml、README
- GitHub 仓库名（需要用户在 GitHub 设置中改）

### 命名规则
| 原名 | 新名 |
|------|------|
| `deepnova-core` | `deepnova-core` |
| `deepnova-agent` | `deepnova-agent` |
| `deepnova-cli` | `deepnova-cli` |
| ... | ... |
| `deepnova_desktop` (crate) | `deepnova_desktop` |
| `DeepNova` (展示名) | `DeepNova` |

---

## 二、代码审查结果

### 冗余/空模块（stubs）
以下模块几乎是空的，可以考虑填充或删除：

| 文件 | 状态 | 建议 |
|------|------|------|
| `deepnova-core/src/identity/merkle/mod.rs` | 1行 `pub mod tree;` | 检查 tree.rs 是否有实现 |
| `deepnova-agent/src/budget/mod.rs` | 1行 `pub mod controller;` | 转发模块，保留 |
| `deepnova-core/src/memory/lifecycle.rs` | 9行 enum | 可填充或保留为占位 |
| `deepnova-core/src/prefix/mod.rs` | 7个子模块声明 | 检查子模块是否有实现 |

### 编译质量
- ✅ Clippy 零警告
- ✅ 360/361 测试通过
- ✅ 无 dead_code 警告
- ✅ 无 unused imports

---

## 三、内置 Skill 设计

### 设计理念
DeepNova 的 Skill 不是静态文档，而是 **可执行的认知框架** — 每个 Skill 定义了一种思考方式和工作流程。

### 内置 Skill 列表

#### 1. `frontend-developer` — 前端开发
- **触发**: 用户要求构建网页、UI 组件、dashboard
- **能力**: HTML/CSS/JS/React/Vue/Svelte 代码生成，设计系统，响应式布局
- **实现**: 复用现有 `deepnova-tools` 的 write_file/edit_file + 设计规范知识库

#### 2. `coding-copilot` — 编程助手
- **触发**: 用户要求写代码、重构、修 bug
- **能力**: 多语言代码生成、代码审查、重构建议、测试生成
- **实现**: 组合 grep/glob/edit_file/shell 工具 + 代码规范

#### 3. `loop-engineering` — 循环工程
- **触发**: 用户要求迭代优化某个产出
- **能力**: 自动执行 "生成→评估→改进" 循环，每轮评估产出质量并改进
- **实现**: Agent coordinator 中增加 loop 模式，支持 max_iterations + 质量评估函数

#### 4. `first-principles` — 第一性原理
- **触发**: 用户要求分析复杂问题、做架构决策
- **能力**: 将问题分解为基本事实，从零推理出解决方案
- **实现**: 特定 system prompt + 结构化输出模板

#### 5. `adversarial-review` — 对抗式审查
- **触发**: 用户要求审查代码/方案/文档
- **能力**: 扮演批评者角色，主动寻找漏洞、边界情况、风险
- **实现**: 子 Agent 模式，独立审查后汇总

---

## 四、项目后置产出 (Post-Project Artifacts)

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
- 存入 DeepNova 的长期记忆系统

---

## 五、自动记忆系统 (学习 Hermes Agent)

### Hermes Agent 核心机制（研究总结）

**闭环学习系统 (Closed Learning Loop):**
```
执行任务 → 评估结果 → 提炼技能 → 存入记忆 → 下次调用
                ↑                                      │
                └────────── 反馈优化 ←──────────────────┘
```

**四层记忆架构:**
1. **短期工作记忆** — 当前对话上下文
2. **中期任务记忆** — 当前项目的会话历史
3. **长期技能记忆** — 从经验中提炼的可复用 Skill
4. **永久用户画像** — 用户偏好模型（基于 Honcho 辩证建模）

**关键特性:**
- 复杂任务（5+ 工具调用）完成后自动创建 Skill
- Skill 在使用中持续优化
- FTS5 全文检索实现毫秒级跨会话召回
- Agent 主动 "推自己一把" 持久化重要信息
- 兼容 agentskills.io 开放标准

### DeepNova 适配方案

```
┌─────────────────────────────────────────────────┐
│              DeepNova Memory Architecture         │
├─────────────────────────────────────────────────┤
│                                                  │
│  ┌──────────┐  ┌──────────┐  ┌──────────────┐ │
│  │ 短期记忆  │  │ 任务记忆  │  │ 技能库(Skills) │ │
│  │ Context  │  │ Session  │  │ ~/.deepnova/  │ │
│  │ Window   │  │ History  │  │   skills/     │ │
│  └──────────┘  └──────────┘  └──────────────┘ │
│        │              │              │          │
│        └──────────────┴──────────────┘          │
│                      │                           │
│              ┌──────────────┐                   │
│              │  用户画像     │                   │
│              │  USER.md     │                   │
│              │  + profiles/  │                  │
│              └──────────────┘                   │
│                      │                           │
│              ┌──────────────┐                   │
│              │  FTS5 检索    │                   │
│              │  (SQLite)    │                   │
│              └──────────────┘                   │
│                                                  │
└─────────────────────────────────────────────────┘
```

**实现步骤:**
1. 在 `deepnova-core/src/memory/` 下实现四层记忆
2. 任务完成后自动评估是否值得创建 Skill
3. Skill 格式: Markdown + YAML frontmatter (兼容 agentskills.io)
4. 使用 SQLite FTS5 做全文检索
5. 每次对话开始时自动检索相关记忆和 Skill

---

## 六、Agent 规范哲学

### 问题诊断
> "很多人做不出好的项目，是因为 agent 没有一个好的规范"

**根因分析:**
1. **Agent 没有工作规范** — 大多数 Agent 是"万能的聊天机器人"，但没有定义什么是"好的工作产出"
2. **缺乏质量门控** — Agent 生成代码后没有自动审查环节
3. **经验不积累** — 每次 from scratch，不学习
4. **没有项目闭环** — 做完就结束，不总结、不文档化、不沉淀

### DeepNova 的规范设计

#### DeepNova Agent 工作规范 (DNA Spec)

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
  ├── 对抗式审查 (adversarial-review skill)
  └── 与成功标准对比

Phase 5: 沉淀 (Distill) ← 这是大多数 Agent 缺失的
  ├── 提炼可复用的 Skill
  ├── 更新记忆
  ├── 询问是否生成 Wiki/知识卡片
  └── 记录项目经验
```

**核心原则:**
- **可验证性**: 每个产出必须有验证标准
- **可追溯性**: 每个决策都有上下文
- **可复用性**: 每次工作的经验都要沉淀
- **诚实性**: 不确定就说不确定，不编造

---

## 七、执行优先级

| 优先级 | 任务 | 预计工作量 |
|--------|------|-----------|
| P0 | 重命名 DeepNova → DeepNova | 机械替换，1次提交 |
| P1 | 实现自动记忆系统 | 新模块，2-3个文件 |
| P2 | 内置 5 个 Skill | Skill 定义文件 |
| P3 | 项目后置产出 | Wiki/卡片/记忆生成 |
| P4 | Agent 工作规范 | 设计文档 + system prompt |
