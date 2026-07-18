/**
 * MessageCard — renders one message in the conversation transcript.
 * Supports 4 roles: user, assistant, reasoning, tool.
 */
import type { Message } from "../types";

function truncate(s: string, max: number): string {
  if (s.length <= max) return s;
  let boundary = max;
  while (boundary > 0 && (s.charCodeAt(boundary) & 0xC0) === 0x80) boundary--;
  return s.slice(0, boundary) + "...";
}

interface MessageCardProps {
  message: Message;
}

export default function MessageCard({ message }: MessageCardProps) {
  const { role, content, toolName, toolArgs, toolResult, reasoningDone } = message;

  const className = `msg ${role === "user" ? "msg-user" : role === "assistant" ? "msg-assistant" : ""}`;

  return (
    <div className="msg-turn">
      {/* Role label */}
      <div className="msg-status" style={{ textAlign: "left", marginBottom: 4 }}>
        {role === "user" ? "You" :
         role === "reasoning" ? (reasoningDone ? "Thought" : "Thinking...") :
         role === "tool" ? (toolName ?? "Tool") :
         "DPronix"}
      </div>

      {/* Reasoning — collapsible (DeepSeek thinking mode) */}
      {role === "reasoning" ? (
        <div className="msg-reasoning">
          <details open={!reasoningDone}>
            <summary style={{ cursor: "pointer", fontSize: 11, color: "var(--warning)", fontWeight: 500 }}>
              {reasoningDone ? "Thinking (done)" : "Thinking..."}
            </summary>
            <pre style={{ fontFamily: "inherit", fontSize: "inherit", margin: "6px 0 0", whiteSpace: "pre-wrap" }}>{content}</pre>
          </details>
        </div>
      ) : role === "tool" ? (
        /* Tool call */
        <div className="msg-tool">
          <div className="tool-name">{toolName}</div>
          {toolArgs && <div className="tool-args">{truncate(toolArgs, 500)}</div>}
          {toolResult && (
            <div className="tool-result">
              {truncate(toolResult, 800)}
            </div>
          )}
        </div>
      ) : content ? (
        /* Text message */
        <div className={className}>
          <div style={{ whiteSpace: "pre-wrap", lineHeight: 1.6 }}>{content}</div>
        </div>
      ) : null}
    </div>
  );
}
