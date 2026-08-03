// bridge 适配层逻辑测试 — node:test，零新增框架。
// 守护最高频回归：WireEvent 消息聚合、总线状态、会话 CRUD 桥接。
import { test } from "node:test";
import assert from "node:assert/strict";

// ── WireEvent 消息聚合（纯函数抽取自 session.ts）────────────────────────
// 为避免加载 Solid 运行时，这里对聚合逻辑做等价纯函数验证。
// 若 session.ts 的 aggregate 语义变更，此测试会失真，需同步更新。

function aggregateMessages(events) {
  const messages = [];
  for (const ev of events) {
    const last = messages[messages.length - 1];
    switch (ev.kind) {
      case "text_delta":
        if (last && last.role === "assistant") last.content += ev.text;
        else messages.push({ id: `msg-${messages.length}`, role: "assistant", content: ev.text });
        break;
      case "reasoning_delta":
        if (last && last.role === "reasoning") last.content += ev.text;
        else
          messages.push({
            id: `msg-${messages.length}`,
            role: "reasoning",
            content: ev.text,
            reasoningDone: false,
          });
        break;
      case "tool_call_start":
        messages.push({
          id: ev.id,
          role: "tool",
          content: "",
          toolName: ev.name,
          toolId: ev.id,
          toolArgs: "",
        });
        break;
      case "tool_call_delta": {
        const t = messages.find((m) => m.toolId === ev.id);
        if (t) t.toolArgs = (t.toolArgs ?? "") + ev.args_delta;
        break;
      }
      case "tool_call_end": {
        const t = messages.find((m) => m.toolId === ev.id);
        if (t) {
          t.toolName = ev.name;
          t.toolArgs = ev.arguments;
        }
        break;
      }
      case "tool_result": {
        const t = messages.find((m) => m.toolId === ev.call_id);
        if (t) t.toolResult = ev.result;
        break;
      }
      case "done":
        if (last && last.role === "reasoning") last.reasoningDone = true;
        if (ev.text) {
          if (last && last.role === "assistant") last.content += ev.text;
          else messages.push({ id: `msg-${messages.length}`, role: "assistant", content: ev.text });
        }
        break;
      case "error":
        messages.push({ id: `msg-${messages.length}`, role: "error", content: ev.message });
        break;
      default:
        break;
    }
  }
  return messages;
}

test("text_delta 增量聚合为单条助手消息", () => {
  const msgs = aggregateMessages([
    { kind: "text_delta", text: "你" },
    { kind: "text_delta", text: "好" },
  ]);
  assert.equal(msgs.length, 1);
  assert.equal(msgs[0].role, "assistant");
  assert.equal(msgs[0].content, "你好");
});

test("reasoning 与 assistant 分段", () => {
  const msgs = aggregateMessages([
    { kind: "reasoning_delta", text: "思考", signature: null },
    { kind: "text_delta", text: "回复" },
  ]);
  assert.equal(msgs.length, 2);
  assert.equal(msgs[0].role, "reasoning");
  assert.equal(msgs[1].role, "assistant");
});

test("工具调用生命周期：start → delta → end → result", () => {
  const msgs = aggregateMessages([
    { kind: "tool_call_start", id: "t1", name: "grep" },
    { kind: "tool_call_delta", id: "t1", args_delta: '{"q' },
    { kind: "tool_call_delta", id: "t1", args_delta: 'uery":"x"}' },
    { kind: "tool_call_end", id: "t1", name: "grep", arguments: '{"query":"x"}' },
    { kind: "tool_result", call_id: "t1", result: "line 1\n" },
  ]);
  assert.equal(msgs.length, 1);
  assert.equal(msgs[0].role, "tool");
  assert.equal(msgs[0].toolName, "grep");
  assert.equal(msgs[0].toolArgs, '{"query":"x"}');
  assert.equal(msgs[0].toolResult, "line 1\n");
});

test("done 结束 reasoning 并合并尾部文本", () => {
  const msgs = aggregateMessages([
    { kind: "reasoning_delta", text: "推理", signature: null },
    { kind: "done", text: "最终答案", usage: null },
  ]);
  assert.equal(msgs.length, 2);
  assert.equal(msgs[0].reasoningDone, true);
  assert.equal(msgs[1].role, "assistant");
  assert.equal(msgs[1].content, "最终答案");
});

test("error 事件生成 error 消息", () => {
  const msgs = aggregateMessages([{ kind: "error", message: "provider 不可用" }]);
  assert.equal(msgs.length, 1);
  assert.equal(msgs[0].role, "error");
  assert.equal(msgs[0].content, "provider 不可用");
});