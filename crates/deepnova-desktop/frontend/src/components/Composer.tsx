/**
 * Composer — input area with auto-resize and keyboard shortcuts.
 *
 * Keyboard:
 *   Enter         → send (or cancel if running)
 *   Shift+Enter   → newline
 *   Cmd/Ctrl+↑ ↓  → prompt history (handled by parent via onKeyDown)
 */
import { useRef, useEffect } from "react";

interface ComposerProps {
  value: string;
  onChange: (value: string) => void;
  onSubmit: () => void;
  onCancel: () => void;
  onKeyDown?: (e: React.KeyboardEvent) => void;
  running: boolean;
  disabled?: boolean;
  placeholder?: string;
}

export default function Composer({
  value,
  onChange,
  onSubmit,
  onCancel,
  onKeyDown,
  running,
  disabled,
  placeholder,
}: ComposerProps) {
  const textareaRef = useRef<HTMLTextAreaElement>(null);

  // auto-resize
  useEffect(() => {
    const el = textareaRef.current;
    if (el) {
      el.style.height = "auto";
      el.style.height = Math.min(el.scrollHeight, 180) + "px";
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

  const handleKeyDown = (e: React.KeyboardEvent) => {
    onKeyDown?.(e);
  };

  return (
    <div className="dp-composer">
      <div className="dp-composer-inner">
        <textarea
          ref={textareaRef}
          placeholder={
            running
              ? "Agent is running… (Enter to cancel)"
              : placeholder ?? "Ask anything…"
          }
          value={value}
          onChange={(e) => onChange(e.target.value)}
          onKeyDown={handleKeyDown}
          disabled={running || disabled}
          rows={1}
        />
        <button
          type="button"
          className={`send${running ? " cancel" : ""}`}
          aria-label={running ? "Cancel" : "Send"}
          onClick={running ? onCancel : onSubmit}
          disabled={!running && (!value.trim() || disabled)}
        >
          {running ? (
            <svg viewBox="0 0 24 24" fill="currentColor">
              <rect x="6" y="6" width="12" height="12" rx="2" />
            </svg>
          ) : (
            <svg viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
              <path d="M5 12h14M13 6l6 6-6 6" />
            </svg>
          )}
        </button>
      </div>
      <div className="dp-composer-hints">
        <span>
          <kbd>Enter</kbd> send · <kbd>Shift+Enter</kbd> newline
        </span>
        <span>
          <kbd>⌘↑</kbd> history
        </span>
      </div>
    </div>
  );
}
