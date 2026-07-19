/**
 * CodeViewer — Lazy-loadable code viewer component.
 *
 * Uses dp-* CSS variables instead of hardcoded dark-mode colors,
 * so it automatically adapts to light/dark themes.
 */

import React from "react";

interface CodeViewerProps {
  code: string;
  language?: string;
  maxHeight?: string;
}

const style: React.CSSProperties = {
  margin: 0,
  padding: "12px",
  background: "var(--dp-panel)",
  color: "var(--dp-fg)",
  borderRadius: "var(--dp-radius-sm)",
  fontSize: "13px",
  lineHeight: 1.5,
  overflow: "auto",
  whiteSpace: "pre-wrap",
  wordBreak: "break-all",
  fontFamily: "var(--dp-font-mono)",
};

const CodeViewer: React.FC<CodeViewerProps> = ({ code, maxHeight }) => {
  return (
    <pre style={{ ...style, ...(maxHeight ? { maxHeight } : {}) }}>
      <code>{code}</code>
    </pre>
  );
};

export default CodeViewer;
