import type { ChatMessage } from "../types";

/** 用户消息原样重发；助手消息从其前一条用户消息重新生成。 */
export function prepareRetry(messages: ChatMessage[], index: number): { history: ChatMessage[]; text: string } | null {
  if (!Number.isInteger(index) || index < 0 || index >= messages.length) return null;
  let userIndex = index;
  if (messages[index].role === "assistant") {
    while (userIndex >= 0 && messages[userIndex].role !== "user") userIndex -= 1;
  }
  if (userIndex < 0 || messages[userIndex].role !== "user") return null;
  return { history: messages.slice(0, userIndex), text: messages[userIndex].content };
}

/** 仅确认开启的读后会话可直接保存对应书籍的书评。 */
export function canSaveReview(reviewBookId: number | undefined, bookId: number, status: string): boolean {
  return reviewBookId === bookId && status === "已读";
}
