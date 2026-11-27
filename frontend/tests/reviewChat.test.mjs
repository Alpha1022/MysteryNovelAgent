import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import ts from "typescript";

const source = readFileSync(new URL("../src/utils/reviewChat.ts", import.meta.url), "utf8");
const { outputText } = ts.transpileModule(source, {
  compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2021 },
});
const { buildReviewPrompt, startReviewChat, REVIEW_CHAT_EVENT, isReviewDraftCommand, prepareReviewReply } = await import(
  `data:text/javascript;base64,${Buffer.from(outputText).toString("base64")}`
);

test("读后讨论明确定位本地书籍，并等待用户决定生成后保存", () => {
  const prompt = buildReviewPrompt({ id: 42, title: "测试书", author: "作者", tags: "本格" });
  assert.match(prompt, /《测试书》.*作者.*book_id：42.*本格/);
  assert.match(prompt, /已经标记为已读，无需再次修改阅读状态/);
  assert.match(prompt, /只有我明确要求生成时/);
  assert.match(prompt, /不要因我结束聊天就生成/);
  assert.match(prompt, /我点击保存后才写入这本书/);
  assert.match(prompt, /book_id：42、status=已读 和完整的 review 正文/);
});

const bookId = 452;
const review = "我喜欢结尾的照应。\n\n中段的几条线索值得重读。";
const receipt = "已在界面弹出保存窗口，请确认后才会写入（book_id 452，状态保持「已读」）。";
const action = { kind: "status", bookId, status: "已读", review };
const marked = `<book_review book_id="${bookId}">${review}</book_review>`;
const reply = (content, steps = []) => ({ content, steps, model: "test" });

test("工具正文优先，重复出现在文本中也只打开一个编辑框", () => {
  for (const text of [receipt, marked]) {
    const result = prepareReviewReply(reply(text), [action], bookId, true);
    assert.deepEqual(result.actions, [action]);
    assert.equal(result.message.content, review);
  }
});

test("不支持工具时按草稿标记处理，读后讨论明确生成也接受纯正文", () => {
  for (const [text, id, requested] of [[marked, undefined, false], [marked, bookId, false], [review, bookId, true], [review + "\n\n" + receipt, bookId, true]]) {
    const result = prepareReviewReply(reply(text), [], id, requested);
    assert.deepEqual(result.actions, [action]);
    assert.equal(result.message.content, review);
  }
  const result = prepareReviewReply(reply(receipt), [], bookId, true);
  assert.deepEqual(result.actions, []);
  assert.doesNotMatch(result.message.content, /已在界面弹出/);
});

test("普通聊天、错误目标、坏标记和工具报错不被当成可保存的正文", () => {
  for (const [text, id, requested, steps] of [
    [review, bookId, false], [review, undefined, false], [marked, bookId + 1, true],
    ['<book_review book_id="452">漏了结束标记', bookId, true],
    [marked + marked, bookId, true],
    ["书籍不存在", bookId, true, [{ tool: "set_book_status", pending_confirmation: false }]],
  ]) {
    assert.deepEqual(prepareReviewReply(reply(text, steps), [], id, requested).actions, []);
  }
  assert.equal(isReviewDraftCommand("请帮我生成一篇短评。"), true);
  for (const text of ["先别生成短评", "生成书评，只展示即可", "你说的“生成书评”是什么意思？"]) {
    assert.equal(isReviewDraftCommand(text), false);
  }
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
