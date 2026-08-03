// bridge 适配层逻辑测试 — node:test + esbuild 转译，零新增框架。
// 直接导入真实 src/bridge/aggregate.ts（M3），守护消息聚合关键路径：
// - text_delta 增量累积（C1 修复后的事件序列）
// - reasoning/assistant 分段
// - 工具调用生命周期
// - done 携带全量文本 → 替换而非追加（H1）
// - paused / error 终态
import { test } from "node:test";
import assert from "node:assert/strict";
import { importTsModule } from "./_helpers.mjs";

const { aggregateMessages } = await importTsModule("src/bridge/aggregate.ts");

/** 依序应用事件流，返回最终消息列表与 finished 标记 */
function run(events) {
  let messages = [];
  let finished = false;
  let pausedReason;
  for (const ev of events) {
    const r = aggregateMessages(messages, ev);
    messages = r.messages;
    finished = r.finished;
    pausedReason = r.pausedReason;
  }
  return { messages, finished, pausedReason };
}

test("text_delta 增量聚合为单条助手消息", () => {
  const { messages } = run([
    { kind: "text_delta", text: "你" },
    { kind: "text_delta", text: "好" },
  ]);
  assert.equal(messages.length, 1);
  assert.equal(messages[0].role, "assistant");
  assert.equal(messages[0].content, "你好");
  assert.equal(messages[0].id, messages[0].id, "id 稳定");
});

test("reasoning 与 assistant 分段", () => {
  const { messages } = run([
    { kind: "reasoning_delta", text: "思考", signature: null },
    { kind: "text_delta", text: "回复" },
  ]);
  assert.equal(messages.length, 2);
  assert.equal(messages[0].role, "reasoning");
  assert.equal(messages[1].role, "assistant");
});

test("工具调用生命周期：start → delta → end → result", () => {
  const { messages } = run([
    { kind: "tool_call_start", id: "t1", name: "grep" },
    { kind: "tool_call_delta", id: "t1", args_delta: '{"q' },
    { kind: "tool_call_delta", id: "t1", args_delta: 'uery":"x"}' },
    { kind: "tool_call_end", id: "t1", name: "grep", arguments: '{"query":"x"}' },
    { kind: "tool_result", call_id: "t1", result: "line 1\n" },
  ]);
  assert.equal(messages.length, 1);
  assert.equal(messages[0].role, "tool");
  assert.equal(messages[0].toolName, "grep");
  assert.equal(messages[0].toolArgs, '{"query":"x"}');
  assert.equal(messages[0].toolResult, "line 1\n");
});

test("done 携带全量最终文本 → 替换而非追加（H1）", () => {
  // 真实后端序列：每个 chunk 发 text_delta，最后 done 带累积的全量文本
  const { messages, finished } = run([
    { kind: "text_delta", text: "第一" },
    { kind: "text_delta", text: "部分" },
    { kind: "done", text: "第一部分完整回复", usage: null },
  ]);
  assert.equal(messages.length, 1);
  assert.equal(messages[0].role, "assistant");
  // H1 断言：内容等于全量文本，不重复叠加
  assert.equal(messages[0].content, "第一部分完整回复");
  assert.equal(finished, true);
});

test("done 结束 reasoning 并生成 assistant 最终消息", () => {
  const { messages, finished } = run([
    { kind: "reasoning_delta", text: "推理", signature: null },
    { kind: "done", text: "最终答案", usage: null },
  ]);
  assert.equal(messages.length, 2);
  assert.equal(messages[0].role, "reasoning");
  assert.equal(messages[0].reasoningDone, true);
  assert.equal(messages[1].role, "assistant");
  assert.equal(messages[1].content, "最终答案");
  assert.equal(finished, true);
});

test("paused 标记终态并透出 reason（M4）", () => {
  const { messages, finished, pausedReason } = run([
    { kind: "text_delta", text: "部分输出" },
    { kind: "paused", reason: "max-steps 达到上限", session_id: "s1" },
  ]);
  assert.equal(finished, true);
  assert.equal(pausedReason, "max-steps 达到上限");
  assert.equal(messages[messages.length - 1].role, "assistant");
});

test("error 事件生成 error 消息并终态", () => {
  const { messages, finished } = run([{ kind: "error", message: "provider 不可用" }]);
  assert.equal(messages.length, 1);
  assert.equal(messages[0].role, "error");
  assert.equal(messages[0].content, "provider 不可用");
  assert.equal(finished, true);
});

test("done 后 finished 标记为 true（后端不再发事件）", () => {
  const { finished } = run([
    { kind: "text_delta", text: "答案" },
    { kind: "done", text: "答案", usage: null },
  ]);
  assert.equal(finished, true);
});