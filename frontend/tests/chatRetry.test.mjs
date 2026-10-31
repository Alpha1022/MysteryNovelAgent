import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import ts from "typescript";

const { outputText } = ts.transpileModule(
  readFileSync(new URL("../src/utils/chatRetry.ts", import.meta.url), "utf8"),
  { compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2021 } },
);
const { prepareRetry, canSaveReview } = await import(
  `data:text/javascript;base64,${Buffer.from(outputText).toString("base64")}`
);
const messages = [
  { role: "user", content: "讨论这本书" },
  { role: "assistant", content: "感受如何？", steps: [{ tool: "get_book_detail" }] },
  { role: "user", content: "结尾出乎意料" },
  { role: "assistant", content: "你如何理解结尾？" },
  { role: "user", content: "后续消息" },
];

test("重发中间的用户消息保留前文，清理后续且不修改原历史", () => {
  const original = structuredClone(messages);
  assert.deepEqual(prepareRetry(messages, 2), { history: messages.slice(0, 2), text: messages[2].content });
  assert.deepEqual(messages, original);
});

test("刷新助手回复重新发送上一个用户问题，不把助手文字变成用户消息", () => {
  assert.deepEqual(prepareRetry(messages, 3), { history: messages.slice(0, 2), text: messages[2].content });
  assert.deepEqual(prepareRetry(messages, 1), { history: [], text: messages[0].content });
});

test("首条消息和失败后尚无回复的消息均可重发", () => {
  assert.deepEqual(prepareRetry(messages, 0), { history: [], text: messages[0].content });
  assert.deepEqual(prepareRetry(messages, 4), { history: messages.slice(0, 4), text: messages[4].content });
});

test("不完整或非法历史不会发起错误请求", () => {
  assert.equal(prepareRetry([], 0), null);
  assert.equal(prepareRetry(messages, -1), null);
  assert.equal(prepareRetry(messages, 1.5), null);
  assert.equal(prepareRetry(messages, 99), null);
  assert.equal(prepareRetry([{ role: "assistant", content: "导入的孤立回复" }], 0), null);
});

test("直接保存授权只适用于确认过的目标书，普通会话和其他书仍须确认", () => {
  assert.equal(canSaveReview(42, 42, "已读"), true);
  assert.equal(canSaveReview(undefined, 42, "已读"), false);
  assert.equal(canSaveReview(42, 43, "已读"), false);
  assert.equal(canSaveReview(42, 42, "想读"), false);
});
