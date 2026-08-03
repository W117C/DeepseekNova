// 测试公共助手 — 不匹配 node --test 的测试文件模式，仅供 *.test.mjs 导入。
// 用项目已有的 esbuild 把 TS 源码转译为 ESM 后加载，让测试验证真实实现（M3）。
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { transformSync } from "esbuild";

const here = dirname(fileURLToPath(import.meta.url));
const frontendRoot = join(here, "..");

/** 读取 frontend/ 下的源文件文本 */
export function readSource(relPath) {
  return readFileSync(join(frontendRoot, relPath), "utf8");
}

/** 把一段 TS 源码转译为 ESM 并加载（支持 data: URL 无相对导入的片段） */
export async function importTsSnippet(tsCode) {
  const js = transformSync(tsCode, {
    loader: "ts",
    format: "esm",
    target: "es2022",
  }).code;
  return import("data:text/javascript;base64," + Buffer.from(js).toString("base64"));
}

/** 加载一个无相对导入的 TS 模块 */
export function importTsModule(relPath) {
  return importTsSnippet(readSource(relPath));
}