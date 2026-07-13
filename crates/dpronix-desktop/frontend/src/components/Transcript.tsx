/**
 * Transcript — scrollable conversation message list.
 * Inspired by DeepSeek-Reasonix desktop/frontend/src/components/Transcript.tsx
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

  // Auto-scroll when new messages arrive
  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [messages]);

  if (messages.length === 0) {
    return (
      <div className="transcript">
        <div className="welcome">
          <div className="welcome-icon">
            <svg width="48" height="48" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.5">
              <path d="M12 2L2 7l10 5 10-5-10-5z"/><path d="M2 17l10 5 10-5"/><path d="M2 12l10 5 10-5"/>
            </svg>
          </div>
          <h2>DPronix Desktop</h2>
          <p>DeepSeek-V4 optimized AI coding agent</p>
          <div className="welcome-features">
            <span>✓ Thinking Mode</span>
            <span>✓ Tool Calls</span>
            <span>✓ Context Caching</span>
          </div>
        </div>
      </div>
    );
  }

  return (
    <div className="transcript">
      {messages.map((msg) => (
        <MessageCard key={msg.id} message={msg} />
      ))}
      {loading && <div className="typing-indicator">▊</div>}
      <div ref={bottomRef} />
    </div>
  );
}
