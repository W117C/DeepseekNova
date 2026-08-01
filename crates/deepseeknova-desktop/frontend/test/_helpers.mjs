// 测试公共助手 — 不匹配 node --test 的测试文件模式，仅供 *.test.mjs 导入。
// 路线：用项目已有的 typescript devDependency 把 TS 源码转译成 JS，
// 经 data: URL 动态 import 后直接执行真实逻辑（零新增测试框架）。
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import ts from "typescript";

const here = dirname(fileURLToPath(import.meta.url));

/** 读取 frontend/ 下的源文件文本 */
export function readSource(relPath) {
  return readFileSync(join(here, "..", relPath), "utf8");
}

/** 把一段无相对导入的 TS 代码转译并作为 ES 模块加载 */
export async function importTsSnippet(tsCode) {
  const js = ts.transpileModule(tsCode, {
    compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2021 },
  }).outputText;
  return import("data:text/javascript;base64," + Buffer.from(js).toString("base64"));
}

/** 加载一个无相对导入的 TS 模块（如 src/lib/pricing.ts） */
export function importTsModule(relPath) {
  return importTsSnippet(readSource(relPath));
}

/** 从 src/i18n/index.ts 提取某字典的键集合（与 i18n.test.mjs 同款解析） */
export function i18nKeys(dictName) {
  const src = readSource("src/i18n/index.ts");
  const start = src.indexOf(`const ${dictName}: Record<string, string> = {`);
  const end = src.indexOf("\n};", start);
  if (start === -1 || end === -1) throw new Error(`dictionary ${dictName} not found`);
  const body = src.slice(start, end);
  return new Set([...body.matchAll(/^\s*"([^"]+)":/gm)].map((m) => m[1]));
}
