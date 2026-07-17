/**
 * DiffView — Lazy-loadable unified diff view component.
 *
 * Default implementation renders a unified diff with +/- highlighting.
 * Can be swapped to Monaco/CodeMirror diff editor.
 *
 * Usage:
 *   const DiffView = React.lazy(() => import("./components/editors/DiffView"));
 *   <React.Suspense fallback={<div>loading diff...</div>}>
 *     <DiffView diff={unifiedDiffText} />
 *   </React.Suspense>
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
    background: "#1e1e1e",
    borderRadius: "6px",
    fontSize: "13px",
    lineHeight: 1.5,
    overflow: "auto",
    fontFamily: "'Cascadia Code', 'Fira Code', 'JetBrains Mono', 'Consolas', monospace",
    ...(maxHeight ? { maxHeight } : {}),
  };

  return (
    <pre style={containerStyle}>
      {lines.map((line, i) => {
        let bg = "transparent";
        let color = "#d4d4d4";
        let prefix = " ";

        if (line.startsWith("+") && !line.startsWith("+++")) {
          bg = "#1a3a1a";
          color = "#a3d6a3";
          prefix = "+";
        } else if (line.startsWith("-") && !line.startsWith("---")) {
          bg = "#3a1a1a";
          color = "#d6a3a3";
          prefix = "-";
        } else if (line.startsWith("@")) {
          bg = "#2a2a3a";
          color = "#6a9fb5";
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
