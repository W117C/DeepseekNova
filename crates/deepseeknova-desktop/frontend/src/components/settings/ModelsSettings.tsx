/**
 * ModelsSettings.tsx — 模型：提供商列表 + API Key + 思考模式（list_providers）
 */

import { useEffect, useState } from "react";
import { listProviders } from "../../bridge";
import type { ProviderSummary } from "../../types";
import { useStore } from "../../store";
import { SectionHeader, SettingRow } from "./Shared";

export default function ModelsSettings() {
  const [providers, setProviders] = useState<ProviderSummary[]>([]);
  const capabilities = useStore((s) => s.capabilities);
  const model = useStore((s) => s.model);

  useEffect(() => {
    listProviders().then(setProviders).catch(() => {});
  }, []);

  return (
    <div>
      <SectionHeader title="模型" desc="list_providers · get_capabilities" />

      {providers.map((p) => (
        <SettingRow key={p.name} label={p.model ?? p.name} desc={`${p.kind}${p.base_url ? ` · ${p.base_url}` : ""}`}>
          <span className="tag" style={p.model === model ? { color: "var(--accent)" } : undefined}>
            {p.model === model ? "当前" : "可用"}
          </span>
        </SettingRow>
      ))}
      {providers.length === 0 && (
        <SettingRow label="未发现提供商" desc="请检查 ~/.deepseeknova/config.toml 的 [[providers]] 配置">
          <span className="tag">—</span>
        </SettingRow>
      )}

      <SettingRow label="API Key" desc="环境变量 DEEPSEEKNOVA_API_KEY · 不明文存储">
        <span className="tag">{providers.length > 0 ? "已配置" : "未检测到"}</span>
      </SettingRow>
      <SettingRow label="思考模式 thinking" desc="支持 reasoning_content 流式回传">
        <span className="tag">{capabilities?.supports_thinking ? "支持" : "不支持"}</span>
      </SettingRow>
    </div>
  );
}
