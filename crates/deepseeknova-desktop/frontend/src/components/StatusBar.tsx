/**
 * StatusBar.tsx — 底部状态栏
 * 模型 | 模式 | token 上下/下行 | 缓存命中率 | Agent 状态
 * 全局图标/文字双模式（从 theme store 读取）
 */

import { useStore } from "../store";
import { useTheme } from "../store/theme";

export default function StatusBar() {
  const model = useStore((s) => s.model);
  const mode = useStore((s) => s.mode);
  const status = useStore((s) => s.status);
  const lastUsage = useStore((s) => s.lastUsage);
  const sessionCache = useStore((s) => s.sessionCache);
  const totalTokens = useStore((s) => s.totalTokens);

  const displayMode = useTheme((s) => s.displayMode);
  const toggleDisplayMode = useTheme((s) => s.toggleDisplayMode);
  const isIcon = displayMode === "icon";

  const totalCache = sessionCache.hit + sessionCache.miss;
  const cacheRate = totalCache > 0 ? Math.round((sessionCache.hit / totalCache) * 100) : 0;

  // 模式显示
  const modeIcon = mode === "plan" ? "🔒" : mode === "act" ? "✋" : "🚀";
  const modeColor = mode === "plan" ? "var(--blue)" : mode === "act" ? "var(--amber)" : "var(--red)";

  return (
    <footer className="status-bar">
      <span className={`status-dot ${status}`} title={isIcon ? (status === "ready" ? "就绪" : status === "running" ? "运行中" : "错误") : ""} />

      {/* 模型名 */}
      <span className="status-item" style={{ color: "var(--accent)" }} title="当前模型">
        {model}
      </span>
      <span className="status-sep">|</span>

      {/* 模式 */}
      <span className="status-item" title="执行模式">
        <span className="icon-only">{modeIcon}</span>
        <span className="text-only">模式: </span>
        {!isIcon && (
          <span style={{ color: modeColor, fontWeight: 600 }}>{mode.toUpperCase()}</span>
        )}
      </span>
      <span className="status-sep">|</span>

      {/* Token 用量 */}
      {lastUsage ? (
        <>
          <span className="status-item" title="上一轮 Token">
            {isIcon
              ? `${lastUsage.prompt_tokens.toLocaleString()}↑ ${lastUsage.completion_tokens.toLocaleString()}↓`
              : `输入 ${lastUsage.prompt_tokens.toLocaleString()} · 输出 ${lastUsage.completion_tokens.toLocaleString()}`}
          </span>
          <span className="status-sep">|</span>

          {lastUsage.reasoning_tokens > 0 && (
            <>
              <span className="status-item" title="推理 Token">
                {isIcon ? `🧠 ${lastUsage.reasoning_tokens.toLocaleString()}` : `推理 ${lastUsage.reasoning_tokens.toLocaleString()}`}
              </span>
              <span className="status-sep">|</span>
            </>
          )}
        </>
      ) : (
        <span className="status-item" style={{ color: "var(--text-muted)" }}>
          {isIcon ? "⚡ 待对话" : "暂无 Token 数据"}
        </span>
      )}

      {/* 缓存命中率 */}
      {totalCache > 0 && (
        <>
          <span className="status-item" title="缓存命中率">
            {isIcon ? `💡 ${cacheRate}%` : `缓存 ${cacheRate}%`}
            <span style={{ color: "var(--text-muted)", marginLeft: "2px" }}>
              {isIcon
                ? `(${sessionCache.hit.toLocaleString()}/${totalCache.toLocaleString()})`
                : `命中 ${sessionCache.hit.toLocaleString()} / 共 ${totalCache.toLocaleString()}`}
            </span>
          </span>
          <span className="status-sep">|</span>
        </>
      )}

      {/* 会话总 Token */}
      {totalTokens > 0 && (
        <>
          <span className="status-item" title="会话累计 Token">
            {isIcon ? `Σ ${totalTokens.toLocaleString()}` : `总计 ${totalTokens.toLocaleString()}`}
          </span>
          <span className="status-sep">|</span>
        </>
      )}

      <span className="status-spacer" />

      {/* 显示模式切换按钮 */}
      <button className="status-toggle-btn" onClick={toggleDisplayMode} title={isIcon ? "切换到文字模式" : "切换到图标模式"}>
        {isIcon ? "Aa" : "📦"}
      </button>
      <span className="status-sep">|</span>

      <span className="status-item" style={{ color: "var(--text-3)" }}>
        DeepseekNova · DeepSeek 原生
      </span>
    </footer>
  );
}
