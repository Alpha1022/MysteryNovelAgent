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

test("读后讨论明确定位本地书籍，并等待用户决定生成后保存", () => {
  const prompt = buildReviewPrompt({ id: 42, title: "测试书", author: "作者", tags: "本格" });
  assert.match(prompt, /《测试书》.*作者.*book_id：42.*本格/);
  assert.match(prompt, /已经标记为已读，无需再次修改阅读状态/);
  assert.match(prompt, /只有我明确要求生成时/);
  assert.match(prompt, /不要因我结束聊天就生成/);
  assert.match(prompt, /当我明确要求生成书评时，也授权将生成的书评保存到这本书/);
  assert.match(prompt, /book_id：42、status=已读 和 review 保存书评/);
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


test("鼓励查阅豆瓣并保留来源、个人观点和联网失败降级边界", () => {
  const prompt = buildReviewPrompt({ id: 42, title: "测试书", author: "作者", tags: "" });
  for (const tool of ["get_douban_comments", "get_douban_reviews", "get_douban_review_full"]) {
    assert.ok(prompt.includes(tool));
  }
  assert.match(prompt, /不要把本地 book_id 当作 douban_id，也不要猜测 ID/);
  assert.match(prompt, /注明观点来源/);
  assert.match(prompt, /以我在对话中表达的实际感受为主/);
  assert.match(prompt, /豆瓣不可访问或没有对应条目时，说明情况并继续/);
});
