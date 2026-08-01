/**
 * StatusBar.tsx — 底部状态栏（mockup 定稿 · Hermes 风格详细数据）
 * 状态 | 模型 | 步数 | 工具 | 时长 ─ ↑↓推理 tokens | 速度 | 首字 | 缓存格 | 上下文格 | 费用(估算)
 * 仅订阅低频派生数据；数字 tabular-nums 防布局抖动。
 */

import { useStore } from "../store";
import { useTheme } from "../store/theme";
import { useI18n } from "../i18n";
import { useEffect, useState } from "react";
import { estimateCost } from "../lib/pricing";

/** 方块格进度：n 总格数 / f 填充格数（灰色填充） */
function Cells({ n, f }: { n: number; f: number }) {
  return (
    <span className="cells">
      {Array.from({ length: n }, (_, i) => (
        <i key={i} className={i < f ? "f" : ""} />
      ))}
    </span>
  );
}

const CONTEXT_LIMIT = 64_000;

export default function StatusBar() {
  const { t } = useI18n();
  const status = useStore((s) => s.status);
  const model = useStore((s) => s.model);
  const lastUsage = useStore((s) => s.lastUsage);
  const sessionCache = useStore((s) => s.sessionCache);
  const runToolCalls = useStore((s) => s.runToolCalls);
  const ttftMs = useStore((s) => s.ttftMs);
  const runElapsedMs = useStore((s) => s.runElapsedMs);
  const capabilities = useStore((s) => s.capabilities);

  const displayMode = useTheme((s) => s.displayMode);
  const toggleDisplayMode = useTheme((s) => s.toggleDisplayMode);
  const isIcon = displayMode === "icon";

  const [sessionDuration, setSessionDuration] = useState(0);
  useEffect(() => {
    const start = Date.now();
    const timer = setInterval(() => setSessionDuration(Math.floor((Date.now() - start) / 1000)), 1000);
    return () => clearInterval(timer);
  }, []);

  const fmtDur = (s: number) =>
    `${String(Math.floor(s / 60)).padStart(2, "0")}:${String(s % 60).padStart(2, "0")}`;

  const maxSteps = capabilities?.max_steps_default ?? 25;

  // 缓存命中率 → 10 格方块
  const totalCache = sessionCache.hit + sessionCache.miss;
  const cacheRate = totalCache > 0 ? Math.round((sessionCache.hit / totalCache) * 100) : 0;

  // 上下文占用 → 10 格方块
  const ctxTokens = lastUsage?.total_tokens ?? 0;
  const ctxPct = Math.min(100, (ctxTokens / CONTEXT_LIMIT) * 100);

  // 速度（run 结束后按平均值估算）与首字延迟
  const tokPerSec =
    lastUsage && runElapsedMs && runElapsedMs > 0
      ? Math.round((lastUsage.completion_tokens / runElapsedMs) * 1000)
      : null;

  // 费用估算
  const cost = lastUsage
    ? estimateCost(model, lastUsage.prompt_tokens, lastUsage.cache_hit_tokens, lastUsage.completion_tokens)
    : null;

  return (
    <footer className="status-bar">
      <span className={`status-dot ${status}`} />
      <span className="status-item">
        {status === "ready" ? t("app.ready") : status === "running" ? t("app.running") : "Error"}
      </span>
      <span className="status-vsep" />

      <span className="status-item mono" title="当前模型">{model}</span>
      <span className="status-item" title="工具调用（本次运行）">
        {t("status.tools")} <b>{runToolCalls}/{maxSteps}</b>
      </span>
      <span className="status-item" title="会话时长">
        {t("status.duration")} <b>{fmtDur(sessionDuration)}</b>
      </span>

      <span className="status-spacer" />

      {lastUsage && (
        <>
          <span className="status-item" title="输入 tokens">↑ <b>{lastUsage.prompt_tokens.toLocaleString()}</b></span>
          <span className="status-item" title="输出 tokens">↓ <b>{lastUsage.completion_tokens.toLocaleString()}</b></span>
          {lastUsage.reasoning_tokens > 0 && (
            <span className="status-item" title="推理 tokens">{t("status.reasoning")} <b>{lastUsage.reasoning_tokens.toLocaleString()}</b></span>
          )}
          <span className="status-vsep" />
        </>
      )}

      {(tokPerSec !== null || ttftMs !== null) && (
        <>
          {tokPerSec !== null && (
            <span className="status-item" title="生成速度（平均）">{t("status.speed")} <b>{tokPerSec} tok/s</b></span>
          )}
          {ttftMs !== null && (
            <span className="status-item" title="首字延迟 TTFT">{t("status.ttft")} <b>{(ttftMs / 1000).toFixed(1)}s</b></span>
          )}
          <span className="status-vsep" />
        </>
      )}

      {totalCache > 0 && (
        <span className="status-item" title={`缓存命中 ${sessionCache.hit.toLocaleString()} / ${totalCache.toLocaleString()}`}>
          {t("status.cache")} <Cells n={10} f={Math.round(cacheRate / 10)} /> <b>{cacheRate}%</b>
        </span>
      )}
      <span className="status-item" title="上下文窗口占用">
        {t("status.context")} <Cells n={10} f={Math.round(ctxPct / 10)} />{" "}
        <b>{(ctxTokens / 1000).toFixed(1)}k/{CONTEXT_LIMIT / 1000}k</b>
      </span>

      {cost !== null && (
        <>
          <span className="status-vsep" />
          <span className="status-item" title="本次会话费用（前端估算）">{t("status.cost")} <b>≈¥{cost.toFixed(3)}</b></span>
        </>
      )}

      <span className="status-vsep" />
      <button className="status-toggle-btn" onClick={toggleDisplayMode}>{isIcon ? "Aa" : "📦"}</button>
      <span className="status-item" style={{ color: "var(--text-3)" }}>DeepseekNova</span>
    </footer>
  );
}
