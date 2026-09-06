import { useEffect, useRef, useState } from "react";

const STATUSES = ["想读", "在读", "已读"] as const;

const badgeClass: Record<string, string> = {
  "已读": "read",
  "在读": "reading",
  "想读": "wish",
};

interface Props {
  status: string;
  onChange: (status: string) => void;
}

/** 可点击切换的阅读状态徽章（想读/在读/已读） */
export default function StatusBadge({ status, onChange }: Props) {
  const [open, setOpen] = useState(false);
  const wrapRef = useRef<HTMLSpanElement>(null);

  // 点击组件外部时收起菜单
  useEffect(() => {
    if (!open) return;
    const onDocClick = (e: MouseEvent) => {
      if (wrapRef.current && !wrapRef.current.contains(e.target as Node)) {
        setOpen(false);
      }
    };
    document.addEventListener("mousedown", onDocClick);
    return () => document.removeEventListener("mousedown", onDocClick);
  }, [open]);

  return (
    <span className="status-wrap" ref={wrapRef}>
      <span
        className={`badge ${badgeClass[status] ?? "wish"} clickable`}
        title="点击切换阅读状态"
        onClick={(e) => {
          e.stopPropagation();
          setOpen((o) => !o);
        }}
      >
        {status}
      </span>
      {open && (
        <span className="status-menu">
          {STATUSES.map((s) => (
            <button
              key={s}
              className={s === status ? "cur" : ""}
              onClick={(e) => {
                e.stopPropagation();
                setOpen(false);
                if (s !== status) onChange(s);
              }}
            >
              {s === status ? `${s} ✓` : s}
            </button>
          ))}
        </span>
      )}
    </span>
  );
}
