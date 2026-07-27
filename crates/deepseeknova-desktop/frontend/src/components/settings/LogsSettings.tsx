/**
 * LogsSettings.tsx — 日志与可观测性（get/set_log_config · export_logs）
 */

import { useEffect, useState } from "react";
import { getLogConfig, setLogConfig, exportLogs, type LogConfig } from "../../bridge";
import { SectionHeader, SettingRow, Toggle } from "./Shared";

const LEVELS = ["debug", "info", "warn", "error"];

const DEFAULT_CONFIG: LogConfig = { level: "info", otel_enabled: false, audit_enabled: true };

export default function LogsSettings() {
  const [config, setConfig] = useState<LogConfig | null>(null);
  const [exported, setExported] = useState<string | null>(null);

  useEffect(() => {
    // 非 Tauri 环境 / 命令失败时回退默认值
    getLogConfig().then(setConfig).catch(() => setConfig(DEFAULT_CONFIG));
  }, []);

  const update = async (patch: Partial<LogConfig>) => {
    if (!config) return;
    const next = { ...config, ...patch };
    setConfig(next);
    try {
      await setLogConfig(next);
    } catch {
      setConfig(config); // 回滚
    }
  };

  const doExport = async () => {
    try {
      setExported(await exportLogs());
    } catch (e) {
      setExported(`导出失败：${e}`);
    }
  };

  if (!config) return <SectionHeader title="日志" desc="加载中…" />;

  return (
    <div>
      <SectionHeader title="日志" desc="tracing + OpenTelemetry（deepseeknova-telemetry）· 改动重启后生效" />

      <SettingRow label="日志级别" desc="debug / info / warn / error">
        <div className="seg">
          {LEVELS.map((l) => (
            <button key={l} className={config.level === l ? "on" : ""} onClick={() => update({ level: l })}>
              {l}
            </button>
          ))}
        </div>
      </SettingRow>
      <SettingRow label="OpenTelemetry 追踪" desc="完整链路：prompt · 工具调用 · 耗时">
        <Toggle checked={config.otel_enabled} onChange={() => update({ otel_enabled: !config.otel_enabled })} />
      </SettingRow>
      <SettingRow label="审计日志" desc="记录工具调用与审批决策，可追溯">
        <Toggle checked={config.audit_enabled} onChange={() => update({ audit_enabled: !config.audit_enabled })} />
      </SettingRow>
      <SettingRow label="导出" desc="日志与配置打包到临时目录">
        <button className="btn" onClick={doExport}>导出</button>
      </SettingRow>
      {exported && (
        <div style={{ fontSize: 11, color: "var(--text-2)", fontFamily: "var(--font-mono)", marginTop: 8, wordBreak: "break-all" }}>
          {exported}
        </div>
      )}
    </div>
  );
}
