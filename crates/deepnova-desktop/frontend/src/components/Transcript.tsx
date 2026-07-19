/**
 * Transcript — renders the conversation in chronological order.
 *
 * Unlike the old App.tsx which split messages into three filtered arrays
 * (breaking tool/reasoning/answer interleaving), this component walks the
 * messages array once and renders each via <MessageItem>.
 */
import type { RefObject } from "react";
import type { Message, ApprovalRequest } from "../types";
import MessageItem, { ApprovalCard } from "./MessageItem";

interface TranscriptProps {
  messages: Message[];
  running?: boolean;
  pendingApproval?: ApprovalRequest | null;
  onApprove?: () => void;
  onReject?: () => void;
  endRef?: RefObject<HTMLDivElement>;
}

export default function Transcript({
  messages,
  running,
  pendingApproval,
  onApprove,
  onReject,
  endRef,
}: TranscriptProps) {
  // Empty state hero
  if (messages.length === 0) {
    return (
      <div className="dp-thread-inner">
        <div className="dp-hero">
          <div className="logo" aria-hidden="true">
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="1.5">
              <path d="M12 2L2 7l10 5 10-5-10-5z" />
              <path d="M2 17l10 5 10-5" />
              <path d="M2 12l10 5 10-5" />
            </svg>
          </div>
          <h1>DeepNova Desktop</h1>
          <p>DeepSeek-V4 AI coding agent. Start a conversation below.</p>
          <div className="hints">
            <span className="hint">Enter to send</span>
            <span className="hint">Shift+Enter for newline</span>
            <span className="hint">Cmd+↑ for history</span>
          </div>
        </div>
        {endRef ? <div ref={endRef} /> : null}
      </div>
    );
  }

  // Detect the streaming assistant message id (last assistant msg while running)
  const streamingId = running
    ? [...messages].reverse().find((m) => m.role === "assistant")?.id
    : undefined;

  return (
    <div className="dp-thread-inner">
      {pendingApproval && onApprove && onReject && (
        <ApprovalCard
          title={pendingApproval.title}
          description={pendingApproval.description ?? undefined}
          onApprove={onApprove}
          onReject={onReject}
        />
      )}

      {messages.map((msg) => (
        <MessageItem
          key={msg.id}
          message={msg}
          isStreaming={msg.id === streamingId}
        />
      ))}

      {running && (
        <div className="dp-loading">
          <span className="spinner" aria-hidden="true" />
          <span>working…</span>
        </div>
      )}

      {endRef ? <div ref={endRef} /> : null}
    </div>
  );
}
