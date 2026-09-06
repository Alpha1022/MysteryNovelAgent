import { useState } from "react";

export interface FilterGroup {
  key: string;
  label: string;
  options: string[];
}

interface Props {
  groups: FilterGroup[];
  /** 每个类别已选中的选项（空数组 = 全选） */
  selected: Record<string, string[]>;
  onToggle: (key: string, option: string) => void;
  onClearGroup: (key: string) => void;
}

/**
 * 书架筛选栏（工具栏下一行）：阅读状态 / 作者 / 标签，
 * 选项为 chip，选中以颜色变化表示（无勾选框）；每组默认折叠可展开。
 */
export default function FilterSidebar({ groups, selected, onToggle, onClearGroup }: Props) {
  // 每组默认折叠
  const [collapsed, setCollapsed] = useState<Record<string, boolean>>(() =>
    Object.fromEntries(groups.map((g) => [g.key, true])),
  );

  const toggleCollapse = (key: string) =>
    setCollapsed((prev) => ({ ...prev, [key]: !prev[key] }));

  return (
    <div className="filter-bar">
      {groups.map((g) => {
        const sel = selected[g.key] ?? [];
        const isCollapsed = collapsed[g.key] ?? true;
        return (
          <div key={g.key} className="filter-group">
            <button
              className="filter-group-head"
              onClick={() => toggleCollapse(g.key)}
              title={isCollapsed ? "展开筛选" : "折叠"}
            >
              <span className="filter-group-label">{g.label}</span>
              <span className="filter-arrow">{isCollapsed ? "▾" : "▴"}</span>
              {sel.length > 0 && <span className="filter-count">{sel.length}</span>}
            </button>

            {/* 折叠时仍展示已选中的项（可点击取消） */}
            {isCollapsed && sel.length > 0 && (
              <div className="filter-chips">
                {sel.map((opt) => (
                  <button
                    key={opt}
                    className="filter-chip on"
                    onClick={() => onToggle(g.key, opt)}
                    title={opt}
                  >
                    {opt}
                  </button>
                ))}
              </div>
            )}

            {!isCollapsed && (
              <div className="filter-chips">
                {g.options.map((opt) => {
                  const on = sel.includes(opt);
                  return (
                    <button
                      key={opt}
                      className={`filter-chip ${on ? "on" : ""}`}
                      onClick={() => onToggle(g.key, opt)}
                      title={opt}
                    >
                      {opt}
                    </button>
                  );
                })}
                {sel.length > 0 && (
                  <button
                    className="filter-chip clear"
                    onClick={() => onClearGroup(g.key)}
                    title="清除本组筛选"
                  >
                    清除
                  </button>
                )}
              </div>
            )}
          </div>
        );
      })}
    </div>
  );
}
