/**
 * Composer — input area with send/cancel button.
 * Inspired by DeepSeek-DPronix desktop/frontend/src/components/Composer.tsx
 */

import { useCallback, useRef, useEffect } from "react";

interface ComposerProps {
  value: string;
  onChange: (value: string) => void;
  onSubmit: () => void;
  onCancel: () => void;
  running: boolean;
  disabled?: boolean;
  placeholder?: string;
}

export default function Composer({
  value,
  onChange,
  onSubmit,
  onCancel,
  running,
  disabled,
  placeholder,
}: ComposerProps) {
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  // Auto-resize textarea
  useEffect(() => {
    const el = textareaRef.current;
    if (el) {
      el.style.height = "auto";
      el.style.height = Math.min(el.scrollHeight, 120) + "px";
    }
  }, [value]);

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent) => {
      if (e.key === "Enter" && !e.shiftKey) {
        e.preventDefault();
        if (running) {
          onCancel();
        } else if (value.trim()) {
          onSubmit();
        }
      }
    },
    [running, value, onSubmit, onCancel],
  );

  return (
    <div className="composer">
      <div className="composer-input-wrap">
        <textarea
          ref={textareaRef}
          className="composer-input"
          placeholder={
            running
              ? "Agent is running… (Enter to cancel)"
              : placeholder ?? "Ask anything… (Enter to send, Shift+Enter for new line)"
          }
          value={value}
          onChange={(e) => onChange(e.target.value)}
          onKeyDown={handleKeyDown}
          disabled={running || disabled}
          rows={1}
        />
        <button
          className={`composer-btn ${running ? "btn-cancel" : "btn-send"}`}
          onClick={running ? onCancel : onSubmit}
          disabled={!running && (!value.trim() || disabled)}
          title={running ? "Cancel" : "Send"}
        >
          {running ? "■" : "→"}
        </button>
      </div>
    </div>
  );
}
