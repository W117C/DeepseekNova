/**
 * ReasoningSettings.tsx — 推理参数：采样 / 降级 / 重试（get/set_reasoning_params）
 */

import { useEffect, useState } from "react";
import { getReasoningParams, setReasoningParams, type ReasoningParams } from "../../bridge";
import { SectionHeader, SettingRow } from "./Shared";

const numInput = { width: 90, textAlign: "right" as const };

const DEFAULT_PARAMS: ReasoningParams = {
  temperature: 0.7, top_p: 0.95, max_tokens: 8192,
  stop_sequences: [], fallback_model: null, timeout_secs: 60, max_retries: 2,
};

export default function ReasoningSettings() {
  const [params, setParams] = useState<ReasoningParams | null>(null);
  const [saved, setSaved] = useState<null | "ok" | string>(null);

  useEffect(() => {
    // 非 Tauri 环境 / 命令失败时回退默认值，避免永久「加载中」
    getReasoningParams().then(setParams).catch(() => setParams(DEFAULT_PARAMS));
  }, []);

  const patch = (p: Partial<ReasoningParams>) =>
    setParams((prev) => (prev ? { ...prev, ...p } : prev));

  const save = async () => {
    if (!params) return;
    try {
      await setReasoningParams(params);
      setSaved("ok");
    } catch (e) {
      setSaved(String(e));
    }
    setTimeout(() => setSaved(null), 2500);
  };

  if (!params) return <SectionHeader title="推理参数" desc="加载中…" />;

  return (
    <div>
      <SectionHeader title="推理参数" desc="采样与降级 · 按模型覆盖 · get/set_reasoning_params" />

      <SettingRow label="temperature" desc="随机性（0.0 – 2.0）">
        <input className="input" style={numInput} type="number" step="0.1" min="0" max="2"
          value={params.temperature}
          onChange={(e) => patch({ temperature: Number(e.target.value) })} />
      </SettingRow>
      <SettingRow label="top_p" desc="核采样（0.0 – 1.0）">
        <input className="input" style={numInput} type="number" step="0.05" min="0" max="1"
          value={params.top_p}
          onChange={(e) => patch({ top_p: Number(e.target.value) })} />
      </SettingRow>
      <SettingRow label="max_tokens" desc="单次回复上限">
        <input className="input" style={numInput} type="number" min="1"
          value={params.max_tokens}
          onChange={(e) => patch({ max_tokens: Number(e.target.value) })} />
      </SettingRow>
      <SettingRow label="停止序列" desc="stop sequences，逗号分隔">
        <input className="input" style={{ width: 180 }}
          value={params.stop_sequences.join(",")}
          onChange={(e) => patch({ stop_sequences: e.target.value.split(",").map((s) => s.trim()).filter(Boolean) })} />
      </SettingRow>
      <SettingRow label="失败降级 fallback" desc="主模型不可用时自动切换（留空不降级）">
        <input className="input" style={{ width: 180 }}
          placeholder="deepseek-v4-flash"
          value={params.fallback_model ?? ""}
          onChange={(e) => patch({ fallback_model: e.target.value.trim() || null })} />
      </SettingRow>
      <SettingRow label="超时 / 重试" desc="模型请求级">
        <span style={{ display: "inline-flex", gap: 6, alignItems: "center" }}>
          <input className="input" style={numInput} type="number" min="1"
            value={params.timeout_secs}
            onChange={(e) => patch({ timeout_secs: Number(e.target.value) })} />
          <span style={{ fontSize: 11, color: "var(--text-3)" }}>s ·</span>
          <input className="input" style={{ width: 60, textAlign: "right" }} type="number" min="0"
            value={params.max_retries}
            onChange={(e) => patch({ max_retries: Number(e.target.value) })} />
          <span style={{ fontSize: 11, color: "var(--text-3)" }}>次</span>
        </span>
      </SettingRow>

      <div style={{ display: "flex", gap: 8, marginTop: 12, alignItems: "center" }}>
        <button className="btn btn-primary" onClick={save}>保存</button>
        {saved === "ok" && <span style={{ color: "var(--green)", fontSize: 12 }}>✓ 已保存</span>}
        {saved && saved !== "ok" && <span style={{ color: "var(--red)", fontSize: 12 }}>{saved}</span>}
      </div>
    </div>
  );
}
