// i18n 字典结构测试 — node:test，零依赖，静态解析 src/i18n/index.ts。
// 防两类 tsc 查不出的真实缺陷：zh/en 键位不对齐（漏翻译）、字典内重复键。
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const src = readFileSync(join(here, "../src/i18n/index.ts"), "utf8");

function extractKeys(dictName) {
  const start = src.indexOf(`const ${dictName}: Record<string, string> = {`);
  assert.notEqual(start, -1, `dictionary ${dictName} not found`);
  const end = src.indexOf("\n};", start);
  assert.notEqual(end, -1, `dictionary ${dictName} not terminated`);
  const body = src.slice(start, end);
  return [...body.matchAll(/^\s*"([^"]+)":/gm)].map((m) => m[1]);
}

test("zh/en dictionaries have identical key sets", () => {
  const zh = extractKeys("zh");
  const en = extractKeys("en");
  assert.ok(zh.length > 100, `zh dictionary suspiciously small: ${zh.length}`);
  const zhSet = new Set(zh);
  const enSet = new Set(en);
  const missingInEn = zh.filter((k) => !enSet.has(k));
  const missingInZh = en.filter((k) => !zhSet.has(k));
  assert.deepEqual(missingInEn, [], `keys missing in en: ${missingInEn.join(", ")}`);
  assert.deepEqual(missingInZh, [], `keys missing in zh: ${missingInZh.join(", ")}`);
});

test("no duplicate keys within a dictionary", () => {
  for (const name of ["zh", "en"]) {
    const keys = extractKeys(name);
    const dupes = [...new Set(keys.filter((k, i) => keys.indexOf(k) !== i))];
    assert.deepEqual(dupes, [], `${name} has duplicate keys: ${dupes.join(", ")}`);
  }
});
