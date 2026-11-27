import type { ChatMessage, ChatReply } from "../types";
import type { PendingAction } from "./chatActions";

/** 标记已读后交给书虫讨论的书籍。 */
export interface ReviewTarget {
  id: number;
  title: string;
  author: string;
  tags: string;
}

export const REVIEW_CHAT_EVENT = "mna:review-chat";

/** 打开独立的书虫读后讨论会话。 */
export function startReviewChat(target: ReviewTarget) {
  window.dispatchEvent(new CustomEvent(REVIEW_CHAT_EVENT, { detail: target }));
}

/** 开启读后讨论。用户明确要求生成时才准备短评草稿。 */
export function buildReviewPrompt(target: ReviewTarget): string {
  return `我刚读完《${target.title}》（作者：${target.author || "佚名"}，本地 book_id：${target.id}，标签：${target.tags || "推理小说"}），已经标记为已读，无需再次修改阅读状态。
请和我聊聊这本书，通过具体问题引导我回顾感受、诡计、人物和结局，每次只问一个问题，先用简短的问题开场。需要资料时可以查询本地书籍详情和短评。鼓励你在讨论和生成书评前查阅豆瓣评价：先从本地书籍详情或来源资料确认对应的 douban_id，再按需调用 get_douban_comments、get_douban_reviews；需要深入了解某篇书评时使用 get_douban_review_full。不要把本地 book_id 当作 douban_id，也不要猜测 ID。将豆瓣评价作为追问和比较的参考，注明观点来源，以我在对话中表达的实际感受为主；不要照搬评价，不要把其他读者的看法当成我的观点，也不要编造情节。豆瓣不可访问或没有对应条目时，说明情况并继续基于我们的讨论生成书评，不必等待联网成功。
由我决定何时生成书评：只有我明确要求生成时，才根据我们的讨论以第一人称写一篇书评；不要主动生成，不要因我结束聊天就生成，也不要调用 generate_review 再次启动讨论。生成时调用 set_book_status，携带本地 book_id：${target.id}、status=已读 和完整的 review 正文，交给我编辑。我点击保存后才写入这本书。`;
}

/** 识别对话中独立的短评生成请求。带否定、引述或其他要求的消息照常对话。 */
export function isReviewDraftCommand(text: string): boolean {
  return /^(?:请)?(?:帮我|给我)?(?:生成|写)(?:一篇|一条|一段|个|篇)?(?:读后)?(?:短评|书评)[吧呀。！!\s]*$/u.test(text.trim());
}

/** 只处理本轮回复。已校验的工具正文优先；纯文本协议只创建待编辑的草稿。 */
export function prepareReviewReply(
  reply: ChatReply, actions: PendingAction[], reviewBookId: number | undefined, draftRequested: boolean,
): { message: ChatMessage; actions: PendingAction[] } {
  const steps = reply.steps ?? [];
  const message: ChatMessage = { role: "assistant", content: reply.content, steps };
  // 旧会话可能诱导模型复述原来的弹窗话术。这种独立的界面回执不属于短评正文。
  const popupReceipt = /(?:^|\n)\s*(?:已(?:经)?(?:在界面(?:上)?)?弹出(?:了)?(?:短评|书评|保存|确认|编辑)*(?:窗口|弹窗|对话框)|(?:短评|书评|保存|确认|编辑)+(?:窗口|弹窗|对话框)已(?:经)?弹出)[^\n]*$/u;
  const text = reply.content.trim().replace(popupReceipt, "").trim();
  const match = /^<book_review book_id="([1-9]\d*)">([\s\S]+)<\/book_review>$/u.exec(text);
  if (text !== reply.content.trim()) {
    message.content = text || "这条回复没有附上短评正文。";
  }
  const toolDraft = actions.find((a) => a.kind === "status" && a.review);
  if (toolDraft?.kind === "status" && toolDraft.review) {
    if (actions.length === 1 && (draftRequested || !text || match)) message.content = toolDraft.review;
    return { message, actions };
  }

  let draft: { bookId: number; review: string } | undefined;
  if (match) {
    const id = Number(match[1]);
    const body = match[2].trim();
    if (Number.isSafeInteger(id) && (reviewBookId === undefined || id === reviewBookId)
      && body && !/<\/?book_review\b/u.test(body)) {
      draft = { bookId: id, review: body };
    }
  } else if (draftRequested && reviewBookId !== undefined && text && !/<\/?book_review\b/u.test(text)
    && !steps.some((s) => s.tool === "set_book_status")) {
    // 这个请求显式指定了目标和纯正文输出，普通聊天文本不能走此分支。
    draft = { bookId: reviewBookId, review: text };
  }
  if (!draft) return { message, actions };
  message.content = draft.review;
  // 同一本书的无正文状态请求并入短评编辑框，避免随后重复确认或重新开启讨论。
  const other = actions.filter((a) => a.kind !== "status" || a.bookId !== draft.bookId || a.status !== "已读");
  return { message, actions: [...other, { kind: "status", bookId: draft.bookId, status: "已读", review: draft.review }] };
}
