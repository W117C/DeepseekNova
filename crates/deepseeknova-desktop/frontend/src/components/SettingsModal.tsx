/**
 * SettingsModal.tsx — 设置中心（mockup 定稿：6 组 20+ 分区）
 *
 * 应用：通用 / 角色 / 外观 / 快捷键
 * 模型与执行：模型 / 推理参数 / 执行
 * 能力：工具 / MCP / 技能 / 子代理
 * 安全：沙箱 / 权限 / 网络 / 钩子
 * 数据：记忆 / 知识库
 * 系统：日志 / 触发 / 诊断 / 账单 / 更新
 */

import { useState } from "react";
import { useStore } from "../store";
import { useTheme } from "../store/theme";
import { useI18n } from "../i18n";

import GeneralSettings from "./settings/GeneralSettings";
import RoleSettings from "./settings/RoleSettings";
import AppearanceSettings from "./settings/AppearanceSettings";
import ShortcutsSettings from "./settings/ShortcutsSettings";
import ModelsSettings from "./settings/ModelsSettings";
import ReasoningSettings from "./settings/ReasoningSettings";
import ExecutionSettings from "./settings/ExecutionSettings";
import ToolsSettings from "./settings/ToolsSettings";
import MCPSettings from "./settings/MCPSettings";
import SkillsSettings from "./settings/SkillsSettings";
import SubAgentsSettings from "./settings/SubAgentsSettings";
import SandboxSettings from "./settings/SandboxSettings";
import PermissionsSettings from "./settings/PermissionsSettings";
import NetworkSettings from "./settings/NetworkSettings";
import HooksSettings from "./settings/HooksSettings";
import MemorySettings from "./settings/MemorySettings";
import KnowledgeSettings from "./settings/KnowledgeSettings";
import LogsSettings from "./settings/LogsSettings";
import TriggersSettings from "./settings/TriggersSettings";
import DiagnosticsSettings from "./settings/DiagnosticsSettings";
import BillingSettings from "./settings/BillingSettings";
import AboutSettings from "./settings/AboutSettings";

type SettingsSection =
  | "general" | "role" | "appearance" | "shortcuts"
  | "models" | "reasoning" | "execution"
  | "tools" | "mcp" | "skills" | "subagents"
  | "sandbox" | "permissions" | "network" | "hooks"
  | "memory" | "knowledge"
  | "logs" | "triggers" | "diagnostics" | "billing" | "about";

export default function SettingsModal() {
  const { t } = useI18n();
  const setShowSettings = useStore((s) => s.setShowSettings);
  const capabilities = useStore((s) => s.capabilities);
  const theme = useTheme((s) => s.theme);
  const setTheme = useTheme((s) => s.setTheme);
  const displayMode = useTheme((s) => s.displayMode);
  const toggleDisplayMode = useTheme((s) => s.toggleDisplayMode);
  const skills = useStore((s) => s.skills);

  const [section, setSection] = useState<SettingsSection>("general");

  const sections: { id: SettingsSection; label: string; group: string }[] = [
    { id: "general", label: t("ssec.general"), group: t("sgroup.app") },
    { id: "role", label: t("ssec.role"), group: t("sgroup.app") },
    { id: "appearance", label: t("ssec.appearance"), group: t("sgroup.app") },
    { id: "shortcuts", label: t("ssec.shortcuts"), group: t("sgroup.app") },
    { id: "models", label: t("ssec.models"), group: t("sgroup.modelExec") },
    { id: "reasoning", label: t("ssec.reasoning"), group: t("sgroup.modelExec") },
    { id: "execution", label: t("ssec.execution"), group: t("sgroup.modelExec") },
    { id: "tools", label: t("ssec.tools"), group: t("sgroup.capability") },
    { id: "mcp", label: t("ssec.mcp"), group: t("sgroup.capability") },
    { id: "skills", label: t("ssec.skills"), group: t("sgroup.capability") },
    { id: "subagents", label: t("ssec.subagents"), group: t("sgroup.capability") },
    { id: "sandbox", label: t("ssec.sandbox"), group: t("sgroup.security") },
    { id: "permissions", label: t("ssec.permissions"), group: t("sgroup.security") },
    { id: "network", label: t("ssec.network"), group: t("sgroup.security") },
    { id: "hooks", label: t("ssec.hooks"), group: t("sgroup.security") },
    { id: "memory", label: t("ssec.memory"), group: t("sgroup.data") },
    { id: "knowledge", label: t("ssec.knowledge"), group: t("sgroup.data") },
    { id: "logs", label: t("ssec.logs"), group: t("sgroup.system") },
    { id: "triggers", label: t("ssec.triggers"), group: t("sgroup.system") },
    { id: "diagnostics", label: t("ssec.diagnostics"), group: t("sgroup.system") },
    { id: "billing", label: t("ssec.billing"), group: t("sgroup.system") },
    { id: "about", label: t("ssec.update"), group: t("sgroup.system") },
  ];

  const groups = [...new Set(sections.map((s) => s.group))];

  return (
    <>
      <div className="modal-backdrop" onClick={() => setShowSettings(false)} />
      <div className="modal modal-centered" style={{ width: 920, height: 640, display: "flex" }}>
        {/* 左侧导航（6 组） */}
        <div className="settings-nav">
          {groups.map((g) => (
            <div key={g}>
              <div className="settings-nav-group">{g}</div>
              {sections.filter((s) => s.group === g).map((s) => (
                <div
                  key={s.id}
                  className={`settings-nav-item ${section === s.id ? "active" : ""}`}
                  onClick={() => setSection(s.id)}
                >
                  <span>{s.label}</span>
                </div>
              ))}
            </div>
          ))}
        </div>

        {/* 右侧内容 */}
        <div className="settings-content">
          {section === "general" && <GeneralSettings />}
          {section === "role" && <RoleSettings />}
          {section === "appearance" && (
            <AppearanceSettings theme={theme} setTheme={setTheme} displayMode={displayMode} toggleDisplayMode={toggleDisplayMode} />
          )}
          {section === "shortcuts" && <ShortcutsSettings />}
          {section === "models" && <ModelsSettings />}
          {section === "reasoning" && <ReasoningSettings />}
          {section === "execution" && <ExecutionSettings />}
          {section === "tools" && <ToolsSettings />}
          {section === "mcp" && <MCPSettings />}
          {section === "skills" && <SkillsSettings skills={skills} />}
          {section === "subagents" && <SubAgentsSettings />}
          {section === "sandbox" && <SandboxSettings />}
          {section === "permissions" && <PermissionsSettings />}
          {section === "network" && <NetworkSettings />}
          {section === "hooks" && <HooksSettings />}
          {section === "memory" && <MemorySettings />}
          {section === "knowledge" && <KnowledgeSettings />}
          {section === "logs" && <LogsSettings />}
          {section === "triggers" && <TriggersSettings />}
          {section === "diagnostics" && <DiagnosticsSettings />}
          {section === "billing" && <BillingSettings />}
          {section === "about" && <AboutSettings capabilities={capabilities} />}
        </div>
      </div>
    </>
  );
}
