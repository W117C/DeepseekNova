/**
 * ReasoningCard.tsx — 推理过程（mockup 定稿：无边框灰字折叠行，默认折叠）
 */

import { useState } from "react";
import type { Message } from "../types";

export default function ReasoningCard({ message }: { message: Message }) {
  const [expanded, setExpanded] = useState(false);

  return (
    <div className={`row-fold reason thread-inset ${expanded ? "open" : ""}`}>
      <div className="row-fold-h" onClick={() => setExpanded(!expanded)}>
        <span className="tri">▶</span>
        <span className="lbl">推理过程</span>
        <span className="meta">
          {message.reasoningDone ? `${message.content.length} 字` : "进行中…"}
        </span>
        <span className="right">
          <span className="meta">{expanded ? "收起" : "点击展开"}</span>
        </span>
      </div>
      <div className="row-fold-b">{message.content}</div>
    </div>
  );
}
