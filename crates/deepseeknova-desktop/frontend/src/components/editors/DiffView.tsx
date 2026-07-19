/**
 * DiffView — Lazy-loadable unified diff view component.
 *
 * Uses dp-* CSS variables for theme-aware colors.
 */

import React from "react";

interface DiffViewProps {
  diff: string;
  maxHeight?: string;
}

const DiffView: React.FC<DiffViewProps> = ({ diff, maxHeight }) => {
  const lines = diff.split("\n");

  const containerStyle: React.CSSProperties = {
    margin: 0,
    padding: "8px 0",
    background: "var(--dp-panel)",
    borderRadius: "var(--dp-radius-sm)",
    fontSize: "13px",
    lineHeight: 1.5,
    overflow: "auto",
    fontFamily: "var(--dp-font-mono)",
    ...(maxHeight ? { maxHeight } : {}),
  };

  return (
    <pre style={containerStyle}>
      {lines.map((line, i) => {
        let bg = "transparent";
        let color = "var(--dp-fg)";
        let prefix = " ";

        if (line.startsWith("+") && !line.startsWith("+++")) {
          bg = "color-mix(in srgb, var(--dp-success) 18%, transparent)";
          color = "var(--dp-success)";
          prefix = "+";
        } else if (line.startsWith("-") && !line.startsWith("---")) {
          bg = "color-mix(in srgb, var(--dp-danger) 18%, transparent)";
          color = "var(--dp-danger)";
          prefix = "-";
        } else if (line.startsWith("@")) {
          bg = "color-mix(in srgb, var(--dp-cyan) 14%, transparent)";
          color = "var(--dp-cyan)";
          prefix = "@";
        }

        return (
          <div
            key={i}
            style={{
              background: bg,
              color,
              padding: "0 16px",
              whiteSpace: "pre-wrap",
              wordBreak: "break-all",
              display: "flex",
            }}
          >
            <span style={{ userSelect: "none", width: 20, flexShrink: 0 }}>{prefix}</span>
            <span>{line.length > 1 ? line.slice(1) : ""}</span>
          </div>
        );
      })}
    </pre>
  );
};

export default DiffView;
