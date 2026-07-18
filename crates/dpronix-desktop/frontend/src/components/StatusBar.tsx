import type { AgentStatus } from "../types";

interface StatusBarProps { tokensUp: number; tokensDown: number; cachePercent: number; status: AgentStatus; }
export default function StatusBar({ tokensUp, tokensDown, cachePercent, status }: StatusBarProps) {
  return (
    <div className="status-bar">
      <span>&uarr; {tokensUp.toLocaleString()}</span>
      <span>&darr; {tokensDown.toLocaleString()}</span>
      <span>Cache {cachePercent}%</span>
      <span className="status">
        <span className={`dot ${status}`} />
        {status === "ready" ? "Ready" : "Running"}
      </span>
    </div>
  );
}
