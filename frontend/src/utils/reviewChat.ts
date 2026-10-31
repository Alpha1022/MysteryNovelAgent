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

/** 自动发送的用户消息：用户决定生成时机，生成后保存到对应书籍。 */
export function buildReviewPrompt(target: ReviewTarget): string {
  return `我刚读完《${target.title}》（作者：${target.author || "佚名"}，本地 book_id：${target.id}，标签：${target.tags || "推理小说"}），已经标记为已读，无需再次修改阅读状态。
请和我聊聊这本书，通过具体问题引导我回顾感受、诡计、人物和结局，每次只问一个问题，先用简短的问题开场。需要资料时可以查询本地书籍详情和短评。鼓励你在讨论和生成书评前查阅豆瓣评价：先从本地书籍详情或来源资料确认对应的 douban_id，再按需调用 get_douban_comments、get_douban_reviews；需要深入了解某篇书评时使用 get_douban_review_full。不要把本地 book_id 当作 douban_id，也不要猜测 ID。将豆瓣评价作为追问和比较的参考，注明观点来源，以我在对话中表达的实际感受为主；不要照搬评价，不要把其他读者的看法当成我的观点，也不要编造情节。豆瓣不可访问或没有对应条目时，说明情况并继续基于我们的讨论生成书评，不必等待联网成功。
由我决定何时生成书评：只有我明确要求生成时，才根据我们的讨论以第一人称写一篇书评；不要主动生成，不要因我结束聊天就生成，也不要调用 generate_review 再次启动讨论。我已确认：当我明确要求生成书评时，也授权将生成的书评保存到这本书。届时请在对话中展示书评，并调用 set_book_status 携带本地 book_id：${target.id}、status=已读 和 review 保存书评，不要仅输出文字。`;
}
