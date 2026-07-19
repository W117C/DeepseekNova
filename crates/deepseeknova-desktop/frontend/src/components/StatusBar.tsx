/**
 * StatusBar — bottom bar: token counts, cache rate, agent state.
 */
import type { AgentStatus, Mode } from "../types";

interface StatusBarProps {
  model?: string;
  mode?: Mode;
  tokensUp: number;
  tokensDown: number;
  cachePercent: number;
  cacheHit?: number;
  status: AgentStatus;
}

export default function StatusBar({
  model,
  mode,
  tokensUp,
  tokensDown,
  cachePercent,
  status,
}: StatusBarProps) {
  const modeColor = mode === "plan" ? "var(--dp-success)" : mode === "act" ? "var(--dp-accent)" : "var(--dp-danger)";

  return (
    <footer className="dp-statusbar">
      {model && <span className="stat model">{model}</span>}
      {mode && (
        <span className="stat" style={{ color: modeColor, fontWeight: 600, fontSize: 11, padding: "0 6px" }}>
          {mode.toUpperCase()}
        </span>
      )}
      <span className="sep" />
      <span className="stat">
        <span className="arrow">↑</span> {tokensUp.toLocaleString()}
      </span>
      <span className="sep" />
      <span className="stat">
        <span className="arrow">↓</span> {tokensDown.toLocaleString()}
      </span>
      <span className="sep" />
      <span className="stat">Cache {cachePercent}%</span>

      <span className="grow" />

      <span className={`state ${status}`}>
        <span className="dot" aria-hidden="true" />
        {status === "ready" ? "Ready" : "Running"}
      </span>
    </footer>
  );
}
