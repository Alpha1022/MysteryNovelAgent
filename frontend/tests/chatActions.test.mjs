import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import test from "node:test";
import ts from "typescript";

const { outputText } = ts.transpileModule(
  readFileSync(new URL("../src/utils/chatActions.ts", import.meta.url), "utf8"),
  { compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ES2021 } },
);
const { collectActions } = await import(
  `data:text/javascript;base64,${Buffer.from(outputText).toString("base64")}`
);

const reviewArgs = { book_id: 452, status: "已读", review: "第一段。\n\n第二段。" };
const step = (tool, args) => ({
  tool, args: JSON.stringify(args), result: '{"status":"pending_confirmation"}',
  pending_confirmation: true,
});

test("短评只从本次后端认可的写工具中提取，保留完整正文", () => {
  const approved = step("set_book_status", reviewArgs);
  assert.deepEqual(collectActions([
    { ...approved, result: '{"error":"书籍不存在"}', pending_confirmation: false },
    { tool: approved.tool, args: approved.args, result: approved.result },
    { ...approved, tool: "get_book_detail" },
  ]), []);
  // result 是可能被截断的展示文本；标记由后端从未截断的实际工具结果生成。
  assert.deepEqual(collectActions([{ ...approved, result: '{"message":"很长的结果' }]), [
    { kind: "status", bookId: 452, status: "已读", review: reviewArgs.review, generateReview: false },
  ]);
  assert.equal(collectActions([step("set_book_status", { ...reviewArgs, generate_review: null })])[0].review, reviewArgs.review);
});

test("参数无效的短评不会退化成修改状态的请求", () => {
  const invalid = [
    { ...reviewArgs, book_id: "452" },
    { ...reviewArgs, book_id: 1.5 },
    { ...reviewArgs, status: "读完" },
    { ...reviewArgs, status: "想读" },
    { ...reviewArgs, review: 123 },
    { ...reviewArgs, review: "   " },
    { ...reviewArgs, generate_review: true },
    { ...reviewArgs, generate_review: "true" },
  ].map((args) => step("set_book_status", args));
  for (const args of ["{", "null", "[]"]) {
    invalid.push({ ...step("set_book_status", reviewArgs), args });
  }
  assert.deepEqual(collectActions(invalid), []);
});

test("普通状态、元数据和合并按首次请求的顺序确认，重复调用只入队一次", () => {
  const status = step("set_book_status", { book_id: 10, status: "在读" });
  const meta = step("update_book_meta", { book_id: 2, title: "新标题" });
  const merge = step("merge_books", { book_ids: [9, 7] });
  const actions = collectActions([meta, merge, meta, status, merge, status]);
  assert.deepEqual(actions.map(({ kind }) => kind), ["meta", "merge", "status"]);
  assert.equal(actions[0].patch.title, "新标题");
  assert.deepEqual(actions[1].ids, [9, 7]);
  assert.equal(actions[2].status, "在读");
  assert.deepEqual(collectActions([
    step("update_book_meta", { book_id: 2, title: [] }),
    step("merge_books", { book_ids: [2, 2] }),
    step("merge_books", { book_ids: [9, "8", 7] }),
  ]), []);
});
