/**
 * Welcome.tsx — 欢迎屏（空状态）
 */

import { useStore } from "../store";
import { useI18n } from "../i18n";

export default function Welcome() {
  const { t } = useI18n();
  const mode = useStore((s) => s.mode);
  const model = useStore((s) => s.model);

  const features = [
    { icon: "🧠", title: t("welcome.feat.memory"), desc: t("welcome.feat.memoryDesc") },
    { icon: "⚡", title: t("welcome.feat.skills"), desc: t("welcome.feat.skillsDesc") },
    { icon: "🔒", title: t("welcome.feat.sandbox"), desc: t("welcome.feat.sandboxDesc") },
    { icon: "📊", title: t("welcome.feat.token"), desc: t("welcome.feat.tokenDesc") },
  ];

  return (
    <div style={{ padding: "40px 20px", maxWidth: "640px", margin: "0 auto" }}>
      <h1 style={{
        fontSize: "28px",
        fontWeight: 700,
        background: "linear-gradient(135deg, var(--accent), var(--cyan))",
        WebkitBackgroundClip: "text",
        WebkitTextFillColor: "transparent",
        marginBottom: "8px",
      }}>
        DeepseekNova
      </h1>
      <p style={{ color: "var(--text-2)", fontSize: "14px", marginBottom: "24px" }}>
        {t("welcome.subtitle")} · {model} · {mode}
      </p>

      <div style={{
        display: "grid",
        gridTemplateColumns: "1fr 1fr",
        gap: "12px",
      }}>
        {features.map((f) => (
          <div key={f.title} className="card" style={{ padding: "16px" }}>
            <div style={{ fontSize: "20px", marginBottom: "4px" }}>{f.icon}</div>
            <div style={{ fontSize: "13px", fontWeight: 600, color: "var(--text-1)", marginBottom: "2px" }}>
              {f.title}
            </div>
            <div style={{ fontSize: "12px", color: "var(--text-3)" }}>
              {f.desc}
            </div>
          </div>
        ))}
      </div>

      <div style={{
        marginTop: "24px",
        padding: "12px 16px",
        background: "var(--bg-2)",
        border: "1px solid var(--border-1)",
        borderRadius: "var(--radius-md)",
      }}>
        <div style={{ fontSize: "12px", color: "var(--text-3)", marginBottom: "4px" }}>{t("welcome.quickStart")}</div>
        <div style={{ fontSize: "13px", color: "var(--text-2)" }}>
          {t("welcome.quickStartHint")}
        </div>
      </div>
    </div>
  );
}
