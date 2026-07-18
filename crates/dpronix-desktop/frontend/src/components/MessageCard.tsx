/**
 * MessageCard — Reasonix-style .msg.user / .msg.assistant / .reasoning-card / .tool-card
 */
import { useState } from "react";
import type { Message } from "../types";

function truncate(s: string, max: number): string {
  if (s.length <= max) return s;
  let boundary = max;
  while (boundary > 0 && (s.charCodeAt(boundary) & 0xC0) === 0x80) boundary--;
  return s.slice(0, boundary) + "...";
}

interface MessageCardProps { message: Message; }

export default function MessageCard({ message }: MessageCardProps) {
  const { role, content, toolName, toolArgs, toolResult, reasoningDone } = message;

  // User message
  if (role === "user") {
    return (
      <div className="turn-divider">
        <span>YOU</span>
        <span className="line" />
      </div>
    );
  }

  // Reasoning (thinking) card
  if (role === "reasoning") {
    return <ReasoningCard content={content} done={reasoningDone} />;
  }

  // Tool call card
  if (role === "tool") {
    return <ToolCard name={toolName ?? ""} args={toolArgs ?? ""} result={toolResult ?? ""} />;
  }

  // Assistant message
  if (role === "assistant" && content) {
    return (
      <div className="msg assistant">
        <div className="body">
          <div className="msg-text">{content}</div>
        </div>
      </div>
    );
  }

  return null;
}

/* User message as turn-divider + msg.user */
export function UserMsg({ text }: { text: string }) {
  return (
    <div>
      <div className="turn-divider">
        <span>YOU</span>
        <span className="line" />
      </div>
      <div className="msg user">
        <div className="body">
          <div className="msg-text">{text}</div>
        </div>
      </div>
    </div>
  );
}

/* Reasoning card — collapsible */
function ReasoningCard({ content, done }: { content: string; done?: boolean }) {
  const [open, setOpen] = useState(!done);
  return (
    <div className="reasoning-card">
      <button className="reasoning-card-head" onClick={() => setOpen((o) => !o)}>
        <span className={`chevron ${open ? "open" : ""}`}>
          <svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="3"><path d="M9 18l6-6-6-6" /></svg>
        </span>
        <span>{done ? "Thinking (done)" : "Thinking..."}</span>
      </button>
      {open && <div className="reasoning-card-body">{content}</div>}
    </div>
  );
}

/* Tool card */
function ToolCard({ name, args, result }: { name: string; args: string; result: string }) {
  const [open, setOpen] = useState(true);
  return (
    <div className="tool-card">
      <button className="tool-card-head" onClick={() => setOpen((o) => !o)}>
        <span className="icon">
          <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"><path d="M14.7 6.3a1 1 0 0 0 0 1.4l1.6 1.6a1 1 0 0 0 1.4 0l3.77-3.77a6 6 0 0 1-7.94 7.94l-6.91 6.91a2.12 2.12 0 0 1-3-3l6.91-6.91a6 6 0 0 1 7.94-7.94l-3.76 3.76z" /></svg>
        </span>
        <span className="name">{name}</span>
      </button>
      {open && (
        <div className="tool-card-body">
          {args && <div className="args">{truncate(args, 500)}</div>}
          {result && <div className="result">{truncate(result, 800)}</div>}
        </div>
      )}
    </div>
  );
}
