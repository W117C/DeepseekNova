import type { ContextFile, FileChangeType } from "../types";

function badge(ct?: FileChangeType) { return ct === "added" ? "+" : ct === "removed" ? "-" : "M"; }
function badgeClass(ct?: FileChangeType) { return ct ?? "modified"; }

interface ContextPanelProps {
  files: ContextFile[];
  modified: ContextFile[];
  memoryCount: number;
  collapsed?: boolean;
}

export default function ContextPanel({ files, modified, memoryCount, collapsed }: ContextPanelProps) {
  if (collapsed) return null;
  return (
    <div className="context-panel">
      <div className="section">
        <p className="heading">Files in Context</p>
        <div className="file-list">{files.map((f) => <span key={f.path} className="file">{f.path}</span>)}</div>
      </div>
      <div className="section">
        <p className="heading">Modified</p>
        <div className="file-list">{modified.map((f) => <span key={f.path} className={`file ${badgeClass(f.changeType)}`}>{badge(f.changeType)} {f.path}</span>)}</div>
      </div>
      <div className="section">
        <p className="heading">Memory</p>
        <p className="memory-count">{memoryCount} entries</p>
      </div>
    </div>
  );
}
