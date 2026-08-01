/**
 * i18n/index.ts — 轻量国际化系统
 * 支持中文 (zh-CN) 和英文 (en-US) 切换
 */

import { createContext, useContext } from "react";

export type Locale = "zh-CN" | "en-US";

export const LOCALES: { id: Locale; label: string }[] = [
  { id: "zh-CN", label: "简体中文" },
  { id: "en-US", label: "English" },
];

// ── 翻译字典 ────────────────────────────────────────────────

const zh: Record<string, string> = {
  // App
  "app.name": "DeepseekNova",
  "app.ready": "就绪",
  "app.running": "运行中",
  "app.send": "发送",
  "app.stop": "停止",
  "app.cancel": "取消",
  "app.confirm": "确认",
  "app.close": "关闭",
  "app.save": "保存",
  "app.search": "搜索",
  "app.loading": "加载中…",

  // Composer
  "composer.placeholder": "输入消息，按 Enter 发送，Shift+Enter 换行，/ 触发命令…",
  "composer.placeholderRunning": "Agent 运行中…",
  "composer.thinking": "Agent 思考中…",

  // Sidebar
  "sidebar.search": "搜索会话…",
  "sidebar.today": "今天",
  "sidebar.yesterday": "昨天",
  "sidebar.earlier": "更早",
  "sidebar.newChat": "新会话",
  "sidebar.sessions": "会话",
  "sidebar.skills": "技能库",

  // Right Panel
  "panel.files": "文件",
  "panel.knowledge": "知识库",
  "panel.memory": "记忆",
  "panel.trace": "Trace",
  "panel.modified": "修改",
  "panel.read": "读取",
  "panel.empty": "暂无数据",
  "panel.emptyKnowledge": "暂无知识卡片，使用 Agent 后自动提取",
  "panel.emptyMemory": "暂无记忆，使用 Agent 后自动积累",
  "panel.emptyTrace": "发送消息后，执行轨迹将显示在这里",

  // Trace
  "trace.steps": "步骤",
  "trace.tools": "工具",
  "trace.duration": "耗时",
  "trace.thinking": "推理",
  "trace.textGen": "文本生成",
  "trace.toolReturn": "工具返回",
  "trace.done": "完成",
  "trace.error": "错误",
  "trace.expand": "展开",
  "trace.collapse": "收起",
  "trace.chars": "字符",

  // Settings
  "settings.title": "设置",
  "settings.general": "通用",
  "settings.appearance": "外观",
  "settings.execution": "执行",
  "settings.shortcuts": "快捷键",
  "settings.sandbox": "沙箱",
  "settings.network": "网络",
  "settings.permissions": "权限",
  "settings.hooks": "钩子",
  "settings.mcp": "插件 (MCP)",
  "settings.subagents": "子智能体",
  "settings.skills": "技能",
  "settings.diagnostics": "诊断",
  "settings.billing": "账单",
  "settings.about": "关于",
  "settings.group.basic": "基础",
  "settings.group.security": "安全",
  "settings.group.extension": "扩展",
  "settings.group.tools": "工具",

  // General Settings
  "general.apiKey": "API Key",
  "general.apiKeyDesc": "DeepSeek API 密钥，在 platform.deepseek.com 申请",
  "general.baseUrl": "Base URL",
  "general.baseUrlDesc": "API 基础地址，可替换为代理",
  "general.model": "默认模型",
  "general.modelDesc": "新会话默认使用的模型",
  "general.language": "界面语言",
  "general.languageDesc": "切换前端显示语言",
  "general.fontSize": "字体大小",
  "general.fontFamily": "字体家族",
  "general.autoSave": "自动保存会话",
  "general.autoSaveDesc": "会话内容自动保存到磁盘",
  "general.tabRestore": "标签页恢复",
  "general.tabRestoreDesc": "重启后自动恢复所有标签和滚动位置",

  // Appearance
  "appearance.theme": "主题",
  "appearance.themeDesc": "选择界面主题模式",
  "appearance.light": "浅色",
  "appearance.dark": "深色",
  "appearance.system": "跟随系统",
  "appearance.accent": "强调色",
  "appearance.accentDesc": "界面主色调",
  "appearance.compact": "紧凑模式",
  "appearance.compactDesc": "减少间距，显示更多内容",
  "appearance.animation": "动画效果",
  "appearance.animationDesc": "界面过渡动画",
  "appearance.stream": "流式输出",
  "appearance.streamDesc": "AI 回复实时流式显示",

  // Mode（旧三档已废弃，四档 key 见下方 mockup 新增块）

  // Welcome
  "welcome.title": "欢迎使用 DeepseekNova",
  "welcome.subtitle": "DeepSeek-V4 原生 AI 编程助手",
  "welcome.hint": "输入消息开始对话，或按 / 查看可用命令",
  "welcome.quickStart": "快速开始",
  "welcome.quickStartHint": "输入消息开始对话，或使用 / 触发 Slash 命令",
  "welcome.feat.memory": "自动记忆",
  "welcome.feat.memoryDesc": "跨会话保持上下文，自动提取技能",
  "welcome.feat.skills": "6 个内置技能",
  "welcome.feat.skillsDesc": "前端开发、代码审查、第一性原理等",
  "welcome.feat.sandbox": "沙箱执行",
  "welcome.feat.sandboxDesc": "安全隔离，审批模式可控",
  "welcome.feat.token": "Token 追踪",
  "welcome.feat.tokenDesc": "缓存命中率、成本实时显示",

  // TitleBar
  "titlebar.expand": "展开",
  "titlebar.collapse": "折叠",
  "titlebar.commandPalette": "命令面板",
  "titlebar.command": "命令",
  "titlebar.expandPanel": "展开面板",
  "titlebar.collapsePanel": "折叠面板",
  "titlebar.mainSession": "主会话",
  "titlebar.newSession": "新会话",

  // ControlBar
  "control.textMode": "文字模式",
  "control.iconMode": "图标模式",
  "control.settings": "设置",

  // Transcript
  "transcript.thinking": "Agent 思考中…",
  "transcript.you": "你",

  // ToolCard
  "tool.args": "参数",
  "tool.result": "结果",
  "tool.running": "执行中…",
  "tool.done": "完成",
  "tool.truncated": "已截断",

  // ReasoningCard
  "reasoning.title": "推理过程",
  "reasoning.inProgress": "进行中…",

  // ApprovalCard
  "approval.title": "需要审批",
  "approval.approve": "允许",

  // CommandPalette
  "palette.placeholder": "输入命令…",
  "palette.noResults": "无匹配命令",

  // ContextPanel / WorkspacePanel
  "workspace.title": "工作区",
  "workspace.files": "文件",
  "workspace.noFiles": "暂无文件变更",

  // MemoryPanel
  "memoryPanel.title": "记忆",
  "memoryPanel.add": "添加记忆",
  "memoryPanel.empty": "暂无记忆",

  // TodoPanel
  "todo.title": "待办",
  "todo.add": "添加待办",
  "todo.placeholder": "输入待办事项…",
  "todo.empty": "暂无待办",

  // KnowledgeBaseModal
  "kb.title": "知识库",
  "kb.wiki": "Wiki",
  "kb.cards": "知识卡片",
  "kb.addCard": "手动添加知识卡片",

  // MarkdownRenderer / CodeViewer
  "code.copy": "复制",
  "code.copied": "已复制",
  "code.lines": "行",

  // DiffView
  "diff.added": "新增",
  "diff.removed": "删除",

  // Execution Settings
  "exec.maxSteps": "最大步数",
  "exec.maxStepsDesc": "Agent 单次运行最大工具调用轮数",
  "exec.timeout": "超时时间",
  "exec.timeoutDesc": "单次工具执行超时（秒）",
  "exec.autoApprove": "自动审批",
  "exec.autoApproveDesc": "YOLO 模式下自动批准所有工具调用",
  "exec.parallel": "并行执行",
  "exec.parallelDesc": "允许并行执行独立工具调用",

  // Sandbox Settings
  "sandbox.enabled": "启用沙箱",
  "sandbox.enabledDesc": "在隔离环境中执行 Shell 命令",
  "sandbox.network": "允许网络",
  "sandbox.networkDesc": "沙箱内允许网络访问",
  "sandbox.paths": "允许路径",
  "sandbox.pathsDesc": "沙箱内可访问的额外路径",

  // Network Settings
  "network.proxy": "代理",
  "network.proxyDesc": "HTTP/HTTPS 代理地址",
  "network.timeout": "请求超时",
  "network.timeoutDesc": "API 请求超时时间（秒）",
  "network.retries": "重试次数",
  "network.retriesDesc": "失败后自动重试次数",
  "network.ssl": "SSL 验证",
  "network.sslDesc": "验证服务器 SSL 证书",

  // Permissions Settings
  "perm.title": "权限规则",
  "perm.desc": "当前生效的权限配置",
  "perm.rules": "条规则",

  // Hooks Settings
  "hooks.title": "事件钩子",
  "hooks.desc": "在特定事件触发时执行自定义命令",
  "hooks.add": "添加新钩子",
  "hooks.event": "事件",
  "hooks.command": "命令",

  // MCP Settings
  "mcp.title": "MCP 服务器",
  "mcp.desc": "管理 Model Context Protocol 服务器连接",
  "mcp.add": "添加服务器",
  "mcp.name": "名称",
  "mcp.command": "命令",
  "mcp.status": "状态",
  "mcp.connected": "已连接",
  "mcp.disconnected": "未连接",

  // SubAgents Settings
  "subagents.title": "子智能体",
  "subagents.desc": "多智能体编排配置",
  "subagents.tasks": "次任务",

  // Skills Settings
  "skills.title": "已加载技能",
  "skills.desc": "当前可用的 Agent 技能",
  "skills.tools": "允许工具",
  "skills.empty": "暂无已加载技能",
  "skills.emptyHint": "在 .deepseeknova/skills/ 创建",

  // Diagnostics Settings
  "diag.title": "系统诊断",
  "diag.desc": "系统体检 — 检查所有组件状态",
  "diag.run": "运行诊断",
  "diag.pass": "通过",
  "diag.fail": "失败",
  "diag.warn": "警告",
  "diag.skip": "跳过",

  // Billing Settings
  "billing.title": "用量统计",
  "billing.desc": "Token 用量与成本估算",
  "billing.total": "总计",
  "billing.input": "输入",
  "billing.output": "输出",
  "billing.cache": "缓存命中",
  "billing.cost": "估算成本",
  "billing.runs": "运行次数",

  // About Settings
  "about.title": "关于",
  "about.version": "版本",
  "about.framework": "框架",
  "about.runtime": "运行时",
  "about.license": "许可证",

  // ── Mockup 定稿新增组件 ──
  // 模式切换（四档）
  "mode.agent": "代理",
  "mode.chat": "对话",
  "mode.plan": "规划",
  "mode.review": "审查",
  "mode.agent.title": "全自动执行 — 工具经权限网关审批",
  "mode.chat.title": "纯对话 — 不调用工具",
  "mode.plan.title": "先产出计划再执行",
  "mode.review.title": "代码审查模式",
  // AI 计时指示
  "phase.thinking": "思考中",
  "phase.reasoning": "推理中",
  "phase.replying": "回复中",
  "phase.done": "已完成",
  "phase.stopped": "已停止",
  "phase.elapsed": "用时",
  // Diff 审查面板
  "diff.source": "源代码",
  "diff.modified": "修改后",
  "diff.accept": "接受",
  "diff.reject": "拒绝",
  "diff.accepted": "✓ 已接受",
  "diff.rejected": "✕ 已拒绝",
  "diff.loading": "加载 diff…",
  "diff.empty": "该文件无差异（可能是新增未跟踪文件）",
  "diff.truncated": "已截断 2000 行",
  "diff.pendingHint": "接受写入工作区，拒绝回滚改动",
  "diff.pendingLeft": "待审查 · 剩",
  "diff.pendingFiles": "个文件",
  // 任务抽屉
  "drawer.task": "任务",
  "drawer.context": "上下文",
  "drawer.progress": "进度",
  "drawer.files": "文件变更",
  "drawer.worktree": "工作树",
  "drawer.subagents": "子智能体",
  "drawer.orch": "编排进度",
  "drawer.close": "关闭",
  // 输入区
  "composer.attach": "添加文件",
  "composer.voice": "语音输入（即将支持）",
  "composer.voiceStop": "停止录音（语音输入即将支持）",
  "chip.review": "审查",
  // 长对话导航
  "nav.outline": "消息大纲",
  "nav.backToTop": "回到顶部",
  "nav.noMessages": "暂无消息",
  // 状态栏
  "status.tools": "工具",
  "status.duration": "时长",
  "status.reasoning": "推理",
  "status.speed": "速度",
  "status.ttft": "首字",
  "status.cache": "缓存",
  "status.context": "上下文",
  "status.cost": "费用",
  // 设置中心（6 组 22 分区）
  "sgroup.app": "应用",
  "sgroup.modelExec": "模型与执行",
  "sgroup.capability": "能力",
  "sgroup.security": "安全",
  "sgroup.data": "数据",
  "sgroup.system": "系统",
  "ssec.general": "通用",
  "ssec.role": "角色",
  "ssec.appearance": "外观",
  "ssec.shortcuts": "快捷键",
  "ssec.models": "模型",
  "ssec.reasoning": "推理参数",
  "ssec.execution": "执行",
  "ssec.tools": "工具",
  "ssec.mcp": "MCP",
  "ssec.skills": "技能",
  "ssec.subagents": "子代理",
  "ssec.sandbox": "沙箱",
  "ssec.permissions": "权限",
  "ssec.network": "网络",
  "ssec.hooks": "钩子",
  "ssec.memory": "记忆",
  "ssec.knowledge": "知识库",
  "ssec.logs": "日志",
  "ssec.triggers": "触发",
  "ssec.diagnostics": "诊断",
  "ssec.billing": "账单",
  "ssec.update": "更新",
  // 审批与错误
  "approval.waiting": "等待审批",
  "approval.hint": "权限网关判定为 Ask。本会话内批准后相同操作不再询问。",
  "approval.allow": "允许",
  "approval.deny": "拒绝",
  "error.dismiss": "忽略",
};

const en: Record<string, string> = {
  // App
  "app.name": "DeepseekNova",
  "app.ready": "Ready",
  "app.running": "Running",
  "app.send": "Send",
  "app.stop": "Stop",
  "app.cancel": "Cancel",
  "app.confirm": "Confirm",
  "app.close": "Close",
  "app.save": "Save",
  "app.search": "Search",
  "app.loading": "Loading…",

  // Composer
  "composer.placeholder": "Type a message, Enter to send, Shift+Enter for newline, / for commands…",
  "composer.placeholderRunning": "Agent is running…",
  "composer.thinking": "Agent is thinking…",

  // Sidebar
  "sidebar.search": "Search sessions…",
  "sidebar.today": "Today",
  "sidebar.yesterday": "Yesterday",
  "sidebar.earlier": "Earlier",
  "sidebar.newChat": "New Chat",
  "sidebar.sessions": "Sessions",
  "sidebar.skills": "Skills",

  // Right Panel
  "panel.files": "Files",
  "panel.knowledge": "Knowledge",
  "panel.memory": "Memory",
  "panel.trace": "Trace",
  "panel.modified": "Modified",
  "panel.read": "Read",
  "panel.empty": "No data",
  "panel.emptyKnowledge": "No knowledge cards yet. They are extracted automatically after using the Agent.",
  "panel.emptyMemory": "No memories yet. They accumulate automatically after using the Agent.",
  "panel.emptyTrace": "Send a message to see the execution trace here.",

  // Trace
  "trace.steps": "Steps",
  "trace.tools": "Tools",
  "trace.duration": "Duration",
  "trace.thinking": "Reasoning",
  "trace.textGen": "Text generation",
  "trace.toolReturn": "Tool result",
  "trace.done": "Done",
  "trace.error": "Error",
  "trace.expand": "Expand",
  "trace.collapse": "Collapse",
  "trace.chars": "chars",

  // Settings
  "settings.title": "Settings",
  "settings.general": "General",
  "settings.appearance": "Appearance",
  "settings.execution": "Execution",
  "settings.shortcuts": "Shortcuts",
  "settings.sandbox": "Sandbox",
  "settings.network": "Network",
  "settings.permissions": "Permissions",
  "settings.hooks": "Hooks",
  "settings.mcp": "Plugins (MCP)",
  "settings.subagents": "Sub-Agents",
  "settings.skills": "Skills",
  "settings.diagnostics": "Diagnostics",
  "settings.billing": "Billing",
  "settings.about": "About",
  "settings.group.basic": "Basic",
  "settings.group.security": "Security",
  "settings.group.extension": "Extension",
  "settings.group.tools": "Tools",

  // General Settings
  "general.apiKey": "API Key",
  "general.apiKeyDesc": "DeepSeek API key, apply at platform.deepseek.com",
  "general.baseUrl": "Base URL",
  "general.baseUrlDesc": "API base URL, can be replaced with a proxy",
  "general.model": "Default Model",
  "general.modelDesc": "Model used by default for new sessions",
  "general.language": "Language",
  "general.languageDesc": "Switch the frontend display language",
  "general.fontSize": "Font Size",
  "general.fontFamily": "Font Family",
  "general.autoSave": "Auto-save Sessions",
  "general.autoSaveDesc": "Automatically save session content to disk",
  "general.tabRestore": "Tab Restore",
  "general.tabRestoreDesc": "Restore all tabs and scroll positions after restart",

  // Appearance
  "appearance.theme": "Theme",
  "appearance.themeDesc": "Select the interface theme mode",
  "appearance.light": "Light",
  "appearance.dark": "Dark",
  "appearance.system": "System",
  "appearance.accent": "Accent Color",
  "appearance.accentDesc": "Interface primary color",
  "appearance.compact": "Compact Mode",
  "appearance.compactDesc": "Reduce spacing, show more content",
  "appearance.animation": "Animations",
  "appearance.animationDesc": "Interface transition animations",
  "appearance.stream": "Streaming Output",
  "appearance.streamDesc": "Display AI replies in real-time streaming",

  // Mode (legacy 3-mode removed; 4-mode keys live in the mockup block below)

  // Welcome
  "welcome.title": "Welcome to DeepseekNova",
  "welcome.subtitle": "DeepSeek-V4 Native AI Coding Assistant",
  "welcome.hint": "Type a message to start, or press / to see available commands",
  "welcome.quickStart": "Quick Start",
  "welcome.quickStartHint": "Type a message to start, or use / to trigger Slash commands",
  "welcome.feat.memory": "Auto Memory",
  "welcome.feat.memoryDesc": "Cross-session context retention, automatic skill extraction",
  "welcome.feat.skills": "6 Built-in Skills",
  "welcome.feat.skillsDesc": "Frontend dev, code review, first principles, etc.",
  "welcome.feat.sandbox": "Sandbox Execution",
  "welcome.feat.sandboxDesc": "Secure isolation, approval mode control",
  "welcome.feat.token": "Token Tracking",
  "welcome.feat.tokenDesc": "Cache hit rate, real-time cost display",

  // TitleBar
  "titlebar.expand": "Expand",
  "titlebar.collapse": "Collapse",
  "titlebar.commandPalette": "Command Palette",
  "titlebar.command": "Commands",
  "titlebar.expandPanel": "Expand Panel",
  "titlebar.collapsePanel": "Collapse Panel",
  "titlebar.mainSession": "Main Session",
  "titlebar.newSession": "New Session",

  // ControlBar
  "control.textMode": "Text Mode",
  "control.iconMode": "Icon Mode",
  "control.settings": "Settings",

  // Transcript
  "transcript.thinking": "Agent is thinking…",
  "transcript.you": "You",

  // ToolCard
  "tool.args": "Args",
  "tool.result": "Result",
  "tool.running": "Running…",
  "tool.done": "Done",
  "tool.truncated": "truncated",

  // ReasoningCard
  "reasoning.title": "Reasoning",
  "reasoning.inProgress": "In progress…",

  // ApprovalCard
  "approval.title": "Approval Required",
  "approval.approve": "Allow",

  // CommandPalette
  "palette.placeholder": "Type a command…",
  "palette.noResults": "No matching commands",

  // ContextPanel / WorkspacePanel
  "workspace.title": "Workspace",
  "workspace.files": "Files",
  "workspace.noFiles": "No file changes",

  // MemoryPanel
  "memoryPanel.title": "Memory",
  "memoryPanel.add": "Add Memory",
  "memoryPanel.empty": "No memories yet",

  // TodoPanel
  "todo.title": "Todo",
  "todo.add": "Add Todo",
  "todo.placeholder": "Enter a todo item…",
  "todo.empty": "No todos yet",

  // KnowledgeBaseModal
  "kb.title": "Knowledge Base",
  "kb.wiki": "Wiki",
  "kb.cards": "Knowledge Cards",
  "kb.addCard": "Add knowledge card manually",

  // MarkdownRenderer / CodeViewer
  "code.copy": "Copy",
  "code.copied": "Copied",
  "code.lines": "lines",

  // DiffView
  "diff.added": "Added",
  "diff.removed": "Removed",

  // Execution Settings
  "exec.maxSteps": "Max Steps",
  "exec.maxStepsDesc": "Maximum tool call rounds per agent run",
  "exec.timeout": "Timeout",
  "exec.timeoutDesc": "Single tool execution timeout (seconds)",
  "exec.autoApprove": "Auto Approve",
  "exec.autoApproveDesc": "Automatically approve all tool calls in YOLO mode",
  "exec.parallel": "Parallel Execution",
  "exec.parallelDesc": "Allow parallel execution of independent tool calls",

  // Sandbox Settings
  "sandbox.enabled": "Enable Sandbox",
  "sandbox.enabledDesc": "Execute shell commands in an isolated environment",
  "sandbox.network": "Allow Network",
  "sandbox.networkDesc": "Allow network access inside sandbox",
  "sandbox.paths": "Allowed Paths",
  "sandbox.pathsDesc": "Additional paths accessible inside sandbox",

  // Network Settings
  "network.proxy": "Proxy",
  "network.proxyDesc": "HTTP/HTTPS proxy address",
  "network.timeout": "Request Timeout",
  "network.timeoutDesc": "API request timeout (seconds)",
  "network.retries": "Retries",
  "network.retriesDesc": "Auto retry count after failure",
  "network.ssl": "SSL Verification",
  "network.sslDesc": "Verify server SSL certificate",

  // Permissions Settings
  "perm.title": "Permission Rules",
  "perm.desc": "Currently active permission configuration",
  "perm.rules": "rules",

  // Hooks Settings
  "hooks.title": "Event Hooks",
  "hooks.desc": "Execute custom commands on specific events",
  "hooks.add": "Add New Hook",
  "hooks.event": "Event",
  "hooks.command": "Command",

  // MCP Settings
  "mcp.title": "MCP Servers",
  "mcp.desc": "Manage Model Context Protocol server connections",
  "mcp.add": "Add Server",
  "mcp.name": "Name",
  "mcp.command": "Command",
  "mcp.status": "Status",
  "mcp.connected": "Connected",
  "mcp.disconnected": "Disconnected",

  // SubAgents Settings
  "subagents.title": "Sub-Agents",
  "subagents.desc": "Multi-agent orchestration configuration",
  "subagents.tasks": "tasks",

  // Skills Settings
  "skills.title": "Loaded Skills",
  "skills.desc": "Currently available agent skills",
  "skills.tools": "Allowed Tools",
  "skills.empty": "No skills loaded",
  "skills.emptyHint": "Create in .deepseeknova/skills/",

  // Diagnostics Settings
  "diag.title": "Diagnostics",
  "diag.desc": "System health check — inspect all component states",
  "diag.run": "Run Diagnostics",
  "diag.pass": "Pass",
  "diag.fail": "Fail",
  "diag.warn": "Warning",
  "diag.skip": "Skip",

  // Billing Settings
  "billing.title": "Usage Statistics",
  "billing.desc": "Token usage and cost estimation",
  "billing.total": "Total",
  "billing.input": "Input",
  "billing.output": "Output",
  "billing.cache": "Cache Hit",
  "billing.cost": "Est. Cost",
  "billing.runs": "Runs",

  // About Settings
  "about.title": "About",
  "about.version": "Version",
  "about.framework": "Framework",
  "about.runtime": "Runtime",
  "about.license": "License",

  // ── Mockup redesign components ──
  "mode.agent": "Agent",
  "mode.chat": "Chat",
  "mode.plan": "Plan",
  "mode.review": "Review",
  "mode.agent.title": "Autonomous execution — tools gated by permission gate",
  "mode.chat.title": "Chat only — no tool calls",
  "mode.plan.title": "Produce a plan before executing",
  "mode.review.title": "Code review mode",
  "phase.thinking": "Thinking",
  "phase.reasoning": "Reasoning",
  "phase.replying": "Replying",
  "phase.done": "Done",
  "phase.stopped": "Stopped",
  "phase.elapsed": "took",
  "diff.source": "Source",
  "diff.modified": "Modified",
  "diff.accept": "Accept",
  "diff.reject": "Reject",
  "diff.accepted": "✓ Accepted",
  "diff.rejected": "✕ Rejected",
  "diff.loading": "Loading diff…",
  "diff.empty": "No diff for this file (possibly untracked)",
  "diff.truncated": "truncated at 2000 lines",
  "diff.pendingHint": "accept keeps the change, reject rolls it back",
  "diff.pendingLeft": "Pending ·",
  "diff.pendingFiles": "file(s) left",
  "drawer.task": "Tasks",
  "drawer.context": "Context",
  "drawer.progress": "Progress",
  "drawer.files": "File Changes",
  "drawer.worktree": "Worktrees",
  "drawer.subagents": "Sub-Agents",
  "drawer.orch": "Orchestration",
  "drawer.close": "Close",
  "composer.attach": "Attach files",
  "composer.voice": "Voice input (coming soon)",
  "composer.voiceStop": "Stop recording (voice input coming soon)",
  "chip.review": "Review",
  "nav.outline": "Message outline",
  "nav.backToTop": "Back to top",
  "nav.noMessages": "No messages",
  "status.tools": "Tools",
  "status.duration": "Time",
  "status.reasoning": "Reason",
  "status.speed": "Speed",
  "status.ttft": "TTFT",
  "status.cache": "Cache",
  "status.context": "Context",
  "status.cost": "Cost",
  "sgroup.app": "Application",
  "sgroup.modelExec": "Model & Execution",
  "sgroup.capability": "Capabilities",
  "sgroup.security": "Security",
  "sgroup.data": "Data",
  "sgroup.system": "System",
  "ssec.general": "General",
  "ssec.role": "Persona",
  "ssec.appearance": "Appearance",
  "ssec.shortcuts": "Shortcuts",
  "ssec.models": "Models",
  "ssec.reasoning": "Reasoning",
  "ssec.execution": "Execution",
  "ssec.tools": "Tools",
  "ssec.mcp": "MCP",
  "ssec.skills": "Skills",
  "ssec.subagents": "Sub-Agents",
  "ssec.sandbox": "Sandbox",
  "ssec.permissions": "Permissions",
  "ssec.network": "Network",
  "ssec.hooks": "Hooks",
  "ssec.memory": "Memory",
  "ssec.knowledge": "Knowledge",
  "ssec.logs": "Logs",
  "ssec.triggers": "Triggers",
  "ssec.diagnostics": "Diagnostics",
  "ssec.billing": "Billing",
  "ssec.update": "Updates",
  "approval.waiting": "Approval required",
  "approval.hint": "Permission gate decided Ask. Approving caches the decision for this session.",
  "approval.allow": "Allow",
  "approval.deny": "Deny",
  "error.dismiss": "Dismiss",
};

// ── 字典映射 ────────────────────────────────────────────────

const dictionaries: Record<Locale, Record<string, string>> = {
  "zh-CN": zh,
  "en-US": en,
};

// ── 翻译函数 ────────────────────────────────────────────────

export function translate(locale: Locale, key: string, params?: Record<string, string | number>): string {
  const dict = dictionaries[locale] || zh;
  let text = dict[key] || zh[key] || key;
  if (params) {
    for (const [k, v] of Object.entries(params)) {
      text = text.replace(`{${k}}`, String(v));
    }
  }
  return text;
}

// ── React Context ───────────────────────────────────────────

export interface I18nContextValue {
  locale: Locale;
  setLocale: (l: Locale) => void;
  t: (key: string, params?: Record<string, string | number>) => string;
}

export const I18nContext = createContext<I18nContextValue>({
  locale: "zh-CN",
  setLocale: () => {},
  t: (key) => key,
});

export function useI18n() {
  return useContext(I18nContext);
}
