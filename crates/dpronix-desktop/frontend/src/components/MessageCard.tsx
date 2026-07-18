/**
 * MessageCard — renders one message in the conversation transcript.
 * Supports 4 roles: user, assistant, reasoning, tool.
 * Inspired by DeepSeek-DPronix desktop/frontend/src/components/Message.tsx
 */
import type { Message } from "../types";

/** Truncate long strings at character boundary */
function truncate(s: string, max: number): string {
  if (s.length <= max) return s;
  let boundary = max;
  while (boundary > 0 && (s.charCodeAt(boundary) & 0xC0) === 0x80) boundary--;
  return s.slice(0, boundary) + "…";
}

interface MessageCardProps {
  message: Message;
}

export default function MessageCard({ message }: MessageCardProps) {
  const { role, content, toolName, toolArgs, toolResult, reasoningDone } = message;

  const className = `msg msg-${role}`;

  return (
    <div className={className}>
      {/* Role label */}
      <div className="msg-label">
        {role === "user" ? "You" :
         role === "reasoning" ? (reasoningDone ? "Thought" : "Reasoning") :
         role === "tool" ? (toolName ?? "Tool") :
         "DPronix"}
      </div>

      {/* Reasoning — collapsible (DeepSeek thinking mode); auto-collapses when done */}
      {role === "reasoning" ? (
        <div className="msg-reasoning-content">
          <details open={!reasoningDone}>
            <summary>{reasoningDone ? "Thinking (done)" : "Thinking…"}</summary>
            <pre>{content}</pre>
          </details>
        </div>
      ) : role === "tool" ? (
        /* Tool call — args + result */
        <div className="msg-tool-content">
          <div className="tool-header">
            <code>{toolName}</code>
          </div>
          {toolArgs && (
            <pre className="tool-args">{truncate(toolArgs, 500)}</pre>
          )}
          {toolResult && (
            <div className="tool-result">
              <div className="tool-result-label">→ Result</div>
              <pre>{toolResult}</pre>
            </div>
          )}
        </div>
      ) : (
        /* Text message */
        <div className="msg-text">{content || "▊"}</div>
      )}
    </div>
  );
}
