// StatusBar 纯逻辑行为测试 — node:test，零新增框架。
// 覆盖：fmtDur 时长格式化（真实源码片段转译后执行）、
// estimateCost 费用估算（StatusBar 的费用行依赖 src/lib/pricing.ts）、
// StatusBar 引用的 i18n 键必须在 zh/en 字典中存在。
import { test } from "node:test";
import assert from "node:assert/strict";
import { readSource, importTsSnippet, importTsModule, i18nKeys } from "./_helpers.mjs";

const statusBarSrc = readSource("src/components/StatusBar.tsx");

test("fmtDur formats seconds as zero-padded mm:ss", async () => {
  const snippet = statusBarSrc.match(/const fmtDur = \(s: number\) =>[\s\S]*?;/);
  assert.ok(snippet, "fmtDur not found in StatusBar.tsx");
  const { fmtDur } = await importTsSnippet(`export ${snippet[0]}`);
  assert.equal(fmtDur(0), "00:00");
  assert.equal(fmtDur(5), "00:05");
  assert.equal(fmtDur(65), "01:05");
  assert.equal(fmtDur(3599), "59:59");
});

test("estimateCost applies cache discount and clamps uncached at zero", async () => {
  const { estimateCost, priceFor } = await importTsModule("src/lib/pricing.ts");

  // 未知模型回退 default 价目：input 4 / cached 0.8 / output 12（元/1M tokens）
  const p = priceFor("some-unknown-model");
  assert.equal(p.input, 4);

  // 1M 未命中输入 + 0 缓存 + 0 输出 = 4 元
  assert.equal(estimateCost("m", 1_000_000, 0, 0), 4);
  // 全部命中缓存时按 cached 单价计
  assert.equal(estimateCost("m", 1_000_000, 1_000_000, 0), 0.8);
  // cache_hit > prompt 的异常上报不得算出负费用
  assert.ok(estimateCost("m", 100, 200, 0) >= 0);
  // 输出 tokens 按 output 单价计
  assert.equal(estimateCost("m", 0, 0, 500_000), 6);
});

test("all i18n keys referenced by StatusBar exist in zh and en", () => {
  const used = [...statusBarSrc.matchAll(/\bt\("([^"]+)"\)/g)].map((m) => m[1]);
  assert.ok(used.length >= 5, `suspiciously few t() keys found: ${used.length}`);
  const zh = i18nKeys("zh");
  const en = i18nKeys("en");
  const missing = used.filter((k) => !zh.has(k) || !en.has(k));
  assert.deepEqual(missing, [], `StatusBar keys missing from dictionaries: ${missing.join(", ")}`);
});
