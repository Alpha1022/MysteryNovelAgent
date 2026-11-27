import type { ChatStep } from "../types";

/** 书虫请求界面确认的书库操作。 */
export type PendingAction =
  | {
      kind: "meta";
      bookId: number;
      patch: {
        title?: string;
        author?: string;
        tags?: string;
        description?: string;
        seriesName?: string;
        seriesOrder?: number;
      };
    }
  | {
      kind: "status";
      bookId: number;
      status: "想读" | "在读" | "已读";
      review?: string;
      generateReview?: boolean;
    }
  | { kind: "merge"; ids: number[] };

function isPositiveInteger(value: unknown): value is number {
  return typeof value === "number" && Number.isSafeInteger(value) && value > 0;
}

function nonemptyString(value: unknown): string | undefined {
  return typeof value === "string" && value.trim() ? value : undefined;
}

function readAction(step: ChatStep): PendingAction | null {
  if (step.pending_confirmation !== true) return null;
  let parsed: unknown;
  try {
    parsed = JSON.parse(step.args);
  } catch {
    return null;
  }
  if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return null;
  const args = parsed as Record<string, unknown>;
  if (step.tool === "merge_books") {
    const ids = args.book_ids;
    if (!Array.isArray(ids) || ids.length < 2 || !ids.every(isPositiveInteger)) return null;
    if (new Set(ids).size !== ids.length) return null;
    return { kind: "merge", ids };
  }
  const bookId = args.book_id;
  if (!isPositiveInteger(bookId)) return null;
  if (step.tool === "update_book_meta") {
    const textFields = ["title", "author", "tags", "description", "series_name"];
    if (textFields.some((field) => args[field] != null && typeof args[field] !== "string")) return null;
    const seriesOrder = args.series_order ?? undefined;
    if (seriesOrder !== undefined && !isPositiveInteger(seriesOrder)) return null;
    return {
      kind: "meta",
      bookId,
      patch: {
        title: nonemptyString(args.title),
        author: nonemptyString(args.author),
        tags: nonemptyString(args.tags),
        description: nonemptyString(args.description),
        seriesName: typeof args.series_name === "string" ? args.series_name : undefined,
        seriesOrder,
      },
    };
  }
  if (step.tool !== "set_book_status") return null;
  const status = nonemptyString(args.status)?.trim();
  if (status !== "想读" && status !== "在读" && status !== "已读") return null;
  const review = nonemptyString(args.review);
  if (args.review != null && !review) return null;
  if (args.generate_review != null && typeof args.generate_review !== "boolean") return null;
  const generateReview = args.generate_review === true;
  if ((review || generateReview) && status !== "已读") return null;
  if (review && generateReview) return null;
  return { kind: "status", bookId, status, review, generateReview };
}

/** 从本次完成的工具轨迹中按调用次序提取已通过后端校验的写操作，并去重。 */
export function collectActions(steps: ChatStep[]): PendingAction[] {
  const actions: PendingAction[] = [];
  const seen = new Set<string>();
  for (const step of steps) {
    const action = readAction(step);
    if (!action) continue;
    const key = JSON.stringify(action);
    if (seen.has(key)) continue;
    seen.add(key);
    actions.push(action);
  }
  return actions;
}
