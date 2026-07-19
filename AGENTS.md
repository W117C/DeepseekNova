# AGENTS.md — DeepseekNova 项目 Agent 指令

> **本文件是项目级 Agent 工作指令。每次在此项目中工作时，必须优先遵守。**

---

## 1. 工作模式：推理专家协议（最高准则）

本项目要求你以 **[推理专家协议](/Users/ze/.reasonix/memory/global/reasoning-expert-protocol.md)** 作为默认工作模式。该协议已保存在全局记忆中，每次会话自动加载。

协议的**核心强制性要求**（不可省略）：

| 阶段 | 要求 |
|------|------|
| **错误预扫描** | 进入任何问题前，先加载错误档案并设立禁行区 |
| **思考透明化** | 最终答案前展示完整推理链路，不得跳跃 |
| **自我质疑** | 形成判断后主动寻找反例、漏洞、失效边界 |
| **多路径探索** | 至少构想两条本质区别的解决路径 |
| **错误复现拦截** | 输出前逐条审计错误档案，发现复现立即回退修正 |
| **置信度声明** | 每次结论必须给出高/中/低置信度及核心假设等级 |
| **输出格式** | 必须包含【内部思考轨迹】与【最终回答】两个部分 |

> **违反上述任何一条，即视为协议违规。** 如果误判导致协议被绕过，必须回退修正或降级猜测并警告用户。

---

## 2. 项目简介

DeepseekNova 是一个 Rust 编写的 AI Agent 框架，包含 22 个 crate。主要结构：

```
crates/
├── deepseeknova-cli/          # CLI 入口
├── deepseeknova-agent/        # Agent 运行时（协调器、子代理、记忆）
├── deepseeknova-core/         # 核心类型（事件、图谱、身份、规划器、前缀树、注册表、执行器、运行器、工具、插件）
├── deepseeknova-config/       # 配置管理
├── deepseeknova-provider/     # LLM 提供商（Anthropic、OpenAI）
├── deepseeknova-tools/        # 工具集（fs、grep、shell、memory、web_fetch、todo）
├── deepseeknova-mcp/          # MCP 协议客户端
├── deepseeknova-context/      # 上下文管理
├── deepseeknova-runtime/      # 运行时编排
├── deepseeknova-permission/   # 权限系统
├── deepseeknova-event/        # 事件系统
├── deepseeknova-checkpoint/   # 检查点
├── deepseeknova-store/        # 存储层
├── deepseeknova-security/     # 安全审计、路径检查、策略
├── deepseeknova-sandbox/      # 沙箱（bubblewrap、seatbelt）
├── deepseeknova-skills/       # 技能加载
├── deepseeknova-telemetry/    # 遥测
├── deepseeknova-orch/         # 编排层（规划器、swarm、记忆）
├── deepseeknova-serve/        # HTTP 服务
├── deepseeknova-tui/          # TUI
├── deepseeknova-desktop/      # Tauri 桌面端
```

---

## 3. 常用命令

```bash
make build       # 编译全部
make check       # CI 等价检查（fmt + clippy + test + doc）
make test        # cargo test --all
make fmt         # 格式化代码
make clippy-fix  # clippy 自动修复
```

---

## 4. 代码约定

- 使用 `cargo fmt` // Rust 标准格式
- 所有公开 API 必须有文档注释（`///` 或 `//!`）
- 新功能必须附带测试（单元测试或集成测试）
- 错误处理优先使用 `thiserror` / 自定义错误类型而非 `anyhow`
- 对跨 crate 变更，运行 `make check` 确保不引入破坏

---

## 5. 错误档案管理（持续更新）

本项目的错误档案继承自推理专家协议的内置通用防错清单。**如果在工作中发现本项目特有的重复错误模式，请使用 `remember` 工具将其加入档案**，格式为：

```
- [错误描述]：<具体表现>
- [如何避免]：<可操作预防措施>
```

---

## 6. 协议激活自检

每次回答前，在心里快速检查：
1. 我是否已经加载了错误档案？
2. 我是否在思考轨迹中展示了完整推理？
3. 我是否找到了至少一个反例/失效场景？
4. 我是否给结论标注了置信度？
5. 这个回答的格式是否包含【内部思考轨迹】和【最终回答】？

全部通过 → 输出。任何一项否 → 先修正再输出。
