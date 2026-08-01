/**
 * ToolsSettings.tsx — 工具：内置工具开关（list_tools / set_tool_enabled）
 */

import { useEffect, useState } from "react";
import { listTools, setToolEnabled, type ToolInfo } from "../../bridge";
import { SectionHeader, SettingRow, Toggle } from "./Shared";

export default function ToolsSettings() {
  const [tools, setTools] = useState<ToolInfo[]>([]);

  useEffect(() => {
    listTools().then(setTools).catch(() => {});
  }, []);

  const toggle = async (name: string) => {
    const tool = tools.find((t) => t.name === name);
    if (!tool) return;
    const enabled = !tool.enabled;
    setTools((ts) => ts.map((t) => (t.name === name ? { ...t, enabled } : t)));
    try {
      await setToolEnabled(name, enabled);
    } catch {
      // 回滚
      setTools((ts) => ts.map((t) => (t.name === name ? { ...t, enabled: !enabled } : t)));
    }
  };

  return (
    <div>
      <SectionHeader title="工具" desc={`内置 ${tools.length} 工具（deepseeknova-tools）· list_tools / set_tool_enabled`} />
      {tools.map((t) => (
        <SettingRow key={t.name} label={t.name} desc={t.description}>
          <Toggle checked={t.enabled} onChange={() => toggle(t.name)} />
        </SettingRow>
      ))}
      <SettingRow label="单轮调用上限" desc="超出即终止本轮（执行分区 max_steps）">
        <span className="tag">25 次</span>
      </SettingRow>
      <SettingRow label="结果摘要" desc="超长工具输出自动蒸馏后入上下文">
        <span className="tag">开</span>
      </SettingRow>
    </div>
  );
}
