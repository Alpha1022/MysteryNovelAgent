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

/** 自动发送的用户消息：讨论、生成和保存由用户分别决定。 */
export function buildReviewPrompt(target: ReviewTarget): string {
  return `我刚读完《${target.title}》（作者：${target.author || "佚名"}，本地 book_id：${target.id}，标签：${target.tags || "推理小说"}），已经标记为已读，无需再次修改阅读状态。
请和我聊聊这本书，通过具体问题引导我回顾感受、诡计、人物和结局，每次只问一个问题，先用简短的问题开场。需要资料时可以查询本地书籍详情和短评，但不要把其他读者的看法当成我的观点，也不要编造情节。
由我决定何时生成书评：只有我明确要求生成时，才根据我们的讨论以第一人称写一篇书评；不要主动生成，不要因我结束聊天就生成，也不要调用 generate_review 再次启动讨论。生成后先在对话中展示，只有我明确要求保存时才用 set_book_status 携带 book_id、status=已读 和 review 请求保存。`;
}
