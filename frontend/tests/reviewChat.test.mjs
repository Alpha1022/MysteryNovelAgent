import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import ts from "typescript";

const source = readFileSync(new URL("../src/utils/reviewChat.ts", import.meta.url), "utf8");
const { outputText } = ts.transpileModule(source, {
  compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2021 },
});
const { buildReviewPrompt, startReviewChat, REVIEW_CHAT_EVENT } = await import(
  `data:text/javascript;base64,${Buffer.from(outputText).toString("base64")}`
);

test("读后讨论明确定位本地书籍，并等待用户决定生成和保存", () => {
  const prompt = buildReviewPrompt({ id: 42, title: "测试书", author: "作者", tags: "本格" });
  assert.match(prompt, /《测试书》.*作者.*book_id：42.*本格/);
  assert.match(prompt, /已经标记为已读，无需再次修改阅读状态/);
  assert.match(prompt, /只有我明确要求生成时/);
  assert.match(prompt, /不要因我结束聊天就生成/);
  assert.match(prompt, /只有我明确要求保存时/);
});

test("书籍信息缺失时仍可讨论", () => {
  const prompt = buildReviewPrompt({ id: 7, title: "无资料", author: "", tags: "" });
  assert.match(prompt, /佚名/);
  assert.match(prompt, /推理小说/);
});

test("连续标记不同书籍会分别发出完整的讨论请求", () => {
  const previous = globalThis.window;
  const events = [];
  globalThis.window = { dispatchEvent: (event) => events.push(event) };
  try {
    const books = [1, 2].map((id) => ({ id, title: `书${id}`, author: "", tags: "" }));
    books.forEach(startReviewChat);
    assert.deepEqual(events.map((event) => event.type), [REVIEW_CHAT_EVENT, REVIEW_CHAT_EVENT]);
    assert.deepEqual(events.map((event) => event.detail), books);
  } finally {
    globalThis.window = previous;
  }
});
