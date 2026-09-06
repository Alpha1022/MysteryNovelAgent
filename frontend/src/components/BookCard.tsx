import { useNavigate } from "react-router-dom";
import type { BookCard } from "../types";
import CoverImage from "./CoverImage";
import StatusBadge from "./StatusBadge";

const badgeClass: Record<string, string> = {
  "已读": "read",
  "在读": "reading",
  "想读": "wish",
};

export default function BookCardItem({
  book,
  index,
  onStatusChange,
  selectMode,
  selected,
  orderIndex,
  onToggle,
}: {
  book: BookCard;
  index: number;
  onStatusChange?: (id: number, status: string, title: string) => void;
  /** 多选模式：点击卡片切换选中而非跳转详情 */
  selectMode?: boolean;
  selected?: boolean;
  /** 合并模式下的顺序号（0 起；非 null 时显示序号） */
  orderIndex?: number | null;
  onToggle?: (id: number) => void;
}) {
  const navigate = useNavigate();
  const status = book.status || "想读";
  const picked = selectMode && (selected || orderIndex != null);

  const activate = () => {
    if (selectMode) {
      onToggle?.(book.id);
    } else {
      navigate(`/book/${book.id}`);
    }
  };

  return (
    <div
      className={`card ${selectMode ? "selectable" : ""} ${picked ? "picked" : ""}`}
      role="button"
      tabIndex={0}
      style={{ animationDelay: `${Math.min(index * 45, 600)}ms` }}
      onClick={activate}
      onKeyDown={(e) => {
        if (e.key === "Enter" || e.key === " ") activate();
      }}
    >
      {selectMode ? (
        // 多选模式下徽章不可点，避免与勾选冲突
        <span className={`badge ${badgeClass[status] ?? "wish"}`}>{status}</span>
      ) : onStatusChange ? (
        <StatusBadge
          status={status}
          onChange={(s) => onStatusChange(book.id, s, book.title)}
        />
      ) : (
        <span className={`badge ${badgeClass[status] ?? "wish"}`}>{status}</span>
      )}
      {selectMode && (
        <span className="pick-mark">{orderIndex != null ? orderIndex + 1 : "✓"}</span>
      )}
      <CoverImage coverPath={book.cover_path} title={book.title} />
      <div className="meta">
        <div className="t">{book.title}</div>
        <div className="a">{book.author}</div>
        {book.series_name && (
          <div className="series">
            {book.series_name}
            {book.series_order != null ? ` #${book.series_order}` : ""}
          </div>
        )}
      </div>
    </div>
  );
}
