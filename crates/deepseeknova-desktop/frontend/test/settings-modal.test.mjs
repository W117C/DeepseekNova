// SettingsModal 纯逻辑行为测试 — node:test，零新增框架。
// 守护最高频回归：新增分区漏配右侧渲染分支 / 漏配 zh・en 翻译键 / 分组丢失。
import { test } from "node:test";
import assert from "node:assert/strict";
import { readSource, i18nKeys } from "./_helpers.mjs";

const src = readSource("src/components/SettingsModal.tsx");

/** sections 数组条目：{ id: "x", label: t("ssec.y"), group: t("sgroup.z") } */
function parseSections() {
  const entries = [...src.matchAll(
    /\{ id: "([a-z]+)", label: t\("(ssec\.[a-zA-Z]+)"\), group: t\("(sgroup\.[a-zA-Z]+)"\) \}/g
  )].map(([, id, labelKey, groupKey]) => ({ id, labelKey, groupKey }));
  assert.ok(entries.length >= 20, `suspiciously few sections parsed: ${entries.length}`);
  return entries;
}

test("every declared section has a render branch, and no orphan branches", () => {
  const declared = parseSections().map((s) => s.id);
  const rendered = [...src.matchAll(/section === "([a-z]+)" &&/g)].map((m) => m[1]);
  const declaredSet = new Set(declared);
  const renderedSet = new Set(rendered);

  const unrendered = declared.filter((id) => !renderedSet.has(id));
  const orphans = rendered.filter((id) => !declaredSet.has(id));
  assert.deepEqual(unrendered, [], `sections without render branch: ${unrendered.join(", ")}`);
  assert.deepEqual(orphans, [], `render branches without nav entry: ${orphans.join(", ")}`);
});

test("section ids are unique and organized into 6 groups", () => {
  const sections = parseSections();
  const ids = sections.map((s) => s.id);
  assert.equal(new Set(ids).size, ids.length, "duplicate section ids");
  // mockup 定稿：6 组（应用/模型与执行/能力/安全/数据/系统）
  assert.equal(new Set(sections.map((s) => s.groupKey)).size, 6);
});

test("all ssec/sgroup keys used by SettingsModal exist in zh and en", () => {
  const zh = i18nKeys("zh");
  const en = i18nKeys("en");
  const used = parseSections().flatMap((s) => [s.labelKey, s.groupKey]);
  const missing = [...new Set(used)].filter((k) => !zh.has(k) || !en.has(k));
  assert.deepEqual(missing, [], `SettingsModal keys missing from dictionaries: ${missing.join(", ")}`);
});
