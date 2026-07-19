import { useStore } from "../../store";
import { StatBox } from "./Shared";

export default function BillingSettings() {
  const sessionCache = useStore((s) => s.sessionCache);
  const totalTokens = useStore((s) => s.totalTokens);
  const lastUsage = useStore((s) => s.lastUsage);

  const totalCache = sessionCache.hit + sessionCache.miss;
  const cacheRate = totalCache > 0 ? (sessionCache.hit / totalCache) * 100 : 0;

  const flashInputPrice = 0.28;
  const flashCachedPrice = 0.028;
  const flashOutputPrice = 0.88;

  const inputCost = (sessionCache.miss / 1000000) * flashInputPrice;
  const cachedCost = (sessionCache.hit / 1000000) * flashCachedPrice;
  const outputCost = lastUsage ? (lastUsage.completion_tokens / 1000000) * flashOutputPrice : 0;
  const totalCost = inputCost + cachedCost + outputCost;

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 10 }}>
      <div className="card" style={{ padding: 10 }}>
        <div style={{ fontSize: 12, fontWeight: 600, color: "var(--text-1)", marginBottom: 8 }}>本会话</div>
        <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: 8 }}>
          <StatBox label="缓存命中" value={sessionCache.hit.toLocaleString()} sub={`${cacheRate.toFixed(1)}%`} color="var(--green)" />
          <StatBox label="未缓存" value={sessionCache.miss.toLocaleString()} sub="按全价" color="var(--amber)" />
          <StatBox label="输出" value={lastUsage?.completion_tokens.toLocaleString() || "0"} sub={`推理 ${lastUsage?.reasoning_tokens || 0}`} color="var(--blue)" />
          <StatBox label="总计" value={totalTokens.toLocaleString()} sub="累计" color="var(--accent)" />
        </div>
      </div>

      <div className="card" style={{ padding: 10 }}>
        <div style={{ fontSize: 12, fontWeight: 600, color: "var(--text-1)", marginBottom: 8 }}>费用明细（V4 Flash）</div>
        <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
          <div style={{ display: "flex", justifyContent: "space-between", fontSize: 11 }}>
            <span style={{ color: "var(--text-2)" }}>输入（全价）</span>
            <span style={{ color: "var(--text-3)", fontSize: 10 }}>{sessionCache.miss.toLocaleString()} × ¥{flashInputPrice}/M</span>
            <span style={{ color: "var(--text-1)", fontWeight: 500, minWidth: 60, textAlign: "right" }}>¥{inputCost.toFixed(4)}</span>
          </div>
          <div style={{ display: "flex", justifyContent: "space-between", fontSize: 11 }}>
            <span style={{ color: "var(--text-2)" }}>输入（缓存）</span>
            <span style={{ color: "var(--text-3)", fontSize: 10 }}>{sessionCache.hit.toLocaleString()} × ¥{flashCachedPrice}/M</span>
            <span style={{ color: "var(--text-1)", fontWeight: 500, minWidth: 60, textAlign: "right" }}>¥{cachedCost.toFixed(4)}</span>
          </div>
          <div style={{ display: "flex", justifyContent: "space-between", fontSize: 11 }}>
            <span style={{ color: "var(--text-2)" }}>输出</span>
            <span style={{ color: "var(--text-3)", fontSize: 10 }}>{(lastUsage?.completion_tokens || 0).toLocaleString()} × ¥{flashOutputPrice}/M</span>
            <span style={{ color: "var(--text-1)", fontWeight: 500, minWidth: 60, textAlign: "right" }}>¥{outputCost.toFixed(4)}</span>
          </div>
          <div style={{ borderTop: "1px solid var(--border-1)", marginTop: 4, paddingTop: 4, display: "flex", justifyContent: "space-between" }}>
            <span style={{ fontSize: 12, fontWeight: 600, color: "var(--text-1)" }}>总计</span>
            <span style={{ fontSize: 14, fontWeight: 700, color: "var(--accent)" }}>¥{totalCost.toFixed(4)}</span>
          </div>
        </div>
      </div>

      <div className="card" style={{ padding: 10 }}>
        <div style={{ fontSize: 12, fontWeight: 600, color: "var(--text-1)", marginBottom: 6 }}>缓存分析</div>
        <div style={{ height: 8, borderRadius: 4, background: "var(--bg-3)", overflow: "hidden", display: "flex" }}>
          <div style={{ width: `${cacheRate}%`, background: "var(--green)", transition: "width 0.3s" }} />
          <div style={{ width: `${100 - cacheRate}%`, background: "var(--amber)", transition: "width 0.3s" }} />
        </div>
        <div style={{ display: "flex", justifyContent: "space-between", marginTop: 6, fontSize: 10, color: "var(--text-3)" }}>
          <span>🟢 命中 {cacheRate.toFixed(1)}%</span>
          <span>🟡 未缓存 {(100 - cacheRate).toFixed(1)}%</span>
        </div>
      </div>

      <div className="card" style={{ padding: 10 }}>
        <div style={{ fontSize: 12, fontWeight: 600, color: "var(--text-1)", marginBottom: 6 }}>历史会话</div>
        <div style={{ display: "flex", flexDirection: "column", gap: 4, fontSize: 11 }}>
          <div style={{ display: "flex", justifyContent: "space-between" }}>
            <span style={{ color: "var(--text-2)" }}>今天</span>
            <span style={{ color: "var(--text-1)" }}>3 会话 · ¥0.42</span>
          </div>
          <div style={{ display: "flex", justifyContent: "space-between" }}>
            <span style={{ color: "var(--text-2)" }}>昨天</span>
            <span style={{ color: "var(--text-1)" }}>5 会话 · ¥0.78</span>
          </div>
          <div style={{ display: "flex", justifyContent: "space-between" }}>
            <span style={{ color: "var(--text-2)" }}>本周</span>
            <span style={{ color: "var(--text-1)" }}>18 会话 · ¥2.14</span>
          </div>
          <div style={{ display: "flex", justifyContent: "space-between" }}>
            <span style={{ color: "var(--text-2)" }}>本月</span>
            <span style={{ color: "var(--text-1)" }}>62 会话 · ¥8.45</span>
          </div>
        </div>
      </div>
    </div>
  );
}

