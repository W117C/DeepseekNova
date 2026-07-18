/**
 * TitleBar — Reasonix-style window chrome with macOS traffic lights,
 * sidebar toggle, brand, model name, and action buttons.
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

  return (
    <header className="titlebar" data-settings-open={showSettings}>
      {/* Left: sidebar toggle + brand */}
      <div className="tb-left">
        {/* macOS traffic lights (visual only — functional via Tauri window API) */}
        <div className="mac-controls" aria-label="Window controls">
          <button type="button" className="mac-ctrl close" title="Close" />
          <button type="button" className="mac-ctrl minimize" title="Minimize" />
          <button type="button" className="mac-ctrl zoom" title="Zoom" />
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

      {/* Right: action buttons + more menu */}
      <div className="tb-right">
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
              <div className="popup-item" onClick={() => { onToggleSettings(); closeMenu(); }}>
                <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
                  <circle cx="12" cy="12" r="3" /><path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />
                </svg>
                <span>Settings</span>
                <span className="kb">⌘,</span>
              </div>
              <div className="popup-item" onClick={() => { onNewSession(); closeMenu(); }}>
                <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
                  <path d="M12 5v14M5 12h14" />
                </svg>
                <span>New session</span>
                <span className="kb">⌘N</span>
              </div>
              <div className="popup-sep" />
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
