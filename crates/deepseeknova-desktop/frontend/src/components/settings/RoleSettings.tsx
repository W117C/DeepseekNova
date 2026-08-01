/**
 * RoleSettings.tsx — 角色：系统提示词 / 人设与行为准则（get/set_system_prompt）
 */

import { useEffect, useState } from "react";
import { getSystemPrompt, setSystemPrompt } from "../../bridge";
import { SectionHeader, SettingRow } from "./Shared";

export default function RoleSettings() {
  const [prompt, setPrompt] = useState("");
  const [saved, setSaved] = useState<null | "ok" | "err">(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    getSystemPrompt()
      .then((p) => setPrompt(p))
      .catch(() => {})
      .finally(() => setLoading(false));
  }, []);

  const save = async () => {
    try {
      await setSystemPrompt(prompt);
      setSaved("ok");
    } catch {
      setSaved("err");
    }
    setTimeout(() => setSaved(null), 2000);
  };

  return (
    <div>
      <SectionHeader title="角色" desc="人设、行为准则与能力边界 · get/set_system_prompt" />

      <div style={{ marginBottom: 12 }}>
        <div className="setting-row-label" style={{ marginBottom: 6 }}>系统提示词</div>
        <textarea
          className="input"
          style={{ minHeight: 160, resize: "vertical", fontFamily: "var(--font-mono)", fontSize: 12 }}
          placeholder={loading ? "加载中…" : "定义 agent 的人设、行为准则与能力边界…"}
          value={prompt}
          onChange={(e) => setPrompt(e.target.value)}
          disabled={loading}
        />
        <div style={{ display: "flex", gap: 8, marginTop: 8, alignItems: "center" }}>
          <button className="btn btn-primary" onClick={save} disabled={loading}>保存</button>
          {saved === "ok" && <span style={{ color: "var(--green)", fontSize: 12 }}>✓ 已保存</span>}
          {saved === "err" && <span style={{ color: "var(--red)", fontSize: 12 }}>保存失败</span>}
        </div>
      </div>

      <SettingRow label="响应语言" desc="默认输出语言">
        <span className="tag">跟随界面 · 中文</span>
      </SettingRow>
      <SettingRow label="语气风格" desc="简洁 / 详尽 / 教学（写入系统提示词生效）">
        <span className="tag">简洁</span>
      </SettingRow>
      <SettingRow label="时区" desc="影响时间类回答与定时任务">
        <span className="tag">{Intl.DateTimeFormat().resolvedOptions().timeZone}</span>
      </SettingRow>
    </div>
  );
}
