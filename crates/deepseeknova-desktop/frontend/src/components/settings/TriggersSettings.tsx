/**
 * TriggersSettings.tsx — 触发与调度（get/set_triggers · 一期仅配置持久化）
 */

import { useEffect, useState } from "react";
import { getTriggers, setTriggers, type TriggerConfig } from "../../bridge";
import { SectionHeader, SettingRow, Toggle } from "./Shared";

const DEFAULT_CONFIG: TriggerConfig = {
  http_api_enabled: false, schedules: [], webhook_enabled: false, max_concurrent: 1,
};

export default function TriggersSettings() {
  const [config, setConfig] = useState<TriggerConfig | null>(null);

  useEffect(() => {
    // 非 Tauri 环境 / 命令失败时回退默认值
    getTriggers().then(setConfig).catch(() => setConfig(DEFAULT_CONFIG));
  }, []);

  const update = async (patch: Partial<TriggerConfig>) => {
    if (!config) return;
    const next = { ...config, ...patch };
    setConfig(next);
    try {
      await setTriggers(next);
    } catch {
      setConfig(config); // 回滚
    }
  };

  if (!config) return <SectionHeader title="触发" desc="加载中…" />;

  return (
    <div>
      <SectionHeader title="触发" desc="手动 / API / 定时 · 配置已持久化，运行时调度即将支持" />

      <SettingRow label="HTTP API" desc="deepseeknova-serve 对外服务（即将支持）">
        <Toggle checked={config.http_api_enabled} onChange={() => update({ http_api_enabled: !config.http_api_enabled })} />
      </SettingRow>
      <SettingRow label="定时任务" desc="cron 表达式触发会话（即将支持）">
        <span className="tag">{config.schedules.length} 条</span>
      </SettingRow>
      <SettingRow label="Webhook" desc="外部事件触发（即将支持）">
        <Toggle checked={config.webhook_enabled} onChange={() => update({ webhook_enabled: !config.webhook_enabled })} />
      </SettingRow>
      <SettingRow label="并发实例" desc="同一 agent 并行运行上限">
        <input
          className="input"
          style={{ width: 70, textAlign: "right" }}
          type="number"
          min="1"
          value={config.max_concurrent}
          onChange={(e) => update({ max_concurrent: Math.max(1, Number(e.target.value)) })}
        />
      </SettingRow>
    </div>
  );
}
