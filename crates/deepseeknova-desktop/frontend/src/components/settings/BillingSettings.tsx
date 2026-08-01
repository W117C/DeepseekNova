import { useState, useEffect } from "react";
import { useStore } from "../../store";
import { StatBox } from "./Shared";
import { getBillingStats } from "../../bridge";

export default function BillingSettings() {
  const sessionCache = useStore((s) => s.sessionCache);
  const totalTokens = useStore((s) => s.totalTokens);
  const lastUsage = useStore((s) => s.lastUsage);

  const [stats, setStats] = useState<any>(null);
  const [loading, setLoading] = useState(true);

  useEffect(() => {
    (async () => {
      try {
        const data = await getBillingStats();
        setStats(data);
      } catch {
        // Backend may not be fully initialized — fallback to store values
      } finally {
        setLoading(false);
      }
    })();
  }, []);

  // Prefer backend stats; fall back to store (live session) values
  const session = stats?.session ?? {
    prompt_tokens: totalTokens,
    completion_tokens: lastUsage?.completion_tokens ?? 0,
    total_tokens: totalTokens,
    cache_hit_tokens: sessionCache.hit,
    cache_miss_tokens: sessionCache.miss,
    reasoning_tokens: lastUsage?.reasoning_tokens ?? 0,
    cache_rate: 0,
    run_count: 0,
  };

  const flashInputPrice = 0.28;
  const flashCachedPrice = 0.028;
  const flashOutputPrice = 0.88;

  const totalCache = sessionCache.hit + sessionCache.miss;
  const cacheRate = totalCache > 0 ? (sessionCache.hit / totalCache) * 100 : 0;

  const inputCost = (sessionCache.miss / 1000000) * flashInputPrice;
  const cachedCost = (sessionCache.hit / 1000000) * flashCachedPrice;
  const outputCost = lastUsage ? (lastUsage.completion_tokens / 1000000) * flashOutputPrice : 0;
  const totalCost = inputCost + cachedCost + outputCost;

  const cost = stats?.cost;
  const inputCostValue = cost?.input_full ?? inputCost;
  const cachedCostValue = cost?.input_cached ?? cachedCost;
  const outputCostValue = cost?.output ?? outputCost;
  const totalCostValue = cost?.total ?? totalCost;

  const history = stats?.history ?? [];
  const isMock = stats?.mock === false;

  return (
    <div style={{ display: "flex", flexDirection: "column", gap: 10 }}>
      <div className="card" style={{ padding: 10 }}>
        <div style={{ fontSize: 12, fontWeight: 600, color: "var(--text-1)", marginBottom: 8 }}>本会话</div>
        <div style={{ display: "grid", gridTemplateColumns: "1fr 1fr", gap: 8 }}>
          <StatBox label="缓存命中" value={sessionCache.hit.toLocaleString()} sub={`${cacheRate.toFixed(1)}%`} color="var(--green)" />
          <StatBox label="未缓存" value={sessionCache.miss.toLocaleString()} sub="按全价" color="var(--amber)" />
          <StatBox label="输出" value={lastUsage?.completion_tokens.toLocaleString() || "0"} sub={`推理 ${lastUsage?.reasoning_tokens || 0}`} color="var(--blue)" />
          <StatBox label="总计" value={totalTokens.toLocaleString()} sub={`运行 ${session.run_count ?? 0} 次`} color="var(--accent)" />
        </div>
      </div>

      <div className="card" style={{ padding: 10 }}>
        <div style={{ fontSize: 12, fontWeight: 600, color: "var(--text-1)", marginBottom: 8 }}>费用明细（V4 Flash）</div>
        <div style={{ display: "flex", flexDirection: "column", gap: 4 }}>
          <div style={{ display: "flex", justifyContent: "space-between", fontSize: 11 }}>
            <span style={{ color: "var(--text-2)" }}>输入（全价）</span>
            <span style={{ color: "var(--text-3)", fontSize: 10 }}>{sessionCache.miss.toLocaleString()} × ¥{flashInputPrice}/M</span>
            <span style={{ color: "var(--text-1)", fontWeight: 500, minWidth: 60, textAlign: "right" }}>¥{inputCostValue.toFixed(4)}</span>
          </div>
          <div style={{ display: "flex", justifyContent: "space-between", fontSize: 11 }}>
            <span style={{ color: "var(--text-2)" }}>输入（缓存）</span>
            <span style={{ color: "var(--text-3)", fontSize: 10 }}>{sessionCache.hit.toLocaleString()} × ¥{flashCachedPrice}/M</span>
            <span style={{ color: "var(--text-1)", fontWeight: 500, minWidth: 60, textAlign: "right" }}>¥{cachedCostValue.toFixed(4)}</span>
          </div>
          <div style={{ display: "flex", justifyContent: "space-between", fontSize: 11 }}>
            <span style={{ color: "var(--text-2)" }}>输出</span>
            <span style={{ color: "var(--text-3)", fontSize: 10 }}>{(lastUsage?.completion_tokens || 0).toLocaleString()} × ¥{flashOutputPrice}/M</span>
            <span style={{ color: "var(--text-1)", fontWeight: 500, minWidth: 60, textAlign: "right" }}>¥{outputCostValue.toFixed(4)}</span>
          </div>
          <div style={{ borderTop: "1px solid var(--border-1)", marginTop: 4, paddingTop: 4, display: "flex", justifyContent: "space-between" }}>
            <span style={{ fontSize: 12, fontWeight: 600, color: "var(--text-1)" }}>总计</span>
            <span style={{ fontSize: 14, fontWeight: 700, color: "var(--accent)" }}>¥{totalCostValue.toFixed(4)}</span>
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
        {loading ? (
          <div style={{ fontSize: 10, color: "var(--text-3)" }}>加载中…</div>
        ) : history.length > 0 ? (
          <div style={{ display: "flex", flexDirection: "column", gap: 4, fontSize: 11 }}>
            {history.map((h: any, i: number) => (
              <div key={i} style={{ display: "flex", justifyContent: "space-between" }}>
                <span style={{ color: "var(--text-2)" }}>{h.label}</span>
                <span style={{ color: "var(--text-1)" }}>{h.sessions} 会话 · ¥{h.cost}</span>
              </div>
            ))}
          </div>
        ) : (
          <div style={{ fontSize: 10, color: "var(--text-3)" }}>
            {isMock ? "历史数据暂未接入" : "暂无历史会话数据"}
          </div>
        )}
      </div>
    </div>
  );
}
