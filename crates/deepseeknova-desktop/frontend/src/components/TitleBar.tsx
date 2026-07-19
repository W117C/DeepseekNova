/**
 * TitleBar — window chrome: traffic lights, brand, thinking badge, toggles.
 *
 * Uses dp-* design system. macOS traffic lights are hidden on non-macOS
 * (controlled by html[data-platform]).
 */
import { useCallback } from "react";

interface TitleBarProps {
  title: string;
  thinkingLabel?: string;
  effort?: string;
  sideCollapsed?: boolean;
  onToggleSidebar: () => void;
  onToggleContext: () => void;
  onOpenSettings: () => void;
}

export default function TitleBar({
  title,
  thinkingLabel,
  effort,
  sideCollapsed,
  onToggleSidebar,
  onToggleContext,
  onOpenSettings,
}: TitleBarProps) {
  const handleClose = useCallback(() => {
    import("@tauri-apps/api/window")
      .then(({ getCurrentWindow }) => getCurrentWindow().close())
      .catch(() => {});
  }, []);
  const handleMinimize = useCallback(() => {
    import("@tauri-apps/api/window")
      .then(({ getCurrentWindow }) => getCurrentWindow().minimize())
      .catch(() => {});
  }, []);
  const handleZoom = useCallback(() => {
    import("@tauri-apps/api/window")
      .then(({ getCurrentWindow }) => {
        const win = getCurrentWindow();
        win
          .isFullscreen()
          .then((fs) =>
            fs
              ? win.setFullscreen(false)
              : win.isMaximized().then((mx) => (mx ? win.unmaximize() : win.maximize())),
          );
      })
      .catch(() => {});
  }, []);

  return (
    <header className="dp-titlebar">
      <div className="dp-traffic" aria-hidden="true">
        <span className="dot close" onClick={handleClose} />
        <span className="dot minimize" onClick={handleMinimize} />
        <span className="dot zoom" onClick={handleZoom} />
      </div>

      <div className="brand">
        <span className="mark" aria-hidden="true" />
        <span className="name">{title}</span>
      </div>

      {thinkingLabel && (
        <span className="badge">
          <span className="pulse" aria-hidden="true" />
          {thinkingLabel}
          {effort ? ` · effort ${effort}` : ""}
        </span>
      )}

      <div className="grow" />

      <div className="actions">
        <button
          className="dp-iconbtn"
          aria-label="Toggle sidebar"
          data-on={!sideCollapsed}
          onClick={onToggleSidebar}
        >
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
            <rect x="3" y="3" width="18" height="18" rx="2" />
            <path d="M9 3v18" />
          </svg>
        </button>
        <button
          className="dp-iconbtn"
          aria-label="Toggle context panel"
          onClick={onToggleContext}
        >
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
            <rect x="3" y="3" width="18" height="18" rx="2" />
            <path d="M15 3v18" />
          </svg>
        </button>
        <button
          className="dp-iconbtn"
          aria-label="Open settings"
          onClick={onOpenSettings}
        >
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
            <circle cx="12" cy="12" r="3" />
            <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 1 1-2.83 2.83l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-4 0v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 1 1-2.83-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1 0-4h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 1 1 2.83-2.83l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 4 0v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 1 1 2.83 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 0 4h-.09a1.65 1.65 0 0 0-1.51 1z" />
          </svg>
        </button>
      </div>
    </header>
  );
}
