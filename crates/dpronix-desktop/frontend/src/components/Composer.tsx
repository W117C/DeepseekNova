/**
 * Composer — Reasonix-style input area with auto-resize and keyboard shortcuts.
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
      el.style.height = Math.min(el.scrollHeight, 160) + "px";
    }
  }, [value]);

  // Cmd+L global focus
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if ((e.metaKey || e.ctrlKey) && (e.key === "l" || e.key === "L")) {
        e.preventDefault();
        textareaRef.current?.focus();
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);

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
    <div className="composer-wrap">
      <div className="composer">
        <textarea
          ref={textareaRef}
          placeholder={
            running
              ? "Agent is running... (Enter to cancel)"
              : placeholder ?? "Ask anything... (Enter to send, Shift+Enter for new line)"
          }
          value={value}
          onChange={(e) => onChange(e.target.value)}
          onKeyDown={handleKeyDown}
          disabled={running || disabled}
          rows={1}
        />
        <div className="composer-actions">
          <button
            type="button"
            className={`btn ${running ? "danger" : "primary"}`}
            onClick={running ? onCancel : onSubmit}
            disabled={!running && (!value.trim() || disabled)}
            title={running ? "Cancel (Esc)" : "Send (Enter)"}
          >
            {running ? "Cancel" : "Send"}
          </button>
        </div>
      </div>
    </div>
  );
}
