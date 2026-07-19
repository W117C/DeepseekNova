/**
 * SettingsModal.tsx — 设置面板（完善版）
 *
 * 参考 Reasonix 桌面端的设置面板设计：
 * - 通用：API Key / Base URL / 模型默认 / 语言 / 字体
 * - 外观：主题 / 显示模式 / 字体大小
 * - 执行：默认模式 / 审批策略 / 沙箱
 * - 记忆：记忆管理 / 记忆查看
 * - MCP：MCP 服务器配置
 * - 账单：Token 用量 / 费用统计 / 缓存分析
 * - 关于：版本 / 更新 / 开源信息
 */

import { useState } from "react";
import { useStore } from "../store";
import { useTheme } from "../store/theme";

type SettingsSection = "general" | "appearance" | "execution" | "memory" | "mcp" | "billing" | "about";

export default function SettingsModal() {
  const setShowSettings = useStore((s) => s.setShowSettings);
  const capabilities = useStore((s) => s.capabilities);
  const theme = useTheme((s) => s.theme);
  const setTheme = useTheme((s) => s.setTheme);
  const displayMode = useTheme((s) => s.displayMode);
  const toggleDisplayMode = useTheme((s) => s.toggleDisplayMode);

  const [section, setSection] = useState<SettingsSection>("general");

  const sections: { id: SettingsSection; label: string; icon: string }[] = [
    { id: "general", label: "通用", icon: "⚙️" },
    { id: "appearance", label: "外观", icon: "🎨" },
    { id: "execution", label: "执行", icon: "🚀" },
    { id: "memory", label: "记忆", icon: "🧠" },
    { id: "mcp", label: "MCP", icon: "🔌" },
    { id: "billing", label: "账单", icon: "💰" },
    { id: "about", label: "关于", icon: "ℹ️" },
  ];

  return (
    <div className="modal-overlay" onClick={() => setShowSettings(false)}>
      <div className="modal" onClick={(e) => e.stopPropagation()} style={{ maxWidth: 720, height: "80vh", flexDirection: "row" }}>
        {/* 侧边导航 */}
        <div style={{ width: 160, borderRight: "1px solid var(--border-1)", background: "var(--bg-1)", overflowY: "auto" }}>
          <div style={{ padding: "10px 12px 6px", fontSize: 10, fontWeight: 600, textTransform: "uppercase", color: "var(--text-3)", letterSpacing: 0.5 }}>
            设置
          </div>
          {sections.map((s) => (
            <div
              key={s.id}
              className={`sidebar-item ${section === s.id ? "active" : ""}`}
              onClick={() => setSection(s.id)}
              style={{ padding: "6px 12px" }}
            >
              <span style={{ fontSize: 12 }}>{s.icon}</span>
              <span className="sidebar-item-title">{s.label}</span>
            </div>
          ))}
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
            {section === "memory" && <MemorySettings />}
            {section === "mcp" && <MCPSettings />}
            {section === "billing" && <BillingSettings />}
            {section === "about" && <AboutSettings capabilities={capabilities} />}
          </div>
        </div>
      </div>
    </div>
  );
}

/* === 通用 === */
function GeneralSettings() {
  const [apiKey, setApiKey] = useState("sk-••••••••••••••••");
  const [baseUrl, setBaseUrl] = useState("https://api.deepseek.com");
  const [defaultModel, setDefaultModel] = useState("deepseek-v4-flash");
  const [language, setLanguage] = useState("zh-CN");
  const [fontSize, setFontSize] = useState(13);
  const [fontFamily, setFontFamily] = useState("system");

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 12 }}>
      <SettingRow label="API Key" desc="DeepSeek API 密钥，在 platform.deepseek.com 申请">
        <input className="input" type="password" value={apiKey} onChange={(e) => setApiKey(e.target.value)} style={{ width: 280 }} />
      </SettingRow>
      <SettingRow label="Base URL" desc="API 基础地址，可替换为代理地址">
        <input className="input" value={baseUrl} onChange={(e) => setBaseUrl(e.target.value)} style={{ width: 280 }} />
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
    </div>
  );
}

/* === 外观 === */
function AppearanceSettings({ theme, setTheme, displayMode, toggleDisplayMode }: any) {
  const [accentColor, setAccentColor] = useState("#6b5ded");

  const colorPresets = ["#6b5ded", "#7c3aed", "#2563eb", "#0891b2", "#16a34a", "#d97706", "#dc2626"];

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 16 }}>
      <SettingRow label="主题" desc="选择界面主题模式">
        <div style={{ display: "flex", gap: 8 }}>
          {[
            { id: "light", label: "☀️ 浅色" },
            { id: "dark", label: "🌙 深色" },
            { id: "system", label: "🖥️ 跟随系统" },
          ].map((t) => (
            <button
              key={t.id}
              className={`btn ${theme === t.id ? "btn-primary" : ""}`}
              onClick={() => setTheme(t.id)}
              style={{ padding: "6px 12px" }}
            >
              {t.label}
            </button>
          ))}
        </div>
      </SettingRow>

      <SettingRow label="显示模式" desc={displayMode === "icon" ? "图标模式（紧凑）" : "文字模式（详细）"}>
        <div style={{ display: "flex", gap: 8 }}>
          <button className={`btn ${displayMode === "icon" ? "btn-primary" : ""}`} onClick={() => { if (displayMode !== "icon") toggleDisplayMode(); }}>
            📦 图标
          </button>
          <button className={`btn ${displayMode === "text" ? "btn-primary" : ""}`} onClick={() => { if (displayMode !== "text") toggleDisplayMode(); }}>
            Aa 文字
          </button>
        </div>
      </SettingRow>

      <SettingRow label="强调色" desc="界面主色调">
        <div style={{ display: "flex", gap: 6 }}>
          {colorPresets.map((c) => (
            <button
              key={c}
              onClick={() => setAccentColor(c)}
              style={{
                width: 24, height: 24, borderRadius: "50%", border: accentColor === c ? "2px solid var(--text-1)" : "2px solid transparent",
                background: c, cursor: "pointer", transition: "all 0.12s",
              }}
            />
          ))}
        </div>
      </SettingRow>

      <SettingRow label="紧凑模式" desc="减少间距，在一屏内显示更多内容">
        <label className="toggle-switch"><input type="checkbox" /><span className="toggle-slider"></span></label>
      </SettingRow>

      <SettingRow label="动画效果" desc="界面过渡动画">
        <label className="toggle-switch"><input type="checkbox" defaultChecked /><span className="toggle-slider"></span></label>
      </SettingRow>

      <SettingRow label="代码行号" desc="代码块显示行号">
        <label className="toggle-switch"><input type="checkbox" defaultChecked /><span className="toggle-slider"></span></label>
      </SettingRow>
    </div>
  );
}

/* === 执行 === */
function ExecutionSettings() {
  const [defaultMode, setDefaultMode] = useState("act");
  const [autoCommit, setAutoCommit] = useState(false);
  const [sandbox, setSandbox] = useState(true);
  const [shellConfirm, setShellConfirm] = useState(true);
  const [tokenBudget, setTokenBudget] = useState(500000);
  const [budgetAlert, setBudgetAlert] = useState(5);

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 12 }}>
      <SettingRow label="默认执行模式" desc="新会话的默认模式">
        <select className="input" value={defaultMode} onChange={(e) => setDefaultMode(e.target.value)} style={{ width: 200 }}>
          <option value="plan">📋 Plan（只读审计）</option>
          <option value="act">✋ Act（写操作需审批）</option>
          <option value="yolo">🚀 YOLO（全自动）</option>
        </select>
      </SettingRow>

      <SettingRow label="目录沙箱" desc="所有工具仅能访问启动时的项目目录">
        <label className="toggle-switch"><input type="checkbox" checked={sandbox} onChange={(e) => setSandbox(e.target.checked)} /><span className="toggle-slider"></span></label>
      </SettingRow>

      <SettingRow label="Shell 命令确认" desc="所有 Shell 命令都需要用户确认">
        <label className="toggle-switch"><input type="checkbox" checked={shellConfirm} onChange={(e) => setShellConfirm(e.target.checked)} /><span className="toggle-slider"></span></label>
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
    </div>
  );
}

/* === 记忆 === */
function MemorySettings() {
  const [enableMemory, setEnableMemory] = useState(true);
  const [autoExtract, setAutoExtract] = useState(true);

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 12 }}>
      <SettingRow label="启用记忆系统" desc="跨会话的持久记忆">
        <label className="toggle-switch"><input type="checkbox" checked={enableMemory} onChange={(e) => setEnableMemory(e.target.checked)} /><span className="toggle-slider"></span></label>
      </SettingRow>
      <SettingRow label="自动提取" desc="AI 自动从对话中提取记忆">
        <label className="toggle-switch"><input type="checkbox" checked={autoExtract} onChange={(e) => setAutoExtract(e.target.checked)} /><span className="toggle-slider"></span></label>
      </SettingRow>
      <SettingRow label="记忆查看" desc="在右侧面板的记忆标签中查看所有记忆">
        <button className="btn" style={{ fontSize: 11 }}>打开记忆面板 →</button>
      </SettingRow>
      <SettingRow label="清除会话记忆" desc="清除当前会话的临时记忆（不影响持久记忆）">
        <button className="btn btn-danger" style={{ fontSize: 11 }}>清除会话记忆</button>
      </SettingRow>
    </div>
  );
}

/* === MCP === */
function MCPSettings() {
  const [servers] = useState([
    { name: "filesystem", command: "npx", args: "@modelcontextprotocol/server-filesystem", status: "running" },
    { name: "git", command: "npx", args: "@modelcontextprotocol/server-git", status: "running" },
    { name: "web-search", command: "npx", args: "@modelcontextprotocol/server-brave-search", status: "stopped" },
  ]);

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 8 }}>
      <div style={{ fontSize: 11, color: "var(--text-3)", marginBottom: 4 }}>
        MCP 服务器配置，支持 stdio / SSE / HTTP 传输
      </div>
      {servers.map((s) => (
        <div key={s.name} className="card" style={{ padding: "8px 10px" }}>
          <div style={{ display: "flex", alignItems: "center", gap: 6, marginBottom: 4 }}>
            <span style={{ fontSize: 12, fontWeight: 600, color: "var(--text-1)" }}>{s.name}</span>
            <span className={`tag ${s.status === "running" ? "tag-green" : ""}`} style={{ marginLeft: "auto", fontSize: 9 }}>
              {s.status === "running" ? "● 运行中" : "○ 已停止"}
            </span>
          </div>
          <div style={{ fontSize: 10, color: "var(--text-3)", fontFamily: "var(--font-mono)" }}>
            {s.command} {s.args}
          </div>
        </div>
      ))}
      <button className="btn btn-ghost" style={{ justifyContent: "center", fontSize: 11 }}>
        + 添加 MCP 服务器
      </button>
    </div>
  );
}

/* === 账单 === */
function BillingSettings() {
  const sessionCache = useStore((s) => s.sessionCache);
  const totalTokens = useStore((s) => s.totalTokens);
  const lastUsage = useStore((s) => s.lastUsage);

  const totalCache = sessionCache.hit + sessionCache.miss;
  const cacheRate = totalCache > 0 ? (sessionCache.hit / totalCache) * 100 : 0;

  // 费用计算（DeepSeek V4 Flash 价格）
  const flashInputPrice = 0.28; // 元/百万 Token
  const flashCachedPrice = 0.028; // 元/百万 Token
  const flashOutputPrice = 0.88; // 元/百万 Token

  const inputCost = (sessionCache.miss / 1000000) * flashInputPrice;
  const cachedCost = (sessionCache.hit / 1000000) * flashCachedPrice;
  const outputCost = lastUsage ? (lastUsage.completion_tokens / 1000000) * flashOutputPrice : 0;
  const totalCost = inputCost + cachedCost + outputCost;

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 10 }}>
      {/* 本会话 */}
      <div className="card" style={{ padding: 10 }}>
        <div style={{ fontSize: 12, fontWeight: 600, color: "var(--text-1)", marginBottom: 8 }}>本会话</div>
        <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: 8 }}>
          <StatBox label="输入 Token" value={sessionCache.hit.toLocaleString()} sub={`缓存命中 ${cacheRate.toFixed(1)}%`} color="var(--green)" />
          <StatBox label="未缓存 Token" value={sessionCache.miss.toLocaleString()} sub={`按全价计费`} color="var(--amber)" />
          <StatBox label="输出 Token" value={lastUsage?.completion_tokens.toLocaleString() || "0"} sub={`推理 ${lastUsage?.reasoning_tokens || 0}`} color="var(--blue)" />
          <StatBox label="总计 Token" value={totalTokens.toLocaleString()} sub={`累计`} color="var(--accent)" />
        </div>
      </div>

      {/* 费用 */}
      <div className="card" style={{ padding: 10 }}>
        <div style={{ fontSize: 12, fontWeight: 600, color: "var(--text-1)", marginBottom: 8 }}>费用明细（V4 Flash）</div>
        <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
          <FeeRow label="输入（全价）" tokens={sessionCache.miss} price={flashInputPrice} cost={inputCost} />
          <FeeRow label="输入（缓存）" tokens={sessionCache.hit} price={flashCachedPrice} cost={cachedCost} />
          <FeeRow label="输出" tokens={lastUsage?.completion_tokens || 0} price={flashOutputPrice} cost={outputCost} />
          <div style={{ borderTop: "1px solid var(--border-1)", marginTop: 4, paddingTop: 4, display: "flex", justifyContent: "space-between", alignItems: "center" }}>
            <span style={{ fontSize: 12, fontWeight: 600, color: "var(--text-1)" }}>总计</span>
            <span style={{ fontSize: 14, fontWeight: 700, color: "var(--accent)" }}>¥{totalCost.toFixed(4)}</span>
          </div>
        </div>
      </div>

      {/* 缓存分析 */}
      <div className="card" style={{ padding: 10 }}>
        <div style={{ fontSize: 12, fontWeight: 600, color: "var(--text-1)", marginBottom: 6 }}>缓存分析</div>
        <div style={{ height: 8, borderRadius: 4, background: "var(--bg-3)", overflow: "hidden", display: "flex" }}>
          <div style={{ width: `${cacheRate}%`, background: "var(--green)", transition: "width 0.3s" }} />
          <div style={{ width: `${100 - cacheRate}%`, background: "var(--amber)", transition: "width 0.3s" }} />
        </div>
        <div style={{ display: "flex", justifyContent: "space-between", marginTop: 6, fontSize: 10, color: "var(--text-3)" }}>
          <span>🟢 缓存命中 {cacheRate.toFixed(1)}%</span>
          <span>🟡 未缓存 {(100 - cacheRate).toFixed(1)}%</span>
        </div>
        <div style={{ fontSize: 10, color: "var(--text-muted)", marginTop: 6 }}>
          缓存命中部分按 10% 计费，节省约 {cacheRate > 0 ? ((1 - cacheRate / 100 * 0.1 / (cacheRate / 100 * 0.1 + 1)) * 100).toFixed(0) : 0}% 费用
        </div>
      </div>
    </div>
  );
}

/* === 关于 === */
function AboutSettings({ capabilities }: any) {
  const [checkingUpdate, setCheckingUpdate] = useState(false);
  const [updateAvailable, setUpdateAvailable] = useState(false);

  const checkUpdate = () => {
    setCheckingUpdate(true);
    setTimeout(() => {
      setCheckingUpdate(false);
      setUpdateAvailable(false);
    }, 2000);
  };

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 10, alignItems: "center", textAlign: "center" }}>
      <div style={{ fontSize: 32, marginBottom: 4 }}>📋🤖</div>
      <div style={{ fontSize: 16, fontWeight: 700, color: "var(--text-1)" }}>DeepseekNova</div>
      <div style={{ fontSize: 11, color: "var(--text-3)" }}>
        版本 v{capabilities?.version || "0.3.0"}
      </div>
      <div style={{ fontSize: 11, color: "var(--text-2)", maxWidth: 320, lineHeight: 1.6, marginTop: 4 }}>
        DeepSeek 原生桌面端 AI 编程助手，围绕 Prefix-Cache 机制深度优化，极致缓存命中率。
      </div>

      <button className="btn btn-primary" onClick={checkUpdate} disabled={checkingUpdate} style={{ marginTop: 8 }}>
        {checkingUpdate ? "检查中…" : "检查更新"}
      </button>
      {updateAvailable && (
        <div style={{ fontSize: 11, color: "var(--green)" }}>发现新版本！点击下载</div>
      )}

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

/* === 辅助组件 === */
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

function FeeRow({ label, tokens, price, cost }: { label: string; tokens: number; price: number; cost: number }) {
  return (
    <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", fontSize: 11 }}>
      <span style={{ color: "var(--text-2)" }}>{label}</span>
      <span style={{ color: "var(--text-3)", fontSize: 10 }}>{tokens.toLocaleString()} tok × ¥{price}/M</span>
      <span style={{ color: "var(--text-1)", fontWeight: 500, minWidth: 60, textAlign: "right" }}>¥{cost.toFixed(4)}</span>
    </div>
  );
}
