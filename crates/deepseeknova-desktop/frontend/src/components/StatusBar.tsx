/**
 * StatusBar.tsx — 底部成本仪表盘（参考 Reasonix）
 *
 * Reasonix 特色：常驻显示缓存命中率、Token 消耗、会话时长
 * 去除与控制栏重复的信息（模型名、模式），只保留成本相关数据
 *
 * 布局：[状态指示灯] [缓存命中率+颜色] [Token 用量] [会话时长] ─── [显示模式切换] [品牌]
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

  // 会话时长计时器
  const [sessionDuration, setSessionDuration] = useState(0);
  useEffect(() => {
    const start = Date.now();
    const timer = setInterval(() => {
      setSessionDuration(Math.floor((Date.now() - start) / 1000));
    }, 1000);
    return () => clearInterval(timer);
  }, []);

  const formatDuration = (seconds: number) => {
    if (seconds < 60) return `${seconds}s`;
    const m = Math.floor(seconds / 60);
    const s = seconds % 60;
    return `${m}m ${s}s`;
  };

  // 缓存命中率
  const totalCache = sessionCache.hit + sessionCache.miss;
  const cacheRate = totalCache > 0 ? Math.round((sessionCache.hit / totalCache) * 100) : 0;
  const cacheColor =
    cacheRate >= 80 ? "var(--green)" : cacheRate >= 50 ? "var(--amber)" : "var(--red)";

  // Token 用量
  const promptTokens = lastUsage?.prompt_tokens ?? 0;
  const completionTokens = lastUsage?.completion_tokens ?? 0;
  const reasoningTokens = lastUsage?.reasoning_tokens ?? 0;

  return (
    <footer className="status-bar">
      {/* 状态指示灯 */}
      <span className={`status-dot ${status}`} />
      <span className="status-item" style={{ minWidth: 48 }}>
        {status === "ready" ? "就绪" : status === "running" ? "运行中" : "错误"}
      </span>
      <span className="status-sep">│</span>

      {/* 缓存命中率（Reasonix 特色） */}
      {totalCache > 0 ? (
        <>
          <span className="status-item" title={`命中 ${sessionCache.hit} / 共 ${totalCache}`}>
            {isIcon ? "💡" : "缓存:"}
            <span style={{ color: cacheColor, fontWeight: 600, margin: "0 2px" }}>
              {cacheRate}%
            </span>
            {isIcon && (
              <span style={{ color: "var(--text-muted)", fontSize: 10 }}>
                ({sessionCache.hit}/{totalCache})
              </span>
            )}
          </span>
          <span className="status-sep">│</span>
        </>
      ) : null}

      {/* Token 用量 */}
      {lastUsage ? (
        <>
          <span className="status-item" title="上一轮 Token 消耗">
            {isIcon ? "↑↓" : "Token:"}
            <span style={{ color: "var(--text-2)" }}>
              {promptTokens.toLocaleString()}↑ {completionTokens.toLocaleString()}↓
            </span>
          </span>
          {reasoningTokens > 0 && (
            <span className="status-item" title="推理 Token">
              <span style={{ color: "var(--amber)" }}>🧠</span>
              {reasoningTokens.toLocaleString()}
            </span>
          )}
          <span className="status-sep">│</span>
        </>
      ) : (
        <span className="status-item" style={{ color: "var(--text-muted)" }}>
          {isIcon ? "⚡" : "暂无数据"}
        </span>
      )}

      {/* 会话累计 Token */}
      {totalTokens > 0 && (
        <>
          <span className="status-item" title="会话累计 Token">
            {isIcon ? "Σ" : "总计:"}
            {totalTokens.toLocaleString()}
          </span>
          <span className="status-sep">│</span>
        </>
      )}

      {/* 会话时长 */}
      <span className="status-item" title="会话时长">
        {isIcon ? "⏱" : "时长:"}
        {formatDuration(sessionDuration)}
      </span>

      <span className="status-spacer" />

      {/* 显示模式切换 */}
      <button className="status-toggle-btn" onClick={toggleDisplayMode} title={isIcon ? "切换到文字模式" : "切换到图标模式"}>
        {isIcon ? "Aa" : "📦"}
      </button>
      <span className="status-sep">│</span>

      {/* 品牌 */}
      <span className="status-item" style={{ color: "var(--text-3)" }}>
        DeepseekNova · DeepSeek 原生
      </span>
    </footer>
  );
}
