/**
 * Transcript — Reasonix-style scrollable conversation message list.
 */
import { useEffect, useRef } from "react";
import MessageCard from "./MessageCard";
import type { Message } from "../types";

interface TranscriptProps {
  messages: Message[];
  loading?: boolean;
}

export default function Transcript({ messages, loading }: TranscriptProps) {
  const bottomRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [messages]);

  if (messages.length === 0) {
    return (
      <div className="thread">
        <div className="thread-empty">
          <svg width="36" height="36" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.5" opacity={0.4}>
            <path d="M12 2L2 7l10 5 10-5-10-5z"/><path d="M2 17l10 5 10-5"/><path d="M2 12l10 5 10-5"/>
          </svg>
          <strong>DPronix Desktop</strong>
          <p>DeepSeek-V4 AI coding agent. Start a conversation below.</p>
        </div>
      </div>
    );
  }

  return (
    <div className="thread">
      {messages.map((msg) => (
        <MessageCard key={msg.id} message={msg} />
      ))}
      {loading && <div className="msg-status muted">thinking...</div>}
      <div ref={bottomRef} />
    </div>
  );
}
