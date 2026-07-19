/** Shared helper components for settings panels */

export function SettingRow({ label, desc, children }: { label: string; desc?: string; children: React.ReactNode }) {
  return (
    <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", padding: "6px 0", gap: 12 }}>
      <div style={{ flexShrink: 0 }}>
        <div style={{ fontSize: 12, fontWeight: 500, color: "var(--text-1)" }}>{label}</div>
        {desc && <div style={{ fontSize: 10, color: "var(--text-3)", marginTop: 2 }}>{desc}</div>}
      </div>
      <div style={{ flexShrink: 0 }}>{children}</div>
    </div>
  );
}

export function StatBox({ label, value, sub, color }: { label: string; value: string; sub?: string; color?: string }) {
  return (
    <div className="card" style={{ padding: "10px 14px", textAlign: "center", minWidth: 80 }}>
      <div style={{ fontSize: 9, color: "var(--text-3)", textTransform: "uppercase", letterSpacing: 0.5 }}>{label}</div>
      <div style={{ fontSize: 18, fontWeight: 700, color: color || "var(--text-1)", margin: "2px 0" }}>{value}</div>
      {sub && <div style={{ fontSize: 9, color: "var(--text-3)" }}>{sub}</div>}
    </div>
  );
}

export function Toggle({ checked, onChange, label }: { checked: boolean; onChange: () => void; label?: string }) {
  return (
    <label style={{ display: "inline-flex", alignItems: "center", gap: 6, cursor: "pointer" }}>
      <span
        onClick={onChange}
        style={{
          width: 32, height: 18, borderRadius: 9,
          background: checked ? "var(--accent)" : "var(--bg-3)",
          position: "relative", transition: "background 0.2s",
        }}
      >
        <span style={{
          position: "absolute", top: 2, left: checked ? 16 : 2,
          width: 14, height: 14, borderRadius: "50%",
          background: "white", transition: "left 0.2s",
        }} />
      </span>
      {label && <span style={{ fontSize: 11, color: "var(--text-2)" }}>{label}</span>}
    </label>
  );
}
