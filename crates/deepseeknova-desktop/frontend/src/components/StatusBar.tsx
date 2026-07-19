/**
 * StatusBar.tsx — 底部成本仪表盘（Reasonix 风格）
 * 缓存命中率(带颜色) | Token 用量 | 推理 Token | 会话时长
 */

import { useStore } from "../store";
import { useTheme } from "../store/theme";
import { useEffect, useState } from "react";

export default function StatusBar() {
  const status = useStore((s) => s.status);
  const lastUsage = useStore((s) => s.lastUsage);
  const sessionCache = useStore((s) => s.sessionCache);
  const totalTokens = useStore((s) => s.totalTokens);

  const displayMode = useTheme((s) => s.displayMode);
  const toggleDisplayMode = useTheme((s) => s.toggleDisplayMode);
  const isIcon = displayMode === "icon";

  const [sessionDuration, setSessionDuration] = useState(0);
  useEffect(() => {
    const start = Date.now();
    const timer = setInterval(() => setSessionDuration(Math.floor((Date.now() - start) / 1000)), 1000);
    return () => clearInterval(timer);
  }, []);

  const fmtDur = (s: number) => s < 60 ? `${s}s` : `${Math.floor(s/60)}m ${s%60}s`;

  const totalCache = sessionCache.hit + sessionCache.miss;
  const cacheRate = totalCache > 0 ? Math.round((sessionCache.hit / totalCache) * 100) : 0;
  const cacheColor = cacheRate >= 80 ? "var(--green)" : cacheRate >= 50 ? "var(--amber)" : "var(--red)";

  return (
    <footer className="status-bar">
      <span className={`status-dot ${status}`} />
      <span className="status-item">{status === "ready" ? "就绪" : status === "running" ? "运行中" : "错误"}</span>
      <span className="status-sep">│</span>

      {totalCache > 0 && (
        <>
          <span className="status-item" title={`命中 ${sessionCache.hit}/${totalCache}`}>
            {isIcon ? "💡" : "缓存:"}
            <span style={{ color: cacheColor, fontWeight: 600 }}>{cacheRate}%</span>
          </span>
          <span className="status-sep">│</span>
        </>
      )}

      {lastUsage ? (
        <>
          <span className="status-item" title="Token">
            {isIcon ? "↑↓" : "Token:"}
            <span style={{ color: "var(--text-2)" }}>
              {lastUsage.prompt_tokens.toLocaleString()}↑{lastUsage.completion_tokens.toLocaleString()}↓
            </span>
          </span>
          {lastUsage.reasoning_tokens > 0 && (
            <span className="status-item" title="推理">
              <span style={{ color: "var(--amber)" }}>🧠</span>{lastUsage.reasoning_tokens.toLocaleString()}
            </span>
          )}
          <span className="status-sep">│</span>
        </>
      ) : (
        <span className="status-item" style={{ color: "var(--text-muted)" }}>
          {isIcon ? "⚡" : "暂无数据"}
        </span>
      )}

      {totalTokens > 0 && (
        <>
          <span className="status-item" title="总计">{isIcon ? "Σ" : "总计:"}{totalTokens.toLocaleString()}</span>
          <span className="status-sep">│</span>
        </>
      )}

      <span className="status-item" title="时长">{isIcon ? "⏱" : "时长:"}{fmtDur(sessionDuration)}</span>

      <span className="status-spacer" />
      <button className="status-toggle-btn" onClick={toggleDisplayMode}>{isIcon ? "Aa" : "📦"}</button>
      <span className="status-sep">│</span>
      <span className="status-item" style={{ color: "var(--text-3)" }}>DeepseekNova</span>
    </footer>
  );
}
