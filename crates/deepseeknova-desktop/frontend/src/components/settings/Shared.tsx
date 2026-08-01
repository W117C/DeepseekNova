/** Shared helper components for settings panels */

export function SettingRow({ label, desc, children }: { label: string; desc?: string; children: React.ReactNode }) {
  return (
    <div className="setting-row">
      <div style={{ flexShrink: 0 }}>
        <div className="setting-row-label">{label}</div>
        {desc && <div className="setting-row-desc">{desc}</div>}
      </div>
      <div className="setting-row-control">{children}</div>
    </div>
  );
}

export function StatBox({ label, value, sub, color }: { label: string; value: string; sub?: string; color?: string }) {
  return (
    <div className="stat-box">
      <div className="stat-box-label">{label}</div>
      <div className="stat-box-value" style={color ? { color } : undefined}>{value}</div>
      {sub && <div className="stat-box-sub">{sub}</div>}
    </div>
  );
}

export function Toggle({ checked, onChange, label }: { checked: boolean; onChange: () => void; label?: string }) {
  return (
    <label className="toggle-switch">
      <input type="checkbox" checked={checked} onChange={onChange} />
      <span className="toggle-slider" onClick={onChange} />
      {label && <span style={{ fontSize: 11, color: "var(--text-2)", marginLeft: 6 }}>{label}</span>}
    </label>
  );
}

export function SectionHeader({ title, desc }: { title: string; desc?: string }) {
  return (
    <div style={{ marginBottom: 12 }}>
      <div className="settings-section-title">{title}</div>
      {desc && <div className="settings-section-desc">{desc}</div>}
    </div>
  );
}
