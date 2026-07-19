/**
 * SettingsModal.tsx — 设置面板（完整版）
 *
 * 14 大分区，每个分区独立组件，位于 ./settings/ 目录
 */

import { useState } from "react";
import { useStore } from "../store";
import { useTheme } from "../store/theme";

import GeneralSettings from "./settings/GeneralSettings";
import AppearanceSettings from "./settings/AppearanceSettings";
import ExecutionSettings from "./settings/ExecutionSettings";
import ShortcutsSettings from "./settings/ShortcutsSettings";
import SandboxSettings from "./settings/SandboxSettings";
import NetworkSettings from "./settings/NetworkSettings";
import PermissionsSettings from "./settings/PermissionsSettings";
import HooksSettings from "./settings/HooksSettings";
import MCPSettings from "./settings/MCPSettings";
import SubAgentsSettings from "./settings/SubAgentsSettings";
import SkillsSettings from "./settings/SkillsSettings";
import DiagnosticsSettings from "./settings/DiagnosticsSettings";
import BillingSettings from "./settings/BillingSettings";
import AboutSettings from "./settings/AboutSettings";

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
    { id: "mcp", label: "插件 (MCP)", icon: "🔌", group: "扩展" },
    { id: "subagents", label: "子智能体", icon: "🤖", group: "扩展" },
    { id: "skills", label: "技能", icon: "📦", group: "扩展" },
    { id: "diagnostics", label: "诊断", icon: "🩺", group: "工具" },
    { id: "billing", label: "账单", icon: "💰", group: "工具" },
    { id: "about", label: "关于", icon: "ℹ️", group: "工具" },
  ];

  const groups = [...new Set(sections.map((s) => s.group))];

  return (
    <>
      <div className="modal-backdrop" onClick={() => setShowSettings(false)} />
      <div className="modal" style={{ width: 920, height: 640, display: "flex" }}>
        {/* 左侧导航 */}
        <div className="settings-nav" style={{ width: 200, flexShrink: 0, borderRight: "1px solid var(--border)", overflowY: "auto", padding: "8px 0" }}>
          {groups.map((g) => (
            <div key={g}>
              <div style={{ fontSize: 10, fontWeight: 700, color: "var(--text-3)", padding: "8px 16px 4px", textTransform: "uppercase", letterSpacing: 0.5 }}>
                {g}
              </div>
              {sections.filter((s) => s.group === g).map((s) => (
                <div
                  key={s.id}
                  onClick={() => setSection(s.id)}
                  style={{
                    display: "flex", alignItems: "center", gap: 8,
                    padding: "6px 16px", cursor: "pointer", fontSize: 12,
                    color: section === s.id ? "var(--accent)" : "var(--text-2)",
                    background: section === s.id ? "var(--bg-hover)" : "transparent",
                    borderLeft: section === s.id ? "2px solid var(--accent)" : "2px solid transparent",
                  }}
                >
                  <span>{s.icon}</span>
                  <span>{s.label}</span>
                </div>
              ))}
            </div>
          ))}
        </div>

        {/* 右侧内容 */}
        <div style={{ flex: 1, overflowY: "auto", padding: 20 }}>
          {section === "general" && <GeneralSettings />}
          {section === "appearance" && (
            <AppearanceSettings theme={theme} setTheme={setTheme} displayMode={displayMode} toggleDisplayMode={toggleDisplayMode} />
          )}
          {section === "execution" && <ExecutionSettings />}
          {section === "shortcuts" && <ShortcutsSettings />}
          {section === "sandbox" && <SandboxSettings />}
          {section === "network" && <NetworkSettings />}
          {section === "permissions" && <PermissionsSettings />}
          {section === "hooks" && <HooksSettings />}
          {section === "mcp" && <MCPSettings />}
          {section === "subagents" && <SubAgentsSettings />}
          {section === "skills" && <SkillsSettings skills={skills} />}
          {section === "diagnostics" && <DiagnosticsSettings />}
          {section === "billing" && <BillingSettings />}
          {section === "about" && <AboutSettings capabilities={capabilities} />}
        </div>
      </div>
    </>
  );
}
