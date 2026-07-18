/**
 * TitleBar — Reasonix UI: macOS traffic lights, title, thinking badge, buttons.
 */
import { useCallback } from "react";

interface TitleBarProps {
  title: string;
  thinkingLabel?: string;
  effort?: string;
  onToggleContext: () => void;
  onOpenSettings: () => void;
}

export default function TitleBar({ title, thinkingLabel, effort, onToggleContext, onOpenSettings }: TitleBarProps) {
  const handleClose = useCallback(() => {
    import("@tauri-apps/api/window").then(({ getCurrentWindow }) => getCurrentWindow().close()).catch(() => {});
  }, []);
  const handleMinimize = useCallback(() => {
    import("@tauri-apps/api/window").then(({ getCurrentWindow }) => getCurrentWindow().minimize()).catch(() => {});
  }, []);
  const handleZoom = useCallback(() => {
    import("@tauri-apps/api/window").then(({ getCurrentWindow }) => {
      const win = getCurrentWindow();
      win.isFullscreen().then((fs) => fs ? win.setFullscreen(false) : win.isMaximized().then((mx) => mx ? win.unmaximize() : win.maximize()));
    }).catch(() => {});
  }, []);

  return (
    <div className="titlebar">
      <div className="traffic-lights" aria-hidden="true">
        <span className="dot dot-danger" onClick={handleClose} style={{ cursor: "pointer" }} />
        <span className="dot dot-warning" onClick={handleMinimize} style={{ cursor: "pointer" }} />
        <span className="dot dot-success" onClick={handleZoom} style={{ cursor: "pointer" }} />
      </div>
      <span className="title">{title}</span>
      {thinkingLabel && (
        <span className="badge">{thinkingLabel}{effort ? ` \u00b7 effort ${effort}` : ""}</span>
      )}
      <div className="actions">
        <button className="icon-button" aria-label="Toggle context panel" onClick={onToggleContext}>&#x25A4;</button>
        <button className="icon-button" aria-label="Open settings" onClick={onOpenSettings}>&#x2699;</button>
      </div>
    </div>
  );
}
