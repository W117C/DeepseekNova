/**
 * lib/pricing.ts — 费用估算单价表（前端估算，UI 需标注「估算」）
 * 单位：元 / 1M tokens（可按官方价目表校正）
 */

export interface ModelPrice {
  /** 输入（缓存未命中） */
  input: number;
  /** 输入（缓存命中） */
  inputCached: number;
  /** 输出（含推理） */
  output: number;
}

const PRICES: Record<string, ModelPrice> = {
  default: { input: 4, inputCached: 0.8, output: 12 },
};

export function priceFor(model: string): ModelPrice {
  const key = Object.keys(PRICES).find((k) => k !== "default" && model.includes(k));
  return key ? PRICES[key] : PRICES.default;
}

/** 估算费用（元） */
export function estimateCost(
  model: string,
  promptTokens: number,
  cacheHitTokens: number,
  completionTokens: number
): number {
  const p = priceFor(model);
  const uncached = Math.max(0, promptTokens - cacheHitTokens);
  return (
    (uncached * p.input + cacheHitTokens * p.inputCached + completionTokens * p.output) / 1_000_000
  );
}
