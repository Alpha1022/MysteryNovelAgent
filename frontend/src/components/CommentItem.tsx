import { openUrl } from "@tauri-apps/plugin-opener";
import { FileImg } from "./fileSrc";
import type { CommentRow } from "../types";

/** 来源项目定位信息（悬停展示封面与书名） */
export interface CommentSourceInfo {
  kind: "clasp" | "douban";
  title: string;
  cover_path: string | null;
}

function Stars({ rating }: { rating: number | null }) {
  if (rating == null || rating < 1) return null;
  const filled = Math.min(5, rating);
  return (
    <span className="stars">
      {"★".repeat(filled)}
      <span className="dim">{"☆".repeat(Math.max(0, 5 - filled))}</span>
    </span>
  );
}

export default function CommentItem({
  c,
  sourceMap,
}: {
  c: CommentRow;
  sourceMap?: Map<string, CommentSourceInfo>;
}) {
  const mine = c.is_mine === 1;
  const info = c.source_ref ? sourceMap?.get(c.source_ref) : undefined;

  const link =
    info &&
    (info.kind === "clasp"
      ? `https://claspclub.com/books/${c.source_ref}/comments?sort=popular`
      : `${c.source_ref!.replace(/\/+$/, "")}/comments/`);

  return (
    <div className="comment">
      <div className="comment-head">
        <Stars rating={c.rating} />
        {mine ? (
          <span className="csrc-tag">AI助手</span>
        ) : info && link ? (
          <a
            className="csrc"
            href={link}
            title="打开来源评论页"
            onClick={(e) => {
              e.preventDefault();
              openUrl(link).catch(() => undefined);
            }}
          >
            <span className="csrc-tag">
              {info.kind === "clasp" ? "claspclub" : "豆瓣"}
            </span>
            <span className="csrc-pop">
              {info.cover_path ? (
                <FileImg path={info.cover_path} alt={info.title} />
              ) : null}
              <span className="csrc-title">{info.title}</span>
            </span>
          </a>
        ) : (
          <span className="csrc-tag">{c.source}</span>
        )}
      </div>
      <div className="comment-body">{c.content}</div>
    </div>
  );
}
