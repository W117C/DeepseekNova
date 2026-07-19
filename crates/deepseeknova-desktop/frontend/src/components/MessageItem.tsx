/**
 * MessageItem — routes a single Message to the right sub-component by role.
 *
 * Roles: user | assistant | reasoning | tool
 * Each sub-component uses the dp-* design system classes.
 */
import { useState } from "react";
import type { Message } from "../types";
import MarkdownRenderer from "./MarkdownRenderer";

/* — User message: right-aligned bubble — */
export function UserMessage({ content }: { content: string }) {
  return <div className="dp-msg-user">{content}</div>;
}

/* — Assistant message: avatar + markdown body — */
export function AssistantMessage({
  content,
  streaming,
}: {
  content: string;
  streaming?: boolean;
}) {
  return (
    <div className="dp-msg-assistant">
      <div className="avatar" aria-hidden="true">
        <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
          <path d="M12 2L2 7l10 5 10-5-10-5z" />
          <path d="M2 17l10 5 10-5" />
          <path d="M2 12l10 5 10-5" />
        </svg>
      </div>
      <div className="body">
        <div className="who">DeepseekNova</div>
        <div className="text">
          {content ? (
            <MarkdownRenderer content={content} />
          ) : streaming ? (
            <span className="cursor" aria-label="generating" />
          ) : null}
        </div>
      </div>
    </div>
  );
}

/* — Reasoning block: collapsible, cyan accent, mono text — */
export function ReasoningBlock({
  content,
  done,
}: {
  content: string;
  done?: boolean;
}) {
  const [open, setOpen] = useState(!done);
  return (
    <div className="dp-reasoning">
      <button
        type="button"
        className="head"
        aria-expanded={open}
        onClick={() => setOpen((o) => !o)}
      >
        <span className={`chevron${open ? " open" : ""}`} aria-hidden="true">
          <svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="3">
            <path d="M9 18l6-6-6-6" />
          </svg>
        </span>
        {!done && <span className="spinner" aria-hidden="true" />}
        <span>{done ? "Thinking (done)" : "Thinking…"}</span>
      </button>
      {open && content && <div className="body">{content}</div>}
    </div>
  );
}

/* — Tool block: collapsible, shows name + status + args/result — */
export function ToolBlock({
  name,
  args,
  result,
  running,
}: {
  name: string;
  args?: string;
  result?: string;
  running?: boolean;
}) {
  const [open, setOpen] = useState(true);
  const statusClass = running ? "running" : result ? "success" : "running";
  return (
    <div className="dp-tool">
      <button
        type="button"
        className="head"
        aria-expanded={open}
        onClick={() => setOpen((o) => !o)}
      >
        <span className={`chevron${open ? " open" : ""}`} aria-hidden="true">
          <svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="3">
            <path d="M9 18l6-6-6-6" />
          </svg>
        </span>
        <span className="name">{name || "tool"}</span>
        <span className={`status-ico ${statusClass}`}>
          {running ? (
            <span className="spinner" aria-hidden="true" />
          ) : result ? (
            <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="3">
              <path d="M20 6L9 17l-5-5" />
            </svg>
          ) : null}
        </span>
      </button>
      {open && (args || result) && (
        <div className="body">
          {args && (
            <div>
              <div className="section-label">Args</div>
              <pre>{args}</pre>
            </div>
          )}
          {result && (
            <div>
              <div className="section-label">Result</div>
              <pre>{result}</pre>
            </div>
          )}
        </div>
      )}
    </div>
  );
}

/* — Approval card: needs confirmation — */
export function ApprovalCard({
  title,
  description,
  onApprove,
  onReject,
}: {
  title: string;
  description?: string;
  onApprove: () => void;
  onReject: () => void;
}) {
  return (
    <div className="dp-approval" role="alert">
      <div className="label">Needs Confirmation</div>
      <div className="title">{title}</div>
      {description && <div className="desc">{description}</div>}
      <div className="actions">
        <button className="dp-btn" onClick={onReject}>
          Reject
        </button>
        <button className="dp-btn primary" onClick={onApprove}>
          Approve
        </button>
      </div>
    </div>
  );
}

/* — Router — */
export default function MessageItem({
  message,
  isStreaming,
}: {
  message: Message;
  isStreaming?: boolean;
}) {
  const { role, content, toolName, toolArgs, toolResult, reasoningDone } = message;
  if (role === "user") return <UserMessage content={content} />;
  if (role === "assistant")
    return <AssistantMessage content={content} streaming={isStreaming} />;
  if (role === "reasoning")
    return <ReasoningBlock content={content} done={reasoningDone} />;
  if (role === "tool")
    return (
      <ToolBlock
        name={toolName ?? ""}
        args={toolArgs}
        result={toolResult}
        running={!toolResult}
      />
    );
  return null;
}
