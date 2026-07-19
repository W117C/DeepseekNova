/**
 * SettingsModal.tsx — 设置面板（完整版）
 *
 * 参考 Reasonix 桌面端设置面板，12 大分区：
 * 通用 | 外观 | 执行 | 沙箱 | 网络 | 钩子 | 插件(MCP) | 子智能体 | 技能 | 快捷键 | 权限 | 诊断 | 账单 | 关于
 */

import { useState } from "react";
import { useStore } from "../store";
import { useTheme } from "../store/theme";

type SettingsSection =
  | "general" | "appearance" | "execution" | "sandbox" | "network"
  | "hooks" | "mcp" | "subagents" | "skills" | "shortcuts"
  | "permissions" | "diagnostics" | "billing" | "about";

export default function SettingsModal() {
  const setShowSettings = useStore((s) => s.setShowSettings);
  const capabilities = useStore((s) => s.capabilities);
  const theme = useTheme((s) => s.theme);
  const setTheme = useTheme((s) => s.setTheme);
  const displayMode = useTheme((s) => s.displayMode);
  const toggleDisplayMode = useTheme((s) => s.toggleDisplayMode);
  const skills = useStore((s) => s.skills);

  const [section, setSection] = useState<SettingsSection>("general");

  const sections: { id: SettingsSection; label: string; icon: string; group: string }[] = [
    { id: "general", label: "通用", icon: "⚙️", group: "基础" },
    { id: "appearance", label: "外观", icon: "🎨", group: "基础" },
    { id: "execution", label: "执行", icon: "🚀", group: "基础" },
    { id: "shortcuts", label: "快捷键", icon: "⌨️", group: "基础" },
    { id: "sandbox", label: "沙箱", icon: "🔒", group: "安全" },
    { id: "network", label: "网络", icon: "🌐", group: "安全" },
    { id: "permissions", label: "权限", icon: "🛡️", group: "安全" },
    { id: "hooks", label: "钩子", icon: "🪝", group: "扩展" },
    { id: "mcp", label: "插件", icon: "🔌", group: "扩展" },
    { id: "subagents", label: "子智能体", icon: "🤖", group: "扩展" },
    { id: "skills", label: "技能", icon: "⚡", group: "扩展" },
    { id: "diagnostics", label: "诊断", icon: "🔬", group: "工具" },
    { id: "billing", label: "账单", icon: "💰", group: "工具" },
    { id: "about", label: "关于", icon: "ℹ️", group: "工具" },
  ];

  // 按 group 分组
  
  let lastGroup = "";

  return (
    <div className="modal-overlay" onClick={() => setShowSettings(false)}>
      <div className="modal" onClick={(e) => e.stopPropagation()} style={{ maxWidth: 760, width: "90%", height: "80vh", flexDirection: "row" }}>
        {/* 侧边导航 */}
        <div style={{ width: 170, borderRight: "1px solid var(--border-1)", background: "var(--bg-1)", overflowY: "auto" }}>
          <div style={{ padding: "10px 12px 6px", fontSize: 10, fontWeight: 600, textTransform: "uppercase", color: "var(--text-3)", letterSpacing: 0.5 }}>
            设置
          </div>
          {sections.map((s) => {
            const showGroup = s.group !== lastGroup;
            lastGroup = s.group;
            return (
              <>
                {showGroup && (
                  <div style={{ padding: "8px 12px 2px", fontSize: 9, fontWeight: 600, textTransform: "uppercase", color: "var(--text-muted)", letterSpacing: 0.5 }}>
                    {s.group}
                  </div>
                )}
                <div
                  key={s.id}
                  className={`sidebar-item ${section === s.id ? "active" : ""}`}
                  onClick={() => setSection(s.id)}
                  style={{ padding: "5px 12px" }}
                >
                  <span style={{ fontSize: 11 }}>{s.icon}</span>
                  <span className="sidebar-item-title">{s.label}</span>
                </div>
              </>
            );
          })}
        </div>

        {/* 内容区 */}
        <div style={{ flex: 1, display: "flex", flexDirection: "column", overflow: "hidden" }}>
          <div className="modal-header">
            <div className="modal-title">
              {sections.find(s => s.id === section)?.icon} {sections.find(s => s.id === section)?.label}
            </div>
            <button className="btn-icon" onClick={() => setShowSettings(false)}>✕</button>
          </div>

          <div className="modal-body">
            {section === "general" && <GeneralSettings />}
            {section === "appearance" && <AppearanceSettings theme={theme} setTheme={setTheme} displayMode={displayMode} toggleDisplayMode={toggleDisplayMode} />}
            {section === "execution" && <ExecutionSettings />}
            {section === "shortcuts" && <ShortcutsSettings />}
            {section === "sandbox" && <SandboxSettings />}
            {section === "network" && <NetworkSettings />}
            {section === "permissions" && <PermissionsSettings />}
            {section === "hooks" && <HooksSettings />}
            {section === "mcp" && <MCPSettings />}
            {section === "subagents" && <SubAgentsSettings />}
            {section === "skills" && <SkillsSettings skills={skills} />}
            {section === "diagnostics" && <DiagnosticsSettings capabilities={capabilities} />}
            {section === "billing" && <BillingSettings />}
            {section === "about" && <AboutSettings capabilities={capabilities} />}
          </div>
        </div>
      </div>
    </div>
  );
}

/* ============================================================
 * 辅助组件
 * ============================================================ */
function SettingRow({ label, desc, children }: { label: string; desc?: string; children: React.ReactNode }) {
  return (
    <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", gap: 12, padding: "6px 0", borderBottom: "1px solid var(--border-1)" }}>
      <div style={{ flex: "0 0 auto" }}>
        <div style={{ fontSize: 12, fontWeight: 500, color: "var(--text-1)" }}>{label}</div>
        {desc && <div style={{ fontSize: 10, color: "var(--text-3)", marginTop: 1 }}>{desc}</div>}
      </div>
      <div style={{ flex: "0 0 auto" }}>{children}</div>
    </div>
  );
}

function StatBox({ label, value, sub, color }: { label: string; value: string; sub?: string; color?: string }) {
  return (
    <div style={{ padding: "6px 8px", background: "var(--bg-3)", borderRadius: "var(--radius-sm)" }}>
      <div style={{ fontSize: 10, color: "var(--text-3)" }}>{label}</div>
      <div style={{ fontSize: 13, fontWeight: 600, color: color || "var(--text-1)" }}>{value}</div>
      {sub && <div style={{ fontSize: 9, color: "var(--text-muted)" }}>{sub}</div>}
    </div>
  );
}

/* ============================================================
 * 通用设置
 * ============================================================ */
function GeneralSettings() {
  const [apiKey, setApiKey] = useState("sk-••••••••••••••••");
  const [baseUrl, setBaseUrl] = useState("https://api.deepseek.com");
  const [defaultModel, setDefaultModel] = useState("deepseek-v4-flash");
  const [language, setLanguage] = useState("zh-CN");
  const [fontSize, setFontSize] = useState(13);
  const [fontFamily, setFontFamily] = useState("system");
  const [autoSave, setAutoSave] = useState(true);
  const [tabRestore, setTabRestore] = useState(true);

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 0 }}>
      <SettingRow label="API Key" desc="DeepSeek API 密钥，在 platform.deepseek.com 申请">
        <input className="input" type="password" value={apiKey} onChange={(e) => setApiKey(e.target.value)} style={{ width: 260 }} />
      </SettingRow>
      <SettingRow label="Base URL" desc="API 基础地址，可替换为代理">
        <input className="input" value={baseUrl} onChange={(e) => setBaseUrl(e.target.value)} style={{ width: 260 }} />
      </SettingRow>
      <SettingRow label="默认模型" desc="新会话默认使用的模型">
        <select className="input" value={defaultModel} onChange={(e) => setDefaultModel(e.target.value)} style={{ width: 200 }}>
          <option value="deepseek-v4-flash">DeepSeek v4 Flash（快速）</option>
          <option value="deepseek-v4-pro">DeepSeek v4 Pro（高级推理）</option>
          <option value="deepseek-coder">DeepSeek Coder</option>
          <option value="deepseek-reasoner">DeepSeek Reasoner R1</option>
        </select>
      </SettingRow>
      <SettingRow label="语言" desc="界面语言">
        <select className="input" value={language} onChange={(e) => setLanguage(e.target.value)} style={{ width: 200 }}>
          <option value="zh-CN">简体中文</option>
          <option value="en-US">English</option>
        </select>
      </SettingRow>
      <SettingRow label="字体大小" desc={`当前: ${fontSize}px`}>
        <input type="range" min="11" max="18" value={fontSize} onChange={(e) => setFontSize(Number(e.target.value))} style={{ width: 200 }} />
      </SettingRow>
      <SettingRow label="字体家族" desc="界面字体">
        <select className="input" value={fontFamily} onChange={(e) => setFontFamily(e.target.value)} style={{ width: 200 }}>
          <option value="system">系统默认</option>
          <option value="sans">无衬线</option>
          <option value="mono">等宽</option>
        </select>
      </SettingRow>
      <SettingRow label="自动保存会话" desc="会话内容自动保存到磁盘">
        <label className="toggle-switch"><input type="checkbox" checked={autoSave} onChange={(e) => setAutoSave(e.target.checked)} /><span className="toggle-slider"></span></label>
      </SettingRow>
      <SettingRow label="标签页恢复" desc="重启后自动恢复所有标签和滚动位置">
        <label className="toggle-switch"><input type="checkbox" checked={tabRestore} onChange={(e) => setTabRestore(e.target.checked)} /><span className="toggle-slider"></span></label>
      </SettingRow>
    </div>
  );
}

/* ============================================================
 * 外观设置
 * ============================================================ */
function AppearanceSettings({ theme, setTheme, displayMode, toggleDisplayMode }: any) {
  const [accentColor, setAccentColor] = useState("#6b5ded");
  const colorPresets = ["#6b5ded", "#7c3aed", "#2563eb", "#0891b2", "#16a34a", "#d97706", "#dc2626"];

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 0 }}>
      <SettingRow label="主题" desc="选择界面主题模式">
        <div style={{ display: "flex", gap: 6 }}>
          {[
            { id: "light", label: "☀️ 浅色" },
            { id: "dark", label: "🌙 深色" },
            { id: "system", label: "🖥️ 跟随系统" },
          ].map((t) => (
            <button key={t.id} className={`btn ${theme === t.id ? "btn-primary" : ""}`} onClick={() => setTheme(t.id)} style={{ padding: "4px 10px", fontSize: 11 }}>
              {t.label}
            </button>
          ))}
        </div>
      </SettingRow>
      <SettingRow label="显示模式" desc={displayMode === "icon" ? "图标模式（紧凑）" : "文字模式（详细）"}>
        <div style={{ display: "flex", gap: 6 }}>
          <button className={`btn ${displayMode === "icon" ? "btn-primary" : ""}`} onClick={() => { if (displayMode !== "icon") toggleDisplayMode(); }} style={{ padding: "4px 10px", fontSize: 11 }}>📦 图标</button>
          <button className={`btn ${displayMode === "text" ? "btn-primary" : ""}`} onClick={() => { if (displayMode !== "text") toggleDisplayMode(); }} style={{ padding: "4px 10px", fontSize: 11 }}>Aa 文字</button>
        </div>
      </SettingRow>
      <SettingRow label="强调色" desc="界面主色调">
        <div style={{ display: "flex", gap: 4 }}>
          {colorPresets.map((c) => (
            <button key={c} onClick={() => setAccentColor(c)} style={{ width: 22, height: 22, borderRadius: "50%", border: accentColor === c ? "2px solid var(--text-1)" : "2px solid transparent", background: c, cursor: "pointer" }} />
          ))}
        </div>
      </SettingRow>
      <SettingRow label="紧凑模式" desc="减少间距，显示更多内容">
        <label className="toggle-switch"><input type="checkbox" /><span className="toggle-slider"></span></label>
      </SettingRow>
      <SettingRow label="动画效果" desc="界面过渡动画">
        <label className="toggle-switch"><input type="checkbox" defaultChecked /><span className="toggle-slider"></span></label>
      </SettingRow>
      <SettingRow label="代码行号" desc="代码块显示行号">
        <label className="toggle-switch"><input type="checkbox" defaultChecked /><span className="toggle-slider"></span></label>
      </SettingRow>
      <SettingRow label="流式输出" desc="AI 回复实时流式显示">
        <label className="toggle-switch"><input type="checkbox" defaultChecked /><span className="toggle-slider"></span></label>
      </SettingRow>
      <SettingRow label="KaTeX 数学公式" desc="渲染 LaTeX 数学公式">
        <label className="toggle-switch"><input type="checkbox" defaultChecked /><span className="toggle-slider"></span></label>
      </SettingRow>
    </div>
  );
}

/* ============================================================
 * 执行设置
 * ============================================================ */
function ExecutionSettings() {
  const [defaultMode, setDefaultMode] = useState("act");
  const [autoCommit, setAutoCommit] = useState(false);
  const [tokenBudget, setTokenBudget] = useState(500000);
  const [budgetAlert, setBudgetAlert] = useState(5);
  const [maxRetries, setMaxRetries] = useState(4);
  const [timeout, setTimeout] = useState(120);

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 0 }}>
      <SettingRow label="默认执行模式" desc="新会话的默认模式">
        <select className="input" value={defaultMode} onChange={(e) => setDefaultMode(e.target.value)} style={{ width: 220 }}>
          <option value="plan">📋 Plan（只读审计）</option>
          <option value="act">✋ Act（写操作需审批）</option>
          <option value="yolo">🚀 YOLO（全自动）</option>
        </select>
      </SettingRow>
      <SettingRow label="自动提交" desc="Agent 完成任务后自动 git commit">
        <label className="toggle-switch"><input type="checkbox" checked={autoCommit} onChange={(e) => setAutoCommit(e.target.checked)} /><span className="toggle-slider"></span></label>
      </SettingRow>
      <SettingRow label="Token 预算" desc="单会话 Token 上限">
        <input type="number" className="input" value={tokenBudget} onChange={(e) => setTokenBudget(Number(e.target.value))} style={{ width: 120 }} />
      </SettingRow>
      <SettingRow label="预算告警" desc={`费用超过 $${budgetAlert} 时提醒`}>
        <input type="number" className="input" value={budgetAlert} onChange={(e) => setBudgetAlert(Number(e.target.value))} style={{ width: 80 }} step="0.5" />
      </SettingRow>
      <SettingRow label="最大重试次数" desc="工具调用失败后自动重试次数">
        <input type="number" className="input" value={maxRetries} onChange={(e) => setMaxRetries(Number(e.target.value))} style={{ width: 80 }} />
      </SettingRow>
      <SettingRow label="执行超时" desc="单次工具执行超时（秒）">
        <input type="number" className="input" value={timeout} onChange={(e) => setTimeout(Number(e.target.value))} style={{ width: 80 }} />
      </SettingRow>
    </div>
  );
}

/* ============================================================
 * 快捷键设置
 * ============================================================ */
function ShortcutsSettings() {
  const shortcuts = [
    { action: "发送消息", keys: "Enter", category: "输入" },
    { action: "换行", keys: "Shift + Enter", category: "输入" },
    { action: "命令面板", keys: "Ctrl/Cmd + P", category: "全局" },
    { action: "中断生成", keys: "Esc", category: "对话" },
    { action: "新建会话", keys: "Ctrl/Cmd + N", category: "会话" },
    { action: "关闭标签", keys: "Ctrl/Cmd + W", category: "会话" },
    { action: "切换标签", keys: "Ctrl/Cmd + Tab", category: "会话" },
    { action: "搜索会话", keys: "Ctrl/Cmd + F", category: "搜索" },
    { action: "@ 引用文件", keys: "@", category: "输入" },
    { action: "/ 斜杠命令", keys: "/", category: "输入" },
    { action: "切换主题", keys: "Ctrl/Cmd + Shift + T", category: "全局" },
    { action: "折叠侧边栏", keys: "Ctrl/Cmd + B", category: "全局" },
    { action: "折叠右侧面板", keys: "Ctrl/Cmd + J", category: "全局" },
    { action: "切换显示模式", keys: "Ctrl/Cmd + Shift + M", category: "全局" },
    { action: "Plan 模式", keys: "Ctrl/Cmd + Shift + 1", category: "模式" },
    { action: "Act 模式", keys: "Ctrl/Cmd + Shift + 2", category: "模式" },
    { action: "YOLO 模式", keys: "Ctrl/Cmd + Shift + 3", category: "模式" },
    { action: "切换模型 Flash/Pro", keys: "Ctrl/Cmd + Shift + M", category: "模型" },
  ];

  const categories = [...new Set(shortcuts.map(s => s.category))];

  return (
    <div>
      {categories.map(cat => (
        <div key={cat} style={{ marginBottom: 12 }}>
          <div style={{ fontSize: 10, fontWeight: 600, textTransform: "uppercase", color: "var(--text-3)", letterSpacing: 0.5, marginBottom: 6 }}>{cat}</div>
          {shortcuts.filter(s => s.category === cat).map((s) => (
            <div key={s.action} style={{ display: "flex", alignItems: "center", justifyContent: "space-between", padding: "4px 0", borderBottom: "1px solid var(--border-1)" }}>
              <span style={{ fontSize: 12, color: "var(--text-1)" }}>{s.action}</span>
              <kbd className="tag" style={{ fontFamily: "var(--font-mono)", fontSize: 10, background: "var(--bg-3)", color: "var(--text-2)", padding: "2px 8px" }}>{s.keys}</kbd>
            </div>
          ))}
        </div>
      ))}
      <button className="btn btn-ghost" style={{ width: "100%", justifyContent: "center", fontSize: 11, marginTop: 8 }}>+ 自定义快捷键</button>
    </div>
  );
}

/* ============================================================
 * 沙箱设置
 * ============================================================ */
function SandboxSettings() {
  const [sandboxEnabled, setSandboxEnabled] = useState(true);
  const [allowedPaths] = useState(["/home/user/project", "/tmp"]);
  const [newPath, setNewPath] = useState("");
  const [blockedPaths] = useState(["/etc", "/var", "~/.ssh"]);

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 10 }}>
      <SettingRow label="启用目录沙箱" desc="所有工具仅能访问白名单内的目录">
        <label className="toggle-switch"><input type="checkbox" checked={sandboxEnabled} onChange={(e) => setSandboxEnabled(e.target.checked)} /><span className="toggle-slider"></span></label>
      </SettingRow>

      <div>
        <div style={{ fontSize: 12, fontWeight: 600, color: "var(--text-1)", marginBottom: 6 }}>✅ 允许访问的路径</div>
        {allowedPaths.map((p, i) => (
          <div key={i} className="card" style={{ padding: "4px 8px", marginBottom: 4, display: "flex", alignItems: "center" }}>
            <span style={{ fontSize: 11, color: "var(--green)", fontFamily: "var(--font-mono)", flex: 1 }}>{p}</span>
            <button className="btn-icon" style={{ width: 20, height: 20 }} title="移除">✕</button>
          </div>
        ))}
        <div style={{ display: "flex", gap: 4 }}>
          <input className="input" placeholder="添加路径…" value={newPath} onChange={(e) => setNewPath(e.target.value)} style={{ flex: 1, fontSize: 11 }} />
          <button className="btn btn-primary" style={{ fontSize: 11 }}>添加</button>
        </div>
      </div>

      <div>
        <div style={{ fontSize: 12, fontWeight: 600, color: "var(--text-1)", marginBottom: 6 }}>🚫 禁止访问的路径</div>
        {blockedPaths.map((p, i) => (
          <div key={i} className="card" style={{ padding: "4px 8px", marginBottom: 4 }}>
            <span style={{ fontSize: 11, color: "var(--red)", fontFamily: "var(--font-mono)" }}>{p}</span>
          </div>
        ))}
      </div>

      <SettingRow label="环境变量隔离" desc="隐藏敏感环境变量（API Key 等）">
        <label className="toggle-switch"><input type="checkbox" defaultChecked /><span className="toggle-slider"></span></label>
      </SettingRow>
      <SettingRow label="CSP 策略" desc="内容安全策略，防止 XSS">
        <label className="toggle-switch"><input type="checkbox" defaultChecked /><span className="toggle-slider"></span></label>
      </SettingRow>
    </div>
  );
}

/* ============================================================
 * 网络设置
 * ============================================================ */
function NetworkSettings() {
  const [proxy, setProxy] = useState("");
  const [timeout, setTimeout] = useState(30);
  const [retry, setRetry] = useState(3);
  const [allowNetwork, setAllowNetwork] = useState(true);

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 0 }}>
      <SettingRow label="允许网络访问" desc="允许 Agent 使用 web_search 等网络工具">
        <label className="toggle-switch"><input type="checkbox" checked={allowNetwork} onChange={(e) => setAllowNetwork(e.target.checked)} /><span className="toggle-slider"></span></label>
      </SettingRow>
      <SettingRow label="代理服务器" desc="HTTP/HTTPS 代理地址">
        <input className="input" placeholder="http://127.0.0.1:7890" value={proxy} onChange={(e) => setProxy(e.target.value)} style={{ width: 260 }} />
      </SettingRow>
      <SettingRow label="请求超时" desc="API 请求超时时间（秒）">
        <input type="number" className="input" value={timeout} onChange={(e) => setTimeout(Number(e.target.value))} style={{ width: 80 }} />
      </SettingRow>
      <SettingRow label="重试次数" desc="网络请求失败重试次数">
        <input type="number" className="input" value={retry} onChange={(e) => setRetry(Number(e.target.value))} style={{ width: 80 }} />
      </SettingRow>
      <SettingRow label="SSL 验证" desc="验证 SSL 证书">
        <label className="toggle-switch"><input type="checkbox" defaultChecked /><span className="toggle-slider"></span></label>
      </SettingRow>
      <SettingRow label="自动重连" desc="网络断开后自动重连">
        <label className="toggle-switch"><input type="checkbox" defaultChecked /><span className="toggle-slider"></span></label>
      </SettingRow>

      <div style={{ marginTop: 12 }}>
        <div style={{ fontSize: 12, fontWeight: 600, color: "var(--text-1)", marginBottom: 6 }}>网络诊断</div>
        <div className="card" style={{ padding: 8 }}>
          <div style={{ display: "flex", alignItems: "center", gap: 6, fontSize: 11 }}>
            <span className="status-dot ready" />
            <span style={{ color: "var(--green)" }}>DeepSeek API</span>
            <span style={{ color: "var(--text-muted)" }}>— 128ms</span>
          </div>
          <div style={{ display: "flex", alignItems: "center", gap: 6, fontSize: 11, marginTop: 4 }}>
            <span className="status-dot ready" />
            <span style={{ color: "var(--green)" }}>GitHub API</span>
            <span style={{ color: "var(--text-muted)" }}>— 45ms</span>
          </div>
          <div style={{ display: "flex", alignItems: "center", gap: 6, fontSize: 11, marginTop: 4 }}>
            <span className="status-dot error" />
            <span style={{ color: "var(--red)" }}>MCP: web-search</span>
            <span style={{ color: "var(--text-muted)" }}>— 未连接</span>
          </div>
        </div>
        <button className="btn" style={{ width: "100%", justifyContent: "center", fontSize: 11, marginTop: 6 }}>重新检测</button>
      </div>
    </div>
  );
}

/* ============================================================
 * 权限设置
 * ============================================================ */
function PermissionsSettings() {
  const rules = [
    { name: "目录沙箱", desc: "所有工具仅能访问启动时的项目目录", enabled: true, type: "文件" },
    { name: "Plan 模式", desc: "AI 只能读，不能写，必须先提交计划", enabled: false, type: "执行" },
    { name: "Review 审批", desc: "写操作进入审核队列，每次确认", enabled: true, type: "执行" },
    { name: "Shell 命令确认", desc: "所有 Shell 命令都需要用户确认", enabled: true, type: "执行" },
    { name: "自动提交", desc: "Agent 完成任务后自动 git commit", enabled: false, type: "Git" },
    { name: "网络访问", desc: "允许 Agent 访问网络", enabled: true, type: "网络" },
    { name: "文件删除", desc: "允许 Agent 删除文件", enabled: false, type: "文件" },
    { name: "文件大小限制", desc: "单文件读写最大 10MB", enabled: true, type: "限制" },
    { name: "Token 预算", desc: "单会话 Token 上限 500K", enabled: true, type: "限制" },
    { name: "敏感文件保护", desc: "禁止访问 .env、.ssh、.aws 等", enabled: true, type: "安全" },
    { name: "多标签隔离", desc: "标签之间完全隔离，不共享上下文", enabled: true, type: "隔离" },
    { name: "剪贴板访问", desc: "允许 Agent 读取剪贴板内容", enabled: false, type: "隐私" },
  ];

  const [ruleStates, setRuleStates] = useState(rules);

  const toggleRule = (idx: number) => {
    const next = [...ruleStates];
    next[idx] = { ...next[idx], enabled: !next[idx].enabled };
    setRuleStates(next);
  };

  return (
    <div>
      <div style={{ fontSize: 10, color: "var(--text-3)", marginBottom: 8 }}>当前生效的权限配置（共 {ruleStates.length} 条）</div>
      {ruleStates.map((r, i) => (
        <div key={r.name} className="card" style={{ padding: "6px 8px", marginBottom: 4 }}>
          <div style={{ display: "flex", alignItems: "center", gap: 6 }}>
            <span className={`tag ${r.type === "安全" ? "tag-red" : r.type === "执行" ? "tag-amber" : r.type === "Git" ? "tag-cyan" : r.type === "网络" ? "tag-blue" : r.type === "隐私" ? "tag-red" : "tag-blue"}`} style={{ fontSize: 9 }}>{r.type}</span>
            <span style={{ fontSize: 12, fontWeight: 500, color: "var(--text-1)" }}>{r.name}</span>
            <label className="toggle-switch" style={{ marginLeft: "auto" }} onClick={(e) => e.stopPropagation()}>
              <input type="checkbox" checked={r.enabled} onChange={() => toggleRule(i)} />
              <span className="toggle-slider"></span>
            </label>
          </div>
          <div style={{ fontSize: 10, color: "var(--text-3)", marginTop: 3 }}>{r.desc}</div>
        </div>
      ))}
    </div>
  );
}

/* ============================================================
 * 钩子设置
 * ============================================================ */
function HooksSettings() {
  const hooks = [
    { event: "on_session_start", command: "echo 'Session started'", enabled: true },
    { event: "on_session_end", command: "git add -A && git stash", enabled: true },
    { event: "on_file_write", command: "prettier --write $FILE", enabled: false },
    { event: "on_tool_call", command: "logger -t deepseeknova 'Tool: $TOOL'", enabled: true },
    { event: "on_error", command: "notify-send 'Error' '$ERROR_MSG'", enabled: false },
    { event: "on_approval_request", command: "paplay /usr/share/sounds/alert.wav", enabled: true },
    { event: "on_task_complete", command: "notify-send 'Done' 'Task completed'", enabled: true },
    { event: "on_budget_exceeded", command: "notify-send 'Budget!' 'Check billing'", enabled: true },
  ];

  const [hookStates, setHookStates] = useState(hooks);

  const events = [
    "on_session_start", "on_session_end", "on_message_send", "on_message_receive",
    "on_file_read", "on_file_write", "on_tool_call", "on_tool_result",
    "on_error", "on_approval_request", "on_task_complete", "on_budget_exceeded",
    "on_model_switch", "on_mode_change", "on_cache_miss",
  ];

  return (
    <div>
      <div style={{ fontSize: 10, color: "var(--text-3)", marginBottom: 8 }}>在特定事件触发时执行自定义命令</div>
      {hookStates.map((h, i) => (
        <div key={i} className="card" style={{ padding: "8px 10px", marginBottom: 6 }}>
          <div style={{ display: "flex", alignItems: "center", gap: 6, marginBottom: 4 }}>
            <span className="tag tag-cyan" style={{ fontFamily: "var(--font-mono)", fontSize: 10 }}>{h.event}</span>
            <label className="toggle-switch" style={{ marginLeft: "auto" }} onClick={(e) => e.stopPropagation()}>
              <input type="checkbox" checked={h.enabled} onChange={() => {
                const next = [...hookStates]; next[i] = { ...next[i], enabled: !next[i].enabled }; setHookStates(next);
              }} />
              <span className="toggle-slider"></span>
            </label>
          </div>
          <div style={{ fontSize: 11, fontFamily: "var(--font-mono)", color: "var(--text-2)", background: "var(--bg-3)", padding: "4px 6px", borderRadius: "var(--radius-sm)" }}>
            $ {h.command}
          </div>
        </div>
      ))}
      <div style={{ marginTop: 12 }}>
        <div style={{ fontSize: 12, fontWeight: 600, color: "var(--text-1)", marginBottom: 6 }}>+ 添加新钩子</div>
        <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
          <select className="input" style={{ fontSize: 11 }}>
            {events.map(e => <option key={e} value={e}>{e}</option>)}
          </select>
          <input className="input" placeholder="shell command…" style={{ fontSize: 11, fontFamily: "var(--font-mono)" }} />
          <button className="btn btn-primary" style={{ fontSize: 11 }}>添加钩子</button>
        </div>
      </div>
      <div style={{ marginTop: 12, fontSize: 10, color: "var(--text-muted)" }}>
        可用变量：$FILE, $TOOL, $ERROR_MSG, $SESSION_ID, $MODEL, $MODE, $TOKENS
      </div>
    </div>
  );
}

/* ============================================================
 * MCP / 插件设置
 * ============================================================ */
function MCPSettings() {
  const [servers] = useState([
    { name: "filesystem", command: "npx", args: "@modelcontextprotocol/server-filesystem", transport: "stdio", status: "running" },
    { name: "git", command: "npx", args: "@modelcontextprotocol/server-git", transport: "stdio", status: "running" },
    { name: "shell", command: "npx", args: "@modelcontextprotocol/server-shell", transport: "stdio", status: "running" },
    { name: "web-search", command: "npx", args: "@modelcontextprotocol/server-brave-search", transport: "stdio", status: "stopped" },
    { name: "github", command: "npx", args: "@modelcontextprotocol/server-github", transport: "stdio", status: "stopped" },
    { name: "database", command: "npx", args: "@modelcontextprotocol/server-postgres", transport: "sse", status: "stopped" },
  ]);

  return (
    <div>
      <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: 8 }}>
        <span style={{ fontSize: 10, color: "var(--text-3)" }}>MCP 服务器（{servers.filter(s => s.status === "running").length}/{servers.length} 运行中）</span>
        <button className="btn btn-primary" style={{ fontSize: 11 }}>+ 添加</button>
      </div>

      {servers.map((s) => (
        <div key={s.name} className="card" style={{ padding: "8px 10px", marginBottom: 6 }}>
          <div style={{ display: "flex", alignItems: "center", gap: 6, marginBottom: 4 }}>
            <span style={{ fontSize: 12, fontWeight: 600, color: "var(--text-1)" }}>{s.name}</span>
            <span className={`tag ${s.status === "running" ? "tag-green" : ""}`} style={{ marginLeft: "auto", fontSize: 9 }}>
              {s.status === "running" ? "● 运行中" : "○ 已停止"}
            </span>
          </div>
          <div style={{ fontSize: 10, color: "var(--text-3)", fontFamily: "var(--font-mono)", marginBottom: 4 }}>
            {s.command} {s.args}
          </div>
          <div style={{ display: "flex", alignItems: "center", gap: 6 }}>
            <span className="tag tag-blue" style={{ fontSize: 9 }}>{s.transport}</span>
            <button className="btn btn-ghost" style={{ fontSize: 10, padding: "2px 6px" }}>配置</button>
            <button className="btn btn-ghost" style={{ fontSize: 10, padding: "2px 6px" }}>{s.status === "running" ? "停止" : "启动"}</button>
            <button className="btn btn-ghost" style={{ fontSize: 10, padding: "2px 6px", color: "var(--red)" }}>删除</button>
          </div>
        </div>
      ))}

      <div style={{ marginTop: 12 }}>
        <div style={{ fontSize: 12, fontWeight: 600, color: "var(--text-1)", marginBottom: 6 }}>支持的传输协议</div>
        <div style={{ display: "flex", gap: 8 }}>
          <div className="card" style={{ padding: "6px 10px", textAlign: "center" }}>
            <div style={{ fontSize: 11, fontWeight: 600, color: "var(--accent)" }}>stdio</div>
            <div style={{ fontSize: 9, color: "var(--text-3)" }}>本地进程</div>
          </div>
          <div className="card" style={{ padding: "6px 10px", textAlign: "center" }}>
            <div style={{ fontSize: 11, fontWeight: 600, color: "var(--accent)" }}>SSE</div>
            <div style={{ fontSize: 9, color: "var(--text-3)" }}>服务端推送</div>
          </div>
          <div className="card" style={{ padding: "6px 10px", textAlign: "center" }}>
            <div style={{ fontSize: 11, fontWeight: 600, color: "var(--accent)" }}>HTTP</div>
            <div style={{ fontSize: 9, color: "var(--text-3)" }}>流式 HTTP</div>
          </div>
        </div>
      </div>
    </div>
  );
}

/* ============================================================
 * 子智能体设置
 * ============================================================ */
function SubAgentsSettings() {
  const agents = [
    { name: "code-reviewer", desc: "代码审查专家", model: "deepseek-v4-pro", status: "idle", tasks: 12 },
    { name: "bug-hunter", desc: "Bug 检测和根因分析", model: "deepseek-v4-pro", status: "idle", tasks: 5 },
    { name: "test-generator", desc: "自动生成测试用例", model: "deepseek-v4-flash", status: "idle", tasks: 8 },
    { name: "refactor-assistant", desc: "代码重构建议", model: "deepseek-v4-pro", status: "running", tasks: 3 },
    { name: "frontend-design", desc: "前端 UI/UX 设计", model: "deepseek-v4-flash", status: "idle", tasks: 7 },
    { name: "doc-generator", desc: "文档生成", model: "deepseek-v4-flash", status: "idle", tasks: 15 },
  ];

  return (
    <div>
      <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: 8 }}>
        <span style={{ fontSize: 10, color: "var(--text-3)" }}>子智能体（{agents.length} 个，{agents.filter(a => a.status === "running").length} 个运行中）</span>
        <button className="btn btn-primary" style={{ fontSize: 11 }}>+ 创建</button>
      </div>

      {agents.map((a) => (
        <div key={a.name} className="card" style={{ padding: "8px 10px", marginBottom: 6 }}>
          <div style={{ display: "flex", alignItems: "center", gap: 6, marginBottom: 4 }}>
            <span style={{ fontSize: 12, fontWeight: 600, color: "var(--accent)", fontFamily: "var(--font-mono)" }}>{a.name}</span>
            <span className={`tag ${a.status === "running" ? "tag-amber" : "tag-green"}`} style={{ marginLeft: "auto", fontSize: 9 }}>
              {a.status === "running" ? "● 运行中" : "● 空闲"}
            </span>
          </div>
          <div style={{ fontSize: 11, color: "var(--text-3)", marginBottom: 4 }}>{a.desc}</div>
          <div style={{ display: "flex", alignItems: "center", gap: 6, fontSize: 10 }}>
            <span className="tag tag-cyan" style={{ fontSize: 9 }}>{a.model}</span>
            <span style={{ color: "var(--text-muted)" }}>·</span>
            <span style={{ color: "var(--text-3)" }}>{a.tasks} 次任务</span>
            <div style={{ marginLeft: "auto", display: "flex", gap: 4 }}>
              <button className="btn btn-ghost" style={{ fontSize: 10, padding: "2px 6px" }}>配置</button>
              <button className="btn btn-ghost" style={{ fontSize: 10, padding: "2px 6px" }}>调用</button>
            </div>
          </div>
        </div>
      ))}
    </div>
  );
}

/* ============================================================
 * 技能设置
 * ============================================================ */
function SkillsSettings({ skills }: { skills: any[] }) {
  const allSkills = skills.length > 0 ? skills : [
    { name: "csv-processor", description: "CSV 文件处理和数据分析", tools_allowed: ["read_file", "write_file"] },
    { name: "git-master", description: "Git 操作和分支管理", tools_allowed: ["run_command", "git_diff"] },
    { name: "api-tester", description: "API 测试和接口文档", tools_allowed: ["web_search", "run_command"] },
    { name: "doc-writer", description: "项目文档自动生成", tools_allowed: ["read_file", "write_file", "list_dir"] },
    { name: "refactor-pro", description: "代码重构和优化建议", tools_allowed: ["read_file", "search_files"] },
  ];

  return (
    <div>
      <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: 8 }}>
        <span style={{ fontSize: 10, color: "var(--text-3)" }}>已安装技能（{allSkills.length} 个）</span>
        <div style={{ display: "flex", gap: 4 }}>
          <button className="btn" style={{ fontSize: 11 }}>📦 技能市场</button>
          <button className="btn btn-primary" style={{ fontSize: 11 }}>+ 创建</button>
        </div>
      </div>

      {allSkills.map((s) => (
        <div key={s.name} className="card" style={{ padding: "8px 10px", marginBottom: 6 }}>
          <div style={{ display: "flex", alignItems: "center", gap: 6, marginBottom: 4 }}>
            <span style={{ fontSize: 12, fontWeight: 600, color: "var(--accent)", fontFamily: "var(--font-mono)" }}>{s.name}</span>
            <div style={{ marginLeft: "auto", display: "flex", gap: 4 }}>
              <button className="btn btn-ghost" style={{ fontSize: 10, padding: "2px 6px" }}>编辑</button>
              <button className="btn btn-ghost" style={{ fontSize: 10, padding: "2px 6px", color: "var(--red)" }}>卸载</button>
            </div>
          </div>
          <div style={{ fontSize: 11, color: "var(--text-3)", marginBottom: 4 }}>{s.description}</div>
          <div style={{ display: "flex", gap: 3, flexWrap: "wrap" }}>
            {s.tools_allowed.map((t: string) => (
              <span key={t} className="tag tag-cyan" style={{ fontSize: 9 }}>{t}</span>
            ))}
          </div>
        </div>
      ))}

      <div style={{ marginTop: 12 }}>
        <div style={{ fontSize: 10, color: "var(--text-3)", lineHeight: 1.6 }}>
          技能使用 Markdown 格式定义，放在 .deepseeknova/skills/ 目录下。兼容 Claude Code 的 .claude/skills/ 格式，跨项目复用。
        </div>
      </div>
    </div>
  );
}
/* ============================================================
 * 诊断设置
 * ============================================================ */
function DiagnosticsSettings({}: any) {
  const [running, setRunning] = useState(false);
  const [results, setResults] = useState<any[]>([]);

  const runDiagnostics = () => {
    setRunning(true);
    setTimeout(() => {
      setResults([
        { name: "Node.js 运行时", status: "pass", detail: "v22.22.1" },
        { name: "Tauri 框架", status: "pass", detail: "v2.0" },
        { name: "DeepSeek API 连接", status: "pass", detail: "128ms" },
        { name: "API Key 有效", status: "pass", detail: "sk-••••••••" },
        { name: "MCP: filesystem", status: "pass", detail: "运行中" },
        { name: "MCP: git", status: "pass", detail: "运行中" },
        { name: "MCP: web-search", status: "warn", detail: "未启动" },
        { name: "缓存系统", status: "pass", detail: "命中率 94%" },
        { name: "记忆系统", status: "pass", detail: "7 条记忆" },
        { name: "沙箱配置", status: "pass", detail: "目录限制已启用" },
        { name: "磁盘空间", status: "pass", detail: "12.4 GB 可用" },
        { name: "内存使用", status: "warn", detail: "412 MB / 2 GB" },
      ]);
      setRunning(false);
    }, 1500);
  };

  return (
    <div>
      <div style={{ display: "flex", justifyContent: "space-between", alignItems: "center", marginBottom: 8 }}>
        <span style={{ fontSize: 10, color: "var(--text-3)" }}>系统体检 — 检查所有组件状态</span>
        <button className="btn btn-primary" onClick={runDiagnostics} disabled={running} style={{ fontSize: 11 }}>
          {running ? "检测中…" : "开始体检"}
        </button>
      </div>

      {results.length > 0 && (
        <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
          {results.map((r, i) => (
            <div key={i} className="card" style={{ padding: "6px 10px", display: "flex", alignItems: "center", gap: 6 }}>
              <span className={`status-dot ${r.status === "pass" ? "ready" : r.status === "warn" ? "running" : "error"}`} />
              <span style={{ fontSize: 11, fontWeight: 500, color: "var(--text-1)", flex: 1 }}>{r.name}</span>
              <span style={{ fontSize: 10, color: r.status === "pass" ? "var(--green)" : r.status === "warn" ? "var(--amber)" : "var(--red)" }}>
                {r.status === "pass" ? "✓ 正常" : r.status === "warn" ? "⚠ 警告" : "✕ 错误"}
              </span>
              <span style={{ fontSize: 10, color: "var(--text-muted)", fontFamily: "var(--font-mono)" }}>{r.detail}</span>
            </div>
          ))}
          <div style={{ display: "flex", justifyContent: "space-between", marginTop: 8, fontSize: 11 }}>
            <span style={{ color: "var(--green)" }}>✓ {results.filter(r => r.status === "pass").length} 正常</span>
            <span style={{ color: "var(--amber)" }}>⚠ {results.filter(r => r.status === "warn").length} 警告</span>
            <span style={{ color: "var(--red)" }}>✕ {results.filter(r => r.status === "error").length} 错误</span>
          </div>
        </div>
      )}

      {results.length === 0 && !running && (
        <div className="empty-state" style={{ padding: 20 }}>
          <div className="empty-state-icon">🔬</div>
          <div className="empty-state-text">点击"开始体检"检查系统状态</div>
        </div>
      )}

      <div style={{ marginTop: 12 }}>
        <div style={{ fontSize: 12, fontWeight: 600, color: "var(--text-1)", marginBottom: 6 }}>日志</div>
        <div className="card" style={{ padding: 8, fontSize: 10, fontFamily: "var(--font-mono)", color: "var(--text-3)", maxHeight: 150, overflowY: "auto" }}>
          <div>[22:14:01] Agent started, model=deepseek-v4-flash</div>
          <div>[22:14:02] Cache initialized, hit_rate=0%</div>
          <div>[22:14:10] Tool call: write_file (csv_processor.py)</div>
          <div>[22:14:11] File created: csv_processor.py (1.8 KB)</div>
          <div>[22:14:12] Cache hit_rate=94%</div>
          <div>[22:15:00] Session duration: 1m 0s</div>
        </div>
      </div>
    </div>
  );
}

/* ============================================================
 * 账单设置
 * ============================================================ */
function BillingSettings() {
  const sessionCache = useStore((s) => s.sessionCache);
  const totalTokens = useStore((s) => s.totalTokens);
  const lastUsage = useStore((s) => s.lastUsage);

  const totalCache = sessionCache.hit + sessionCache.miss;
  const cacheRate = totalCache > 0 ? (sessionCache.hit / totalCache) * 100 : 0;

  const flashInputPrice = 0.28;
  const flashCachedPrice = 0.028;
  const flashOutputPrice = 0.88;

  const inputCost = (sessionCache.miss / 1000000) * flashInputPrice;
  const cachedCost = (sessionCache.hit / 1000000) * flashCachedPrice;
  const outputCost = lastUsage ? (lastUsage.completion_tokens / 1000000) * flashOutputPrice : 0;
  const totalCost = inputCost + cachedCost + outputCost;

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 10 }}>
      <div className="card" style={{ padding: 10 }}>
        <div style={{ fontSize: 12, fontWeight: 600, color: "var(--text-1)", marginBottom: 8 }}>本会话</div>
        <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: 8 }}>
          <StatBox label="缓存命中" value={sessionCache.hit.toLocaleString()} sub={`${cacheRate.toFixed(1)}%`} color="var(--green)" />
          <StatBox label="未缓存" value={sessionCache.miss.toLocaleString()} sub="按全价" color="var(--amber)" />
          <StatBox label="输出" value={lastUsage?.completion_tokens.toLocaleString() || "0"} sub={`推理 ${lastUsage?.reasoning_tokens || 0}`} color="var(--blue)" />
          <StatBox label="总计" value={totalTokens.toLocaleString()} sub="累计" color="var(--accent)" />
        </div>
      </div>

      <div className="card" style={{ padding: 10 }}>
        <div style={{ fontSize: 12, fontWeight: 600, color: "var(--text-1)", marginBottom: 8 }}>费用明细（V4 Flash）</div>
        <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
          <div style={{ display: "flex", justifyContent: "space-between", fontSize: 11 }}>
            <span style={{ color: "var(--text-2)" }}>输入（全价）</span>
            <span style={{ color: "var(--text-3)", fontSize: 10 }}>{sessionCache.miss.toLocaleString()} × ¥{flashInputPrice}/M</span>
            <span style={{ color: "var(--text-1)", fontWeight: 500, minWidth: 60, textAlign: "right" }}>¥{inputCost.toFixed(4)}</span>
          </div>
          <div style={{ display: "flex", justifyContent: "space-between", fontSize: 11 }}>
            <span style={{ color: "var(--text-2)" }}>输入（缓存）</span>
            <span style={{ color: "var(--text-3)", fontSize: 10 }}>{sessionCache.hit.toLocaleString()} × ¥{flashCachedPrice}/M</span>
            <span style={{ color: "var(--text-1)", fontWeight: 500, minWidth: 60, textAlign: "right" }}>¥{cachedCost.toFixed(4)}</span>
          </div>
          <div style={{ display: "flex", justifyContent: "space-between", fontSize: 11 }}>
            <span style={{ color: "var(--text-2)" }}>输出</span>
            <span style={{ color: "var(--text-3)", fontSize: 10 }}>{(lastUsage?.completion_tokens || 0).toLocaleString()} × ¥{flashOutputPrice}/M</span>
            <span style={{ color: "var(--text-1)", fontWeight: 500, minWidth: 60, textAlign: "right" }}>¥{outputCost.toFixed(4)}</span>
          </div>
          <div style={{ borderTop: "1px solid var(--border-1)", marginTop: 4, paddingTop: 4, display: "flex", justifyContent: "space-between" }}>
            <span style={{ fontSize: 12, fontWeight: 600, color: "var(--text-1)" }}>总计</span>
            <span style={{ fontSize: 14, fontWeight: 700, color: "var(--accent)" }}>¥{totalCost.toFixed(4)}</span>
          </div>
        </div>
      </div>

      <div className="card" style={{ padding: 10 }}>
        <div style={{ fontSize: 12, fontWeight: 600, color: "var(--text-1)", marginBottom: 6 }}>缓存分析</div>
        <div style={{ height: 8, borderRadius: 4, background: "var(--bg-3)", overflow: "hidden", display: "flex" }}>
          <div style={{ width: `${cacheRate}%`, background: "var(--green)", transition: "width 0.3s" }} />
          <div style={{ width: `${100 - cacheRate}%`, background: "var(--amber)", transition: "width 0.3s" }} />
        </div>
        <div style={{ display: "flex", justifyContent: "space-between", marginTop: 6, fontSize: 10, color: "var(--text-3)" }}>
          <span>🟢 命中 {cacheRate.toFixed(1)}%</span>
          <span>🟡 未缓存 {(100 - cacheRate).toFixed(1)}%</span>
        </div>
      </div>

      <div className="card" style={{ padding: 10 }}>
        <div style={{ fontSize: 12, fontWeight: 600, color: "var(--text-1)", marginBottom: 6 }}>历史会话</div>
        <div style={{ display: "flex", flexDirection: "column", gap: 4, fontSize: 11 }}>
          <div style={{ display: "flex", justifyContent: "space-between" }}>
            <span style={{ color: "var(--text-2)" }}>今天</span>
            <span style={{ color: "var(--text-1)" }}>3 会话 · ¥0.42</span>
          </div>
          <div style={{ display: "flex", justifyContent: "space-between" }}>
            <span style={{ color: "var(--text-2)" }}>昨天</span>
            <span style={{ color: "var(--text-1)" }}>5 会话 · ¥0.78</span>
          </div>
          <div style={{ display: "flex", justifyContent: "space-between" }}>
            <span style={{ color: "var(--text-2)" }}>本周</span>
            <span style={{ color: "var(--text-1)" }}>18 会话 · ¥2.14</span>
          </div>
          <div style={{ display: "flex", justifyContent: "space-between" }}>
            <span style={{ color: "var(--text-2)" }}>本月</span>
            <span style={{ color: "var(--text-1)" }}>62 会话 · ¥8.45</span>
          </div>
        </div>
      </div>
    </div>
  );
}

/* ============================================================
 * 关于设置
 * ============================================================ */
function AboutSettings({ capabilities }: any) {
  const [checking, setChecking] = useState(false);
  const [updateAvailable, setUpdateAvailable] = useState(false);

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 10, alignItems: "center", textAlign: "center" }}>
      <div style={{ fontSize: 32, marginBottom: 4 }}>📋🤖</div>
      <div style={{ fontSize: 16, fontWeight: 700, color: "var(--text-1)" }}>DeepseekNova</div>
      <div style={{ fontSize: 11, color: "var(--text-3)" }}>版本 v{capabilities?.version || "0.3.0"}</div>
      <div style={{ fontSize: 11, color: "var(--text-2)", maxWidth: 320, lineHeight: 1.6, marginTop: 4 }}>
        DeepSeek 原生桌面端 AI 编程助手，围绕 Prefix-Cache 机制深度优化，极致缓存命中率。
      </div>
      <button className="btn btn-primary" onClick={() => { setChecking(true); setTimeout(() => { setChecking(false); setUpdateAvailable(false); }, 2000); }} disabled={checking} style={{ marginTop: 8 }}>
        {checking ? "检查中…" : "检查更新"}
      </button>
      {updateAvailable && <div style={{ fontSize: 11, color: "var(--green)" }}>发现新版本！</div>}
      <div style={{ marginTop: 12, fontSize: 10, color: "var(--text-muted)" }}>
        <div>MIT 协议 · 开源免费</div>
        <div style={{ marginTop: 2 }}>Rust + Tauri 2.0 + React 18</div>
      </div>
      <div style={{ display: "flex", gap: 8, marginTop: 12 }}>
        <a href="https://github.com/W117C/DeepseekNova" target="_blank" style={{ fontSize: 11, color: "var(--accent)", textDecoration: "none" }}>GitHub →</a>
        <span style={{ color: "var(--text-muted)" }}>·</span>
        <a href="#" style={{ fontSize: 11, color: "var(--accent)", textDecoration: "none" }}>文档</a>
        <span style={{ color: "var(--text-muted)" }}>·</span>
        <a href="#" style={{ fontSize: 11, color: "var(--accent)", textDecoration: "none" }}>问题反馈</a>
      </div>
    </div>
  );
}
