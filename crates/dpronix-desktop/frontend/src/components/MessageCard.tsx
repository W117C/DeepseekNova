/**
 * MessageCard — Reasonix DOM: .msg.user / .msg.assistant / .reasoning-card / .tool-card
 */
import { useState } from "react";
import type { Message } from "../types";

function trunc(s: string, max: number) { return s.length <= max ? s : s.slice(0, max) + "..."; }

/* ── UserMsg ── */
export function UserMsg({ text }: { text: string }) {
  return (
    <div>
      <div className="turn-divider"><span>YOU</span><span className="line" /></div>
      <div className="msg user">
        <div className="avatar" />
        <div className="body">
          <div className="who"><span className="name">YOU</span></div>
          <div className="msg-text">{text}</div>
        </div>
      </div>
    </div>
  );
}

/* ── ReasoningCard ── */
function ReasoningCard({ text, done }: { text: string; done?: boolean }) {
  const [open, setOpen] = useState(!done);
  return (
    <div className="reasoning-card">
      <button type="button" className="reasoning-card-head" onClick={() => setOpen(o => !o)}>
        <span className={`chevron ${open ? "open" : ""}`}>
          <svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="3"><path d="M9 18l6-6-6-6"/></svg>
        </span>
        <span>{done ? "Thinking (done)" : "Thinking..."}</span>
      </button>
      {open && <div className="reasoning-card-body">{text}</div>}
    </div>
  );
}

/* ── ToolCard ── */
function ToolCard({ name, args, result }: { name: string; args: string; result: string }) {
  const [open, setOpen] = useState(true);
  return (
    <div className="tool-card">
      <button type="button" className="tool-card-head" onClick={() => setOpen(o => !o)}>
        <span className="icon">
          <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"><path d="M14.7 6.3a1 1 0 0 0 0 1.4l1.6 1.6a1 1 0 0 0 1.4 0l3.77-3.77a6 6 0 0 1-7.94 7.94l-6.91 6.91a2.12 2.12 0 0 1-3-3l6.91-6.91a6 6 0 0 1 7.94-7.94l-3.76 3.76z"/></svg>
        </span>
        <span className="name">{name}</span>
      </button>
      {open && (
        <div className="tool-card-body">
          {args && <div className="args">{trunc(args, 500)}</div>}
          {result && <div className="result">{trunc(result, 800)}</div>}
        </div>
      )}
    </div>
  );
}

/* ── AssistantMsg ── */
export function AssistantMsg({ text, pending }: { text: string; pending?: boolean }) {
  if (!text && !pending) return null;
  return (
    <div className="msg assistant">
      <div className="avatar" />
      <div className="body">
        <div className="who">
          <span className="name">DPronix</span>
        </div>
        <div className="msg-text">{text || "\u258A"}</div>
      </div>
    </div>
  );
}

/* ── MessageCard (router) ── */
export default function MessageCard({ message }: { message: Message }) {
  const { role, content, toolName, toolArgs, toolResult, reasoningDone } = message;
  if (role === "reasoning") return <ReasoningCard text={content} done={reasoningDone} />;
  if (role === "tool") return <ToolCard name={toolName ?? ""} args={toolArgs ?? ""} result={toolResult ?? ""} />;
  if (role === "assistant") return <AssistantMsg text={content} />;
  return null;
}
