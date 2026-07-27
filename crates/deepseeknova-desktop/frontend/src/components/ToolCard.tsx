/**
 * ToolCard.tsx — 工具调用（mockup 定稿：单行摘要折叠行 + 状态/耗时徽章）
 */

import { useState } from "react";
import type { Message } from "../types";

function argsSummary(args?: string): string {
  if (!args) return "";
  const s = args.replace(/\s+/g, " ").trim();
  return s.length > 60 ? s.slice(0, 60) + "…" : s;
}

export default function ToolCard({ message }: { message: Message }) {
  const [expanded, setExpanded] = useState(false);

  const hasResult = !!message.toolResult;
  const durSec =
    message.startTs && message.endTs
      ? ((message.endTs - message.startTs) / 1000).toFixed(1)
      : null;

  return (
    <div className={`row-fold thread-inset ${expanded ? "open" : ""}`}>
      <div className="row-fold-h" onClick={() => setExpanded(!expanded)}>
        <span className="tri">▶</span>
        <span className="lbl mono">{message.toolName || "tool"}</span>
        <span className="meta mono">{argsSummary(message.toolArgs)}</span>
        <span className="right">
          {hasResult ? (
            <span className="stx ok">完成{durSec ? ` ${durSec}s` : ""}</span>
          ) : (
            <span className="ring act" />
          )}
        </span>
      </div>
      <div className="row-fold-b">
        {message.toolArgs && (
          <pre className="code-block-content mono" style={{ borderRadius: "7px", maxHeight: 150, color: "var(--text-3)" }}>
            {message.toolArgs}
          </pre>
        )}
        {message.toolResult && (
          <pre className="code-block-content mono" style={{ borderRadius: "7px", maxHeight: 150, marginTop: 8 }}>
            {message.toolResult.length > 2000
              ? message.toolResult.slice(0, 2000) + "\n… (已截断)"
              : message.toolResult}
          </pre>
        )}
      </div>
    </div>
  );
}
