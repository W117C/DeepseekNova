/**
 * ModelSelector.tsx — 模型选择下拉
 */

import { useState } from "react";
import { useStore } from "../store";
import { useTheme } from "../store/theme";

export default function ModelSelector() {
  const model = useStore((s) => s.model);
  const setModel = useStore((s) => s.setModel);
  const displayMode = useTheme((s) => s.displayMode);
  const [open, setOpen] = useState(false);

  // DeepSeek 模型列表
  const models = [
    { id: "deepseek-v4-flash", label: "DeepSeek v4 Flash", desc: "快速响应，低成本" },
    { id: "deepseek-v4-pro", label: "DeepSeek v4 Pro", desc: "高级推理，复杂任务" },
    { id: "deepseek-coder", label: "DeepSeek Coder", desc: "代码专用模型" },
    { id: "deepseek-reasoner", label: "DeepSeek Reasoner", desc: "R1 推理模型" },
  ];

  const current = models.find((m) => m.id === model) || models[0];

  return (
    <div style={{ position: "relative" }}>
      <button
        className="btn btn-ghost"
        onClick={() => setOpen(!open)}
        style={{ gap: "4px", fontSize: "12px", padding: "4px 8px" }}
      >
        <span style={{ color: "var(--accent)" }}>●</span>
        {current?.label || model}
        <svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
          <polyline points="6 9 12 15 18 9"/>
        </svg>
      </button>

      {open && (
        <>
          <div
            style={{ position: "fixed", inset: 0, zIndex: 99 }}
            onClick={() => setOpen(false)}
          />
          <div
            style={{
              position: "absolute",
              top: "100%",
              left: 0,
              marginTop: "4px",
              background: "var(--bg-2)",
              border: "1px solid var(--border-1)",
              borderRadius: "var(--radius-md)",
              boxShadow: "var(--shadow-md)",
              zIndex: 100,
              overflow: "hidden",
              minWidth: "240px",
            }}
          >
            <div style={{ padding: "8px 12px", fontSize: "11px", color: "var(--text-3)", borderBottom: "1px solid var(--border-1)" }}>
              选择模型
            </div>
            {models.map((m) => (
              <div
                key={m.id}
                className="sidebar-item"
                style={{ padding: "10px 12px" }}
                onClick={() => {
                  setModel(m.id);
                  setOpen(false);
                }}
              >
                <div style={{ flex: 1 }}>
                  <div style={{ fontSize: "13px", color: "var(--text-1)" }}>{m.label}</div>
                  {displayMode === "text" && <div style={{ fontSize: "11px", color: "var(--text-3)" }}>{m.desc}</div>}
                </div>
                {m.id === model && (
                  <span style={{ color: "var(--accent)", fontSize: "12px" }}>✓</span>
                )}
              </div>
            ))}
          </div>
        </>
      )}
    </div>
  );
}
