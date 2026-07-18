/**
 * StatusBar — Reasonix-style bottom bar with token usage and run state.
 */
import type { UsageInfo } from "../types";

interface StatusBarProps {
  lastUsage: UsageInfo | null;
  sessionCache: { hit: number; miss: number };
  running: boolean;
}

export default function StatusBar({ lastUsage, sessionCache, running }: StatusBarProps) {
  const cachePct = sessionCache.hit + sessionCache.miss > 0
    ? Math.round(sessionCache.hit / (sessionCache.hit + sessionCache.miss) * 100)
    : null;

  return (
    <div className="statusbar">
      <div className="status-left">
        {lastUsage && (
          <>
            <span className="status-item" title="Prompt tokens">{lastUsage.prompt_tokens}↑</span>
            <span className="status-item" title="Completion tokens">{lastUsage.completion_tokens}↓</span>
            {lastUsage.reasoning_tokens > 0 && (
              <span className="status-item" title="DeepSeek-V4 reasoning tokens">
                {lastUsage.reasoning_tokens} think
              </span>
            )}
            {cachePct !== null && (
              <span className="status-item" title="Context cache hit rate">
                cache {cachePct}%
              </span>
            )}
            <span className="status-item" title="Total tokens">{lastUsage.total_tokens} total</span>
          </>
        )}
      </div>
      <div className="status-right">
        {running ? "running..." : "ready"}
      </div>
    </div>
  );
}
