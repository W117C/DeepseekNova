/**
 * Transcript — Reasonix .thread > .thread-inner with turn-dividers
 */
import { useEffect, useRef } from "react";
import MessageCard, { UserMsg } from "./MessageCard";
import type { Message } from "../types";

interface TranscriptProps { messages: Message[]; loading?: boolean; }

export default function Transcript({ messages, loading }: TranscriptProps) {
  const bottomRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [messages]);

  if (messages.length === 0) {
    return (
      <div className="thread">
        <div className="thread-inner thread-inner--standalone" style={{ textAlign: "center", paddingTop: 60 }}>
          <div style={{ fontSize: 32, marginBottom: 12, opacity: 0.3 }}>
            <svg width="48" height="48" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1"><path d="M12 2L2 7l10 5 10-5-10-5z"/><path d="M2 17l10 5 10-5"/><path d="M2 12l10 5 10-5"/></svg>
          </div>
          <div style={{ fontSize: 14, color: "var(--fg-2)", fontWeight: 600, marginBottom: 4 }}>DPronix Desktop</div>
          <div style={{ fontSize: 12, color: "var(--muted)" }}>DeepSeek-V4 AI coding agent. Start a conversation below.</div>
        </div>
      </div>
    );
  }

  return (
    <div className="thread">
      <div className="thread-inner thread-inner--standalone">
        {messages.map((msg) =>
          msg.role === "user" ? (
            <UserMsg key={msg.id} text={msg.content} />
          ) : (
            <MessageCard key={msg.id} message={msg} />
          )
        )}
        {loading && (
          <div style={{ fontSize: 12, color: "var(--muted)", padding: "8px 0", fontStyle: "italic" }}>thinking...</div>
        )}
        <div ref={bottomRef} />
      </div>
    </div>
  );
}
