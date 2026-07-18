/**
 * TitleBar — Reasonix-style window chrome with functional macOS traffic lights,
 * sidebar toggle, brand, model name, thinking/effort controls, and action buttons.
 */
import { useState, useEffect, useRef, useCallback } from "react";

interface TitleBarProps {
  sideCollapsed: boolean;
  onToggleSide: () => void;
  modelName?: string;
  showSettings: boolean;
  onToggleSettings: () => void;
  showSkills: boolean;
  onToggleSkills: () => void;
  onNewSession: () => void;
  running: boolean;
  /** Thinking / reasoning effort controls (wired from parent) */
  thinkingEnabled: boolean;
  onToggleThinking: () => void;
  reasoningEffort: string;
  onSetEffort: (effort: string) => void;
  effortLevels: string[];
  supportsThinking: boolean;
  supportsEffort: boolean;
}

export default function TitleBar({
  sideCollapsed,
  onToggleSide,
  modelName,
  showSettings,
  onToggleSettings,
  showSkills,
  onToggleSkills,
  onNewSession,
  running,
  thinkingEnabled,
  onToggleThinking,
  reasoningEffort,
  onSetEffort,
  effortLevels,
  supportsThinking,
  supportsEffort,
}: TitleBarProps) {
  const [menuOpen, setMenuOpen] = useState(false);
  const menuRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    const onDown = (e: MouseEvent) => {
      if (menuRef.current && !menuRef.current.contains(e.target as Node)) setMenuOpen(false);
    };
    if (menuOpen) window.addEventListener("mousedown", onDown);
    return () => window.removeEventListener("mousedown", onDown);
  }, [menuOpen]);

  const closeMenu = useCallback(() => setMenuOpen(false), []);

  // Tauri window controls (functional on macOS + Windows)
  const handleClose = useCallback(() => {
    try {
      // Dynamic import avoids bundling issues when not in Tauri shell
      import("@tauri-apps/api/window").then(({ getCurrentWindow }) => {
        getCurrentWindow().close();
      }).catch(() => {});
    } catch {}
  }, []);

  const handleMinimize = useCallback(() => {
    try {
      import("@tauri-apps/api/window").then(({ getCurrentWindow }) => {
        getCurrentWindow().minimize();
      }).catch(() => {});
    } catch {}
  }, []);

  const handleZoom = useCallback(() => {
    try {
      import("@tauri-apps/api/window").then(({ getCurrentWindow }) => {
        const win = getCurrentWindow();
        import("@tauri-apps/api/window").then(() => {
          win.isFullscreen().then((fs) => {
            if (fs) win.setFullscreen(false);
            else win.isMaximized().then((mx) => mx ? win.unmaximize() : win.maximize());
          });
        }).catch(() => {});
      }).catch(() => {});
    } catch {}
  }, []);

  return (
    <header className="titlebar">
      {/* Left: macOS traffic lights + sidebar toggle + brand */}
      <div className="tb-left">
        <div className="mac-controls" aria-label="Window controls">
          <button type="button" className="mac-ctrl close" title="Close" onClick={handleClose} />
          <button type="button" className="mac-ctrl minimize" title="Minimize" onClick={handleMinimize} />
          <button type="button" className="mac-ctrl zoom" title="Zoom" onClick={handleZoom} />
        </div>
        <button
          type="button"
          className="iconbtn"
          data-on={!sideCollapsed}
          title="Toggle sidebar (Cmd+B)"
          onClick={onToggleSide}
        >
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
            <rect x="3" y="3" width="18" height="18" rx="2" />
            <path d="M9 3v18" />
          </svg>
        </button>
        <div className="tb-meta">
          <div className="brand">
            <span className="mark" />
            <span className="brand-name">DPronix</span>
          </div>
          {modelName && (
            <div className="crumbs">
              <span className="sep">/</span>
              <span className="cur">{modelName}</span>
            </div>
          )}
        </div>
      </div>

      {/* Center: drag region */}
      <span className="grow" />

      {/* Right: thinking/effort controls + action buttons + more menu */}
      <div className="tb-right" style={{ gap: 6 }}>
        {supportsThinking && (
          <label className="chip chip-toggle" title="DeepSeek-V4 thinking mode">
            <input
              type="checkbox"
              checked={thinkingEnabled}
              disabled={running}
              onChange={onToggleThinking}
            />
            thinking
          </label>
        )}
        {supportsEffort && (
          <select
            className="field"
            value={reasoningEffort}
            disabled={running || !thinkingEnabled}
            onChange={(e) => onSetEffort(e.target.value)}
            title="Reasoning effort"
            style={{ fontSize: 11, padding: "1px 6px", height: 22 }}
          >
            {effortLevels.map((lvl) => (
              <option key={lvl} value={lvl}>{lvl}</option>
            ))}
          </select>
        )}

        <button
          type="button"
          className="iconbtn"
          title="Skills"
          data-on={showSkills}
          onClick={onToggleSkills}
        >
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
            <path d="M12 2L2 7l10 5 10-5-10-5z" /><path d="M2 17l10 5 10-5" /><path d="M2 12l10 5 10-5" />
          </svg>
        </button>

        <button
          type="button"
          className="iconbtn"
          data-on={showSettings}
          title="Settings (Cmd+,)"
          onClick={onToggleSettings}
        >
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
            <circle cx="12" cy="12" r="3" /><path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />
          </svg>
        </button>

        <div ref={menuRef} style={{ position: "relative" }}>
          <button
            type="button"
            className="iconbtn"
            title="More"
            onClick={() => setMenuOpen((v) => !v)}
          >
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
              <circle cx="12" cy="5" r="1.5" /><circle cx="12" cy="12" r="1.5" /><circle cx="12" cy="19" r="1.5" />
            </svg>
          </button>
          {menuOpen && (
            <div className="popup" style={{ top: "calc(100% + 6px)", right: 0, left: "auto", bottom: "auto", width: 200 }}>
              <div className="popup-item" onClick={() => { onNewSession(); closeMenu(); }}>
                <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
                  <path d="M12 5v14M5 12h14" />
                </svg>
                <span>New session</span>
                <span className="kb">&#8984;N</span>
              </div>
              <div className="popup-sep" />
              <div className="popup-item" onClick={() => { onToggleSettings(); closeMenu(); }}>
                <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
                  <circle cx="12" cy="12" r="3" /><path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />
                </svg>
                <span>Settings</span>
                <span className="kb">&#8984;,</span>
              </div>
              <div className="popup-item" onClick={() => { onToggleSkills(); closeMenu(); }}>
                <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
                  <path d="M12 2L2 7l10 5 10-5-10-5z" />
                </svg>
                <span>Skills</span>
              </div>
            </div>
          )}
        </div>
      </div>
    </header>
  );
}
