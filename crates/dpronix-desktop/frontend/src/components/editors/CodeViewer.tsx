/**
 * CodeViewer — Lazy-loadable code viewer component.
 *
 * Default implementation renders code in a <pre><code> block.
 * Can be swapped to Monaco/CodeMirror by changing the lazy import.
 *
 * Usage:
 *   const CodeViewer = React.lazy(() => import("./components/editors/CodeViewer"));
 *   <React.Suspense fallback={<pre>loading editor...</pre>}>
 *     <CodeViewer code="..." language="rust" />
 *   </React.Suspense>
 */

import React from "react";

interface CodeViewerProps {
  code: string;
  language?: string;
  maxHeight?: string;
}

const CodeViewer: React.FC<CodeViewerProps> = ({ code, language, maxHeight }) => {
  const ext = language ?? "";
  const style: React.CSSProperties = {
    margin: 0,
    padding: "12px",
    background: "#1e1e1e",
    color: "#d4d4d4",
    borderRadius: "6px",
    fontSize: "13px",
    lineHeight: 1.5,
    overflow: "auto",
    whiteSpace: "pre-wrap",
    wordBreak: "break-all",
    fontFamily: "'Cascadia Code', 'Fira Code', 'JetBrains Mono', 'Consolas', monospace",
    ...(maxHeight ? { maxHeight } : {}),
  };

  return (
    <pre style={style}>
      <code>{code}</code>
    </pre>
  );
};

export default CodeViewer;
