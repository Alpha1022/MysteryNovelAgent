import { useEffect, useRef, useState } from "react";
import { openUrl } from "@tauri-apps/plugin-opener";
import { useConfirm } from "./ConfirmDialog";
import { FileImg } from "./fileSrc";
import {
  addClaspSources,
  addDoubanSources,
  clearSources,
  deleteSource,
  listSources,
  mergeSourceDescriptions,
  refreshSourceComments,
  refreshSourceMeta,
  resetBookSeries,
  resetBookTags,
  saveSourceOrder,
  searchClasp,
  setSourceAsCover,
  setSourceAsDescription,
} from "../api/tauri";
import type { ClaspMatch, SourceItem } from "../types";
import RemoteCover from "./RemoteCover";

interface Props {
  bookId: number;
  bookTitle: string;
  onClose: (changed: boolean) => void;
}

type Tab = "clasp" | "douban";

/** 来源管理弹窗：claspclub / 豆瓣两个选项卡，均可增删、清空、拖拽排序 */
export default function SourcesModal({ bookId, bookTitle, onClose }: Props) {
  const { confirm, confirmElement } = useConfirm();
  const [sources, setSources] = useState<SourceItem[]>([]);
  const [loading, setLoading] = useState(true);
  const [tab, setTab] = useState<Tab>("clasp");
  const [busyKey, setBusyKey] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);

  // 添加面板：clasp 内联搜索 / 粘贴链接；豆瓣粘贴链接
  const [panel, setPanel] = useState<"none" | "search" | "paste">("none");
  const [pasteText, setPasteText] = useState("");
  const [kw, setKw] = useState(bookTitle);
  const [results, setResults] = useState<ClaspMatch[]>([]);
  const [searching, setSearching] = useState(false);
  const [searchErr, setSearchErr] = useState<string | null>(null);
  const [fuzzy, setFuzzy] = useState(false);
  const [picked, setPicked] = useState<ClaspMatch[]>([]);

  // clasp 版本封面展开
  const [expanded, setExpanded] = useState<string | null>(null);

  // 拖拽排序（鼠标事件实现；dragFrom 用状态驱动高亮）
  const [dragFrom, setDragFrom] = useState<number | null>(null);
  const [dragOver, setDragOver] = useState<number | null>(null);
  const dragFromRef = useRef<number | null>(null);

  const changedRef = useRef(false);

  useEffect(() => {
    let cancelled = false;
    listSources(bookId)
      .then((list) => {
        if (!cancelled) setSources(list);
      })
      .catch((e) => {
        if (!cancelled) setError(String(e));
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [bookId]);

  const items = sources.filter((s) => s.kind === tab);
  const withSummary = sources.filter((s) => (s.summary ?? "").trim() !== "");

  const run = async (key: string, fn: () => Promise<void>) => {
    if (busyKey) return;
    setError(null);
    setNotice(null);
    setBusyKey(key);
    try {
      await fn();
    } catch (e) {
      setError(String(e));
    } finally {
      setBusyKey(null);
    }
  };

  // ---------- 添加 ----------

  const doSearch = async (targetPage = 1) => {
    const q = kw.trim();
    if (!q) return;
    setSearching(true);
    setSearchErr(null);
    setFuzzy(false);
    try {
      const resp = await searchClasp(q, targetPage);
      setResults(resp.items);
      setFuzzy(resp.fuzzy);
    } catch (e) {
      setSearchErr(String(e));
      setResults([]);
      setFuzzy(false);
    } finally {
      setSearching(false);
    }
  };

  const openSearch = () => {
    setPanel("search");
    setResults([]);
    setPicked([]);
    setSearchErr(null);
    setFuzzy(false);
    doSearch(1);
  };

  const togglePick = (m: ClaspMatch) => {
    setPicked((prev) => {
      const idx = prev.findIndex((x) => x.id === m.id);
      if (idx >= 0) return prev.filter((_, i) => i !== idx);
      return [...prev, m];
    });
  };

  const onAddClaspSearch = () =>
    run("add", async () => {
      const list = await addClaspSources(
        bookId,
        picked.map((m) => m.id),
      );
      setSources(list);
      setPanel("none");
      setPicked([]);
      changedRef.current = true;
      setNotice("claspclub 来源已添加（豆瓣来源已按顺序自动补充）");
    });

  const onAddPaste = () =>
    run("add", async () => {
      const lines = pasteText
        .split(/\r?\n/)
        .map((s) => s.trim())
        .filter(Boolean);
      if (lines.length === 0) {
        setError("请先粘贴链接");
        return;
      }
      const list =
        tab === "clasp"
          ? await addClaspSources(bookId, lines)
          : await addDoubanSources(bookId, lines);
      setSources(list);
      setPasteText("");
      setPanel("none");
      changedRef.current = true;
      setNotice("来源已添加");
    });

  // ---------- 项目操作 ----------

  const onDelete = (s: SourceItem) =>
    run(`del-${s.ref_key}`, async () => {
      const list = await deleteSource(bookId, s.kind, s.ref_key);
      setSources(list);
      changedRef.current = true;
    });

  const onClear = () =>
    run("clear", async () => {
      const ok = await confirm({
        title: "清空确认",
        message: `确定清空全部 ${tab === "clasp" ? "claspclub" : "豆瓣"} 来源项目吗？\n\n对应抓取的短评将一并删除，封面按引用计数清理。`,
        okLabel: "清空",
        danger: true,
      });
      if (!ok) return;
      const list = await clearSources(bookId, tab);
      setSources(list);
      changedRef.current = true;
    });

  const onRefreshMeta = (s: SourceItem) =>
    run(`rf-${s.ref_key}`, async () => {
      await refreshSourceMeta(bookId, s.kind, s.ref_key);
      const list = await listSources(bookId);
      setSources(list);
      changedRef.current = true;
      setNotice("来源数据已更新");
    });

  const onRefreshComments = (s: SourceItem) =>
    run(`rc-${s.ref_key}`, async () => {
      const n = await refreshSourceComments(bookId, s.kind, s.ref_key);
      changedRef.current = true;
      setNotice(`已重抓 ${n} 条短评`);
    });

  const onSetCover = (s: SourceItem, editionPath?: string) =>
    run(`cv-${s.ref_key}-${editionPath ?? ""}`, async () => {
      const res = await setSourceAsCover(bookId, s.kind, s.ref_key, editionPath);
      changedRef.current = true;
      setExpanded(null);
      setNotice(`封面已更新：${res.cover_path.split(/[\\/]/).pop() ?? ""}`);
    });

  const onSetDescription = (s: SourceItem) =>
    run(`ds-${s.ref_key}`, async () => {
      const desc = await setSourceAsDescription(bookId, s.kind, s.ref_key);
      changedRef.current = true;
      setNotice("简介已更新");
      void desc;
    });

  const onResetTags = () =>
    run("reset-tags", async () => {
      const tags = await resetBookTags(bookId);
      changedRef.current = true;
      setNotice(`标签已重置：${tags.join("、")}`);
    });

  const onResetSeries = () =>
    run("reset-series", async () => {
      const [name, order] = await resetBookSeries(bookId);
      changedRef.current = true;
      setNotice(`系列已重置：${name}${order != null ? ` #${order}` : ""}`);
    });

  const onMergeDescriptions = () =>
    run("merge-desc", async () => {
      const res = await mergeSourceDescriptions(bookId);
      changedRef.current = true;
      setNotice(
        `简介已合并${res.model ? `（${res.model}）` : ""}${res.degraded ? ` · ${res.degraded}` : ""}`,
      );
    });

  // 简介悬停浮窗（fixed 定位、渲染在弹窗外部，避免被滚动容器裁剪/影响排版）
  const [hoverTip, setHoverTip] = useState<{ text: string; top: number; left: number } | null>(null);
  const POP_W = 320;
  const POP_H = 240;
  const showHoverTip = (text: string, el: HTMLElement) => {
    const r = el.getBoundingClientRect();
    // 垂直：下方空间足够则贴按钮下方，否则弹到上方
    const below = window.innerHeight - r.bottom > POP_H + 16;
    const top = below
      ? r.bottom + 6
      : Math.max(8, r.top - POP_H - 6);
    // 水平：优先右侧贴齐，空间不足则放到左侧
    let left = r.right + 8;
    if (left + POP_W > window.innerWidth - 8) {
      left = r.left - POP_W - 8;
    }
    left = Math.max(8, Math.min(left, window.innerWidth - POP_W - 8));
    setHoverTip({ text, top, left });
  };

  /** 在拖拽手柄上按下：跟踪指针经过的行，松开时落位 */
  const startDrag = (i: number, e: React.MouseEvent) => {
    if (busyKey != null) return;
    e.preventDefault();
    dragFromRef.current = i;
    setDragFrom(i);
    setDragOver(i);

    const idxAt = (x: number, y: number): number | null => {
      const el = document
        .elementsFromPoint(x, y)
        .find((n) => (n as HTMLElement).dataset?.idx !== undefined) as
        | HTMLElement
        | undefined;
      if (!el) return null;
      const idx = Number(el.dataset.idx);
      return Number.isNaN(idx) ? null : idx;
    };

    const onMove = (ev: MouseEvent) => {
      const idx = idxAt(ev.clientX, ev.clientY);
      if (idx != null) setDragOver(idx);
    };
    const onUp = (ev: MouseEvent) => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
      const from = dragFromRef.current;
      const to = idxAt(ev.clientX, ev.clientY);
      dragFromRef.current = null;
      setDragFrom(null);
      setDragOver(null);
      if (from != null && to != null) reorder(from, to);
    };
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
  };

  const reorder = (from: number, to: number) => {
    if (from === to || Number.isNaN(from) || Number.isNaN(to)) return;
    const reordered = [...items];
    const [moved] = reordered.splice(from, 1);
    reordered.splice(to, 0, moved);
    const others = sources.filter((s) => s.kind !== tab);
    setSources([...reordered, ...others]);
    changedRef.current = true;
    saveSourceOrder(bookId, tab, reordered.map((s) => s.ref_key)).catch((e) =>
      setError(String(e)),
    );
  };

  const tabLabel = tab === "clasp" ? "claspclub" : "豆瓣";

  return (
    <>
      <div className="modal-overlay">
        <div className="modal sources-modal">
        <h2 className="modal-title">来 源 管 理</h2>
        <div className="modal-file">《{bookTitle}》</div>

        <div className="src-tabs">
          <button
            className={`tab ${tab === "clasp" ? "active" : ""}`}
            onClick={() => setTab("clasp")}
          >
            claspclub {sources.filter((s) => s.kind === "clasp").length}
          </button>
          <button
            className={`tab ${tab === "douban" ? "active" : ""}`}
            onClick={() => setTab("douban")}
          >
            豆瓣 {sources.filter((s) => s.kind === "douban").length}
          </button>
        </div>

        <div className="src-toolbar">
          {tab === "clasp" && (
            <>
              <button
                className="btn small"
                onClick={openSearch}
                disabled={busyKey != null}
              >
                搜索添加
              </button>
              <button
                className="btn small"
                onClick={() => setPanel(panel === "paste" ? "none" : "paste")}
                disabled={busyKey != null}
              >
                粘贴链接
              </button>
            </>
          )}
          {tab === "douban" && (
            <button
              className="btn small"
              onClick={() => setPanel(panel === "paste" ? "none" : "paste")}
              disabled={busyKey != null}
            >
              添加链接
            </button>
          )}
          <span className="src-toolbar-hint">拖拽项目可排序</span>
          <span className="src-flex" />
          <button
            className="btn small"
            onClick={onResetSeries}
            disabled={busyKey != null || sources.filter((s) => s.kind === "clasp").length === 0}
            title="按 claspclub 来源的系列信息重设本书系列（全部来源系列一致时才可采用）"
          >
            {busyKey === "reset-series" ? "重设中…" : "重设系列"}
          </button>
          <button
            className="btn small"
            onClick={onResetTags}
            disabled={busyKey != null || sources.filter((s) => s.kind === "clasp").length === 0}
            title="按 claspclub 来源的标签并集重设本书标签（豆瓣来源不含标签）"
          >
            {busyKey === "reset-tags" ? "重设中…" : "重设标签"}
          </button>
          <button
            className="btn small"
            onClick={onMergeDescriptions}
            disabled={busyKey != null || withSummary.length < 2}
            title="将所有来源项目的简介经 LLM 合并为一段简体中文简介"
          >
            {busyKey === "merge-desc" ? "合并中…" : "生成合并简介"}
          </button>
          <button
            className="btn small danger-btn"
            onClick={onClear}
            disabled={busyKey != null || items.length === 0}
          >
            清空
          </button>
        </div>

        {/* clasp 内联搜索面板 */}
        {panel === "search" && tab === "clasp" && (
          <div className="src-add-panel">
            <div className="search-row">
              <input
                value={kw}
                placeholder="书名关键词…"
                onChange={(e) => setKw(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") doSearch(1);
                }}
              />
              <button className="btn" onClick={() => doSearch(1)} disabled={searching}>
                {searching ? "搜索中…" : "搜 索"}
              </button>
            </div>
            {searchErr && <div className="modal-error">{searchErr}</div>}
            {!searchErr && fuzzy && results.length > 0 && (
              <div className="modal-warn">未找到精确匹配，正在展示相近结果</div>
            )}
            <div className="result-grid compact">
              {results.map((m, i) => {
                const order = picked.findIndex((x) => x.id === m.id);
                const sel = order >= 0;
                return (
                  <div
                    key={m.id || i}
                    className={`result-item ${sel ? "sel" : ""}`}
                    onClick={() => togglePick(m)}
                  >
                    {sel && <span className="order-badge">{order + 1}</span>}
                    {m.cover_url ? (
                      <RemoteCover url={m.cover_url} title={m.title} className="result-cover" />
                    ) : (
                      <div className="result-cover placeholder">{m.title}</div>
                    )}
                    <div className="result-title">{m.title}</div>
                  </div>
                );
              })}
            </div>
            <div className="modal-actions">
              <span className="modal-count">已选 {picked.length} 项（有序）</span>
              <button className="btn" onClick={() => setPanel("none")}>
                收起
              </button>
              <button
                className="btn primary"
                onClick={onAddClaspSearch}
                disabled={busyKey != null || picked.length === 0}
              >
                {busyKey === "add" ? "添加中…" : "添加所选"}
              </button>
            </div>
          </div>
        )}

        {/* 粘贴链接面板 */}
        {panel === "paste" && (
          <div className="src-add-panel">
            <label className="modal-label">
              {tab === "clasp"
                ? "claspclub 页面链接或条目 ID（每行一条）"
                : "豆瓣书籍页链接（每行一条）"}
              <textarea
                value={pasteText}
                rows={4}
                placeholder={
                  tab === "clasp"
                    ? "https://claspclub.com/books/xxxx"
                    : "https://book.douban.com/subject/26771719/"
                }
                onChange={(e) => setPasteText(e.target.value)}
              />
            </label>
            <div className="modal-actions">
              <button className="btn" onClick={() => setPanel("none")}>
                收起
              </button>
              <button
                className="btn primary"
                onClick={onAddPaste}
                disabled={busyKey != null || !pasteText.trim()}
              >
                {busyKey === "add" ? "添加中…" : "添 加"}
              </button>
            </div>
          </div>
        )}

        {loading ? (
          <div className="search-empty">加载中…</div>
        ) : items.length === 0 ? (
          <div className="search-empty">暂无{tabLabel}来源项目</div>
        ) : (
          <div className="src-list">
            {items.map((s, i) => (
              <div
                key={s.ref_key}
                data-idx={i}
                className={`src-item ${dragFrom === i ? "dragging" : ""} ${dragOver === i && dragFrom !== null && dragFrom !== i ? "over" : ""}`}
              >
                <span
                  className="drag-handle"
                  title="拖拽排序"
                  onMouseDown={(e) => startDrag(i, e)}
                >
                  ⠿
                </span>
                <span className="src-order">{i + 1}</span>
                {s.cover_path ? (
                  <FileImg
                    className="src-cover"
                    path={s.cover_path}
                    alt={s.title ?? ""}
                    lazy
                  />
                ) : s.cover_url ? (
                  <RemoteCover url={s.cover_url} title={s.title ?? ""} className="src-cover" />
                ) : (
                  <div className="src-cover placeholder">无封面</div>
                )}
                <div className="src-info">
                  <div className="src-title">{s.title || s.ref_key}</div>
                  {s.author && <div className="src-author">{s.author}</div>}
                  {s.kind === "clasp" && s.series_name && (
                    <div className="src-series">
                      系列：{s.series_name}
                      {s.series_order != null ? ` #${s.series_order}` : ""}
                    </div>
                  )}
                  <button
                    className="link-btn small-link"
                    onClick={() =>
                      openUrl(
                        s.kind === "clasp"
                          ? `https://claspclub.com/books/${s.ref_key}`
                          : s.ref_key,
                      ).catch(() => undefined)
                    }
                  >
                    {s.kind === "clasp" ? "在 claspclub 查看 ↗" : "在豆瓣查看 ↗"}
                  </button>
                </div>
                <div className="src-actions">
                  <button
                    className="link-btn small-link"
                    disabled={busyKey != null}
                    onClick={() =>
                      s.kind === "clasp" && s.editions.length > 0
                        ? setExpanded(expanded === s.ref_key ? null : s.ref_key)
                        : onSetCover(s)
                    }
                  >
                    设为封面
                  </button>
                  <button
                    className="link-btn small-link"
                    disabled={busyKey != null || !s.summary}
                    onMouseEnter={(e) => {
                      if (s.summary) showHoverTip(s.summary, e.currentTarget);
                    }}
                    onMouseLeave={() => setHoverTip(null)}
                    onClick={() => onSetDescription(s)}
                  >
                    设为简介
                  </button>
                  <button
                    className="link-btn small-link"
                    disabled={busyKey != null}
                    onClick={() => onRefreshMeta(s)}
                  >
                    {busyKey === `rf-${s.ref_key}` ? "更新中…" : "更新数据"}
                  </button>
                  <button
                    className="link-btn small-link"
                    disabled={busyKey != null}
                    onClick={() => onRefreshComments(s)}
                  >
                    {busyKey === `rc-${s.ref_key}` ? "重抓中…" : "重抓短评"}
                  </button>
                  <button
                    className="link-btn small-link danger"
                    disabled={busyKey != null}
                    onClick={() => onDelete(s)}
                  >
                    删除
                  </button>
                </div>

                {/* clasp 版本封面选择（预爬本地文件） */}
                {expanded === s.ref_key && s.editions.length > 0 && (
                  <div className="edition-grid">
                    {s.editions.map((ed) => (
                      <div
                        key={ed.url}
                        className={`cover-item ${busyKey === `cv-${s.ref_key}-${ed.url}` ? "busy" : ""}`}
                        onClick={() => onSetCover(s, ed.url)}
                      >
                        <RemoteCover url={ed.url} title={s.title ?? ""} className="cover-img" />
                        <div className="cover-label">{ed.label}</div>
                      </div>
                    ))}
                  </div>
                )}
              </div>
            ))}
          </div>
        )}

        {error && <div className="modal-error">{error}</div>}
        {notice && <div className="src-notice">{notice}</div>}

        <div className="modal-actions">
          <button className="btn" onClick={() => onClose(changedRef.current)}>
            关 闭
          </button>
        </div>
        {confirmElement}
        </div>
      </div>
      {/* 简介悬浮窗渲染在弹窗外部（fixed 定位），不影响弹窗排版且始终完整显示 */}
      {hoverTip && (
        <div className="summary-pop" style={{ top: hoverTip.top, left: hoverTip.left }}>
          {hoverTip.text}
        </div>
      )}
    </>
  );
}
