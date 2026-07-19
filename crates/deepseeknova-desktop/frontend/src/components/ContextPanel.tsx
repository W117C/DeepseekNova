/**
 * ContextPanel — right-side panel: context files, modified files, memory.
 */
import type { ContextFile, FileChangeType } from "../types";

function badge(ct?: FileChangeType) {
  return ct === "added" ? "+" : ct === "removed" ? "-" : "M";
}
function badgeClass(ct?: FileChangeType) {
  return ct ?? "modified";
}

interface ContextPanelProps {
  files: ContextFile[];
  modified: ContextFile[];
  memoryCount: number;
  collapsed?: boolean;
}

export default function ContextPanel({
  files,
  modified,
  memoryCount,
  collapsed,
}: ContextPanelProps) {
  if (collapsed) return null;
  return (
    <aside className="dp-context">
      <div className="section">
        <p className="heading">Files in Context</p>
        {files.length > 0 ? (
          files.map((f) => (
            <div key={f.path} className="file">
              <span className="path">{f.path}</span>
            </div>
          ))
        ) : (
          <p className="dp-empty" style={{ padding: "8px 6px" }}>
            No files in context.
          </p>
        )}
      </div>

      <div className="section">
        <p className="heading">Modified</p>
        {modified.length > 0 ? (
          modified.map((f) => (
            <div key={f.path} className="file">
              <span className={`badge ${badgeClass(f.changeType)}`}>
                {badge(f.changeType)}
              </span>
              <span className="path">{f.path}</span>
            </div>
          ))
        ) : (
          <p className="dp-empty" style={{ padding: "8px 6px" }}>
            No modified files.
          </p>
        )}
      </div>

      <div className="section">
        <p className="heading">Memory</p>
        <span className="count">{memoryCount} entries</span>
      </div>
    </aside>
  );
}
