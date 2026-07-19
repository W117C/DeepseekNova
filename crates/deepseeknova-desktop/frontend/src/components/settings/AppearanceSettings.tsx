import { useState } from "react";
import { SettingRow } from "./Shared";

export default function AppearanceSettings({ theme, setTheme, displayMode, toggleDisplayMode }: any) {
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

