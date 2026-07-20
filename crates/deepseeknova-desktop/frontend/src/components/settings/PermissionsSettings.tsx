import { useState, useEffect } from "react";
import { getPermissions, setPermissionRule, type PermissionRule } from "../../bridge";

export default function PermissionsSettings() {
  const [rules, setRules] = useState<PermissionRule[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    (async () => {
      try {
        const data = await getPermissions();
        setRules(data);
      } catch (e: any) {
        setError(String(e));
      } finally {
        setLoading(false);
      }
    })();
  }, []);

  const toggleRule = async (name: string) => {
    const rule = rules.find((r) => r.name === name);
    if (!rule) return;
    const newEnabled = !rule.enabled;
    // Optimistic update
    setRules((prev) => prev.map((r) => r.name === name ? { ...r, enabled: newEnabled } : r));
    try {
      await setPermissionRule(name, newEnabled);
    } catch (e: any) {
      // Revert on failure
      setRules((prev) => prev.map((r) => r.name === name ? { ...r, enabled: !newEnabled } : r));
      setError(`Failed to update "${name}": ${e}`);
    }
  };

  if (loading) {
    return <div style={{ padding: 20, fontSize: 11, color: "var(--text-3)" }}>加载权限配置中…</div>;
  }

  if (error) {
    return (
      <div>
        <div style={{ fontSize: 10, color: "var(--red)", marginBottom: 8 }}>加载失败: {error}</div>
        <button className="btn btn-primary" style={{ fontSize: 11 }} onClick={() => window.location.reload()}>重试</button>
      </div>
    );
  }

  return (
    <div>
      <div style={{ fontSize: 10, color: "var(--text-3)", marginBottom: 8 }}>
        当前生效的权限配置（共 {rules.length} 条，已启用 {rules.filter((r) => r.enabled).length} 条）
      </div>
      {rules.map((r) => (
        <div key={r.name} className="card" style={{ padding: "6px 8px", marginBottom: 4 }}>
          <div style={{ display: "flex", alignItems: "center", gap: 6 }}>
            <span
              className={`tag ${
                r.rule_type === "安全" ? "tag-red" :
                r.rule_type === "执行" ? "tag-amber" :
                r.rule_type === "Git" ? "tag-cyan" :
                r.rule_type === "网络" ? "tag-blue" :
                r.rule_type === "隐私" ? "tag-red" : "tag-blue"
              }`}
              style={{ fontSize: 9 }}
            >
              {r.rule_type}
            </span>
            <span style={{ fontSize: 12, fontWeight: 500, color: "var(--text-1)" }}>{r.name}</span>
            <label className="toggle-switch" style={{ marginLeft: "auto" }} onClick={(e) => e.stopPropagation()}>
              <input
                type="checkbox"
                checked={r.enabled}
                onChange={() => toggleRule(r.name)}
              />
              <span className="toggle-slider"></span>
            </label>
          </div>
          <div style={{ fontSize: 10, color: "var(--text-3)", marginTop: 3 }}>{r.description}</div>
        </div>
      ))}
    </div>
  );
}
