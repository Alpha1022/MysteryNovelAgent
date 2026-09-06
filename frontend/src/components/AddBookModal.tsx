import { useEffect, useRef, useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import {
  cancelTask,
  commitImport,
  discardImport,
  prepareImport,
  searchClasp,
  setPendingCover,
  translateText,
} from "../api/tauri";
import type { ClaspMatch, EpubPreview, ImportResult, PreparedPreview } from "../types";
import RemoteCover from "./RemoteCover";
import EpubCover from "./EpubCover";
import { FileImg } from "./fileSrc";
import { useConfirm } from "./ConfirmDialog";

/** 确保包含当前书库的固定标签（默认"推理小说"；数据不变量） */
export function ensureMysteryTag(tags: string[], defaults: string[] = ["推理小说"]): string[] {
  const list = tags.map((t) => t.trim()).filter(Boolean);
  for (const d of defaults) {
    const t = d.trim();
    if (t && !list.includes(t)) list.push(t);
  }
  return list;
}

interface Props {
  preview: EpubPreview;
  /** LLM 可用性（null = 未检测）；多条简介合并确认与翻译按钮用 */
  llmReady?: boolean | null;
  /** 当前书库的固定标签 */
  defaultTags?: string[];
  /** 长任务实时进度（task-progress 事件推送） */
  progressText?: string | null;
  /** 进度百分比（total=0 的阶段无进度条） */
  progressPct?: number | null;
  /** 导入完成（res=null 表示未导入关闭弹窗） */
  onFinished: (res: ImportResult | null) => void;
}

/**
 * 加书弹窗：搜索（claspclub）→ 豆瓣链接（未匹配时）→ 确认导入
 *
 * 进入确认步前先完成全部爬取与简介合并（多条简介且 LLM 可用时弹窗
 * 确认合并还是取第一本）；确认步可编辑元数据、上传封面、逐字段翻译。
 */
export default function AddBookModal({
  preview,
  llmReady,
  defaultTags = ["推理小说"],
  progressText,
  progressPct,
  onFinished,
}: Props) {
  const { confirm, confirmElement } = useConfirm();
  const [step, setStep] = useState<"search" | "douban" | "confirm">(
    preview.matched ? "search" : "search",
  );
  const [keyword, setKeyword] = useState(preview.title);
  const [results, setResults] = useState<ClaspMatch[]>([]);
  const [searching, setSearching] = useState(false);
  const [searchErr, setSearchErr] = useState<string | null>(null);
  const [fuzzy, setFuzzy] = useState(false);
  const [page, setPage] = useState(1);
  const [totalPages, setTotalPages] = useState(1);
  const [selected, setSelected] = useState<ClaspMatch[]>(
    preview.matched ? [preview.matched] : [],
  );

  // 豆瓣链接步
  const [doubanText, setDoubanText] = useState("");
  const [doubanErr, setDoubanErr] = useState<string | null>(null);

  // 确认步
  const [prepared, setPrepared] = useState<PreparedPreview | null>(null);
  const [preparing, setPreparing] = useState(false);
  const [committing, setCommitting] = useState(false);
  const [title, setTitle] = useState(preview.title);
  const [author, setAuthor] = useState(preview.author);
  const [tagsStr, setTagsStr] = useState("");
  const [desc, setDesc] = useState("");
  const [coverOverridePath, setCoverOverridePath] = useState<string | null>(null);
  const [seriesName, setSeriesName] = useState("");
  const [seriesOrder, setSeriesOrder] = useState<string>("");
  const [translating, setTranslating] = useState<string | null>(null);

  const [error, setError] = useState<string | null>(null);
  const taskIdRef = useRef<string | null>(null);
  const seqRef = useRef(0);

  const searchedRef = useRef(false);

  // 进入搜索步自动执行一次初始搜索
  useEffect(() => {
    if (!searchedRef.current) {
      searchedRef.current = true;
      doSearch(preview.title, 1);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const fileName = preview.path.split(/[\\/]/).pop() ?? preview.path;
  const busy = preparing || committing;

  const doSearch = async (kw: string, targetPage: number) => {
    const q = kw.trim();
    if (!q) return;
    setSearching(true);
    setSearchErr(null);
    setFuzzy(false);
    try {
      const resp = await searchClasp(q, targetPage);
      setResults(resp.items);
      setTotalPages(resp.total_pages);
      setPage(resp.page);
      // searchMode=fuzzy：无精确匹配，平台在返回相近结果（视为未搜到）
      setFuzzy(resp.fuzzy);
    } catch (e) {
      setSearchErr(String(e));
      setResults([]);
      setTotalPages(1);
      setPage(1);
      setFuzzy(false);
    } finally {
      setSearching(false);
    }
  };

  /** 点击结果切换选中状态（保持选择顺序） */
  const toggle = (m: ClaspMatch) => {
    setSelected((prev) => {
      const idx = prev.findIndex((x) => x.id === m.id);
      if (idx >= 0) return prev.filter((_, i) => i !== idx);
      return [...prev, m];
    });
  };

  /** 搜索步预填：合并多选标签与作者 */
  const prefillFromSelection = () => {
    const first = selected[0];
    const tagUnion: string[] = [];
    for (const m of selected) {
      for (const t of m.tags) {
        if (!tagUnion.includes(t)) tagUnion.push(t);
      }
    }
    const authors: string[] = [];
    for (const m of selected) {
      const a = m.author.trim();
      if (a && !authors.includes(a)) authors.push(a);
    }
    return {
      title: preview.title.trim() || first?.title || preview.title.trim(),
      author: preview.author.trim() || authors.join("、"),
      tags: first ? ensureMysteryTag(tagUnion, defaultTags) : [],
    };
  };

  /** 预处理 + 进入确认步（多条简介时弹窗确认合并/取第一本，逻辑与合并模式相同） */
  const startPrepare = async (t: string, a: string, tg: string[]) => {
    let merge = false;
    if (selected.length + doubanLinks.length > 1 && llmReady) {
      merge = await confirm({
        title: "简介合并",
        message:
          "检测到多个来源简介。\n\n是否调用 LLM 将它们合并为一段简介？\n选择「取第一本」则仅保留第一本的简介。",
        okLabel: "合并简介",
        cancelLabel: "取第一本",
      });
    }
    const taskId = `prep-${Date.now()}-${++seqRef.current}`;
    taskIdRef.current = taskId;
    setPreparing(true);
    setError(null);
    try {
      const res = await prepareImport(
        preview.effective_path ?? preview.path,
        preview.path,
        t,
        a,
        tg,
        selected,
        doubanLinks,
        merge,
        taskId,
      );
      setPrepared(res);
      // 后端已按 claspclub → 豆瓣 → EPUB 顺序回填元数据
      setTitle(res.title);
      setAuthor(res.author);
      setTagsStr(res.tags.join(", "));
      setDesc(res.description);
      setSeriesName(res.series_name ?? "");
      setSeriesOrder(res.series_order != null ? String(res.series_order) : "");
      setCoverOverridePath(null);
      setStep("confirm");
    } catch (e) {
      if (String(e) === "任务已取消") {
        setStep(selected.length > 0 ? "search" : "douban");
      } else {
        setError(String(e));
      }
    } finally {
      setPreparing(false);
    }
  };

  /** 搜索步 → 下一步：已匹配 claspclub 直接预处理；未匹配则进入豆瓣链接步 */
  const toNext = () => {
    const p = prefillFromSelection();
    if (selected.length > 0) {
      void startPrepare(p.title, p.author, p.tags);
    } else {
      setStep("douban");
    }
  };

  /** 跳过 claspclub 搜索，进入豆瓣链接步 */
  const skipSearch = () => {
    setSelected([]);
    setStep("douban");
  };

  /** 豆瓣链接步 → 预处理（校验链接格式） */
  const doubanToConfirm = () => {
    const links = parseDoubanLinks(doubanText);
    if (links.some((u) => !u.includes("book.douban.com/subject/"))) {
      setDoubanErr("存在无效链接（须为 book.douban.com/subject/ 页面）");
      return;
    }
    setDoubanErr(null);
    void startPrepare(preview.title.trim(), preview.author, []);
  };

  /** 确认步 → 重新搜索（丢弃当前预处理会话） */
  const backToSearch = () => {
    if (prepared) discardImport(prepared.task_id).catch(() => undefined);
    taskIdRef.current = null;
    setPrepared(null);
    setStep("search");
  };

  /** 上传封面（提交时落盘缓存并采用） */
  const onUploadCover = async () => {
    if (!prepared) return;
    setError(null);
    const path = await open({
      multiple: false,
      title: "选择封面图片",
      filters: [{ name: "图片", extensions: ["jpg", "jpeg", "png", "webp"] }],
    }).catch(() => null);
    if (typeof path !== "string") return;
    try {
      await setPendingCover(prepared.task_id, path);
      setCoverOverridePath(path);
    } catch (e) {
      setError(String(e));
    }
  };

  /** 翻译指定字段为中文（LLM） */
  const translateField = async (field: "title" | "author" | "desc") => {
    if (translating || !llmReady) return;
    const cur = field === "title" ? title : field === "author" ? author : desc;
    if (!cur.trim()) return;
    setTranslating(field);
    setError(null);
    try {
      const out = await translateText(cur);
      if (field === "title") setTitle(out);
      else if (field === "author") setAuthor(out);
      else setDesc(out);
    } catch (e) {
      setError(String(e));
    } finally {
      setTranslating(null);
    }
  };

  /** 提交导入 */
  const onImport = async () => {
    if (!prepared || busy) return;
    setCommitting(true);
    setError(null);
    try {
      const res = await commitImport(
        prepared.task_id,
        title,
        author,
        ensureMysteryTag(tagsStr.split(/[,，]/), defaultTags),
        desc.trim() ? desc : null,
        seriesName.trim() ? seriesName : null,
        seriesName.trim() && seriesOrder.trim() ? Number(seriesOrder) : null,
      );
      taskIdRef.current = null;
      onFinished(res);
    } catch (e) {
      setError(String(e));
    } finally {
      setCommitting(false);
    }
  };

  /** 关闭弹窗（取消预处理任务并丢弃会话） */
  const close = () => {
    if (preparing && taskIdRef.current) {
      cancelTask(taskIdRef.current).catch(() => undefined);
    }
    if (taskIdRef.current) {
      discardImport(taskIdRef.current).catch(() => undefined);
    }
    onFinished(null);
  };

  const doubanLinks = parseDoubanLinks(doubanText);
  const isMerged = selected.length > 1;

  /** 字段翻译按钮（配置了 LLM 且内容非空时显示） */
  const TranslateBtn = ({ field }: { field: "title" | "author" | "desc" }) => {
    if (!llmReady) return null;
    const cur = field === "title" ? title : field === "author" ? author : desc;
    if (!cur.trim()) return null;
    return (
      <button
        className="link-btn small-link"
        onClick={() => translateField(field)}
        disabled={busy || translating !== null}
      >
        {translating === field ? "翻译中…" : "翻译为中文"}
      </button>
    );
  };

  // 不启用"点击外部关闭"：拖拽选择文本时鼠标释放到蒙层不会误关弹窗
  return (
    <div className="modal-overlay">
      <div
        className={`modal ${
          step === "search" ? "wide" : step === "confirm" ? "mid" : ""
        }`}
      >
        {step === "search" && (
          <>
            <h2 className="modal-title">搜 索 匹 配</h2>
            <div className="modal-file">{fileName}</div>

            <div className="search-row">
              <input
                value={keyword}
                placeholder="书名关键词…"
                onChange={(e) => setKeyword(e.target.value)}
                onKeyDown={(e) => {
                  if (e.key === "Enter") doSearch(keyword, 1);
                }}
                disabled={busy}
                autoFocus
              />
              <button
                className="btn"
                onClick={() => doSearch(keyword, 1)}
                disabled={busy || searching || !keyword.trim()}
              >
                {searching ? "搜索中…" : "搜 索"}
              </button>
            </div>

            <div className="search-hint">
              点选匹配结果（可多选 = 合并本，封面取第一本）
            </div>

            {searchErr && <div className="modal-error">{searchErr}</div>}
            {!searchErr && fuzzy && results.length > 0 && (
              <div className="modal-warn">未找到精确匹配，正在展示相近结果</div>
            )}

            {results.length === 0 && !searching && !searchErr && (
              <div className="search-empty">无搜索结果，可修改关键词重试</div>
            )}

            <div className="result-grid">
              {results.map((m, i) => {
                const order = selected.findIndex((x) => x.id === m.id);
                const sel = order >= 0;
                return (
                  <div
                    key={m.id || i}
                    className={`result-item ${sel ? "sel" : ""}`}
                    onClick={() => !busy && toggle(m)}
                  >
                    {sel && (
                      <span className="order-badge">{order + 1}</span>
                    )}
                    {m.cover_url ? (
                      <RemoteCover
                        url={m.cover_url}
                        title={m.title}
                        className="result-cover"
                      />
                    ) : (
                      <div className="result-cover placeholder">{m.title}</div>
                    )}
                    <div className="result-title">{m.title}</div>
                    <div className="result-author">{m.author}</div>
                  </div>
                );
              })}
            </div>

            {preparing && (
              <div className="busy-row">
                <div className="busy-text">{progressText ?? "预处理中…"}</div>
                {progressPct != null && (
                  <div className="progress-bar">
                    <div className="progress-fill" style={{ width: `${progressPct}%` }} />
                  </div>
                )}
              </div>
            )}
            {error && <div className="modal-error">{error}</div>}

            <div className="modal-actions">
              <span className="modal-count">
                已选 {selected.length} 项
                {totalPages > 1 && (
                  <span className="pager">
                    <button
                      className="page-btn"
                      disabled={busy || searching || page <= 1}
                      onClick={() => doSearch(keyword, page - 1)}
                    >
                      ‹ 上一页
                    </button>
                    <span className="page-info">
                      {page} / {totalPages}
                    </span>
                    <button
                      className="page-btn"
                      disabled={busy || searching || page >= totalPages}
                      onClick={() => doSearch(keyword, page + 1)}
                    >
                      下一页 ›
                    </button>
                  </span>
                )}
              </span>
              <button
                className="btn"
                onClick={skipSearch}
                disabled={busy}
                title="跳过 claspclub 搜索，改为手动填写豆瓣链接"
              >
                跳过匹配
              </button>
              <button className="btn" onClick={close} disabled={busy}>
                取消
              </button>
              <button
                className="btn primary"
                disabled={busy || selected.length === 0}
                onClick={toNext}
              >
                下一步
              </button>
            </div>
          </>
        )}

        {step === "douban" && (
          <>
            <h2 className="modal-title">豆 瓣 链 接</h2>
            <div className="modal-file">{fileName}</div>

            <div className="search-hint">
              豆瓣无搜索接口，可粘贴书籍页链接补充短评与简介来源（每行一条，可留空）
            </div>

            <label className="modal-label">
              豆瓣书籍页链接（每行一条，可填写多个版本）
              <textarea
                value={doubanText}
                rows={5}
                placeholder={"https://book.douban.com/subject/26771719/\nhttps://book.douban.com/subject/1731370/"}
                onChange={(e) => setDoubanText(e.target.value)}
                disabled={busy}
                autoFocus
              />
            </label>

            {preparing && (
              <div className="busy-row">
                <div className="busy-text">{progressText ?? "预处理中…"}</div>
                {progressPct != null && (
                  <div className="progress-bar">
                    <div className="progress-fill" style={{ width: `${progressPct}%` }} />
                  </div>
                )}
              </div>
            )}
            {doubanErr && <div className="modal-error">{doubanErr}</div>}
            {error && <div className="modal-error">{error}</div>}

            <div className="modal-actions">
              <button className="btn" onClick={() => setStep("search")} disabled={busy}>
                上一步
              </button>
              <button className="btn" onClick={close} disabled={busy}>
                取消
              </button>
              <button className="btn primary" onClick={doubanToConfirm} disabled={busy}>
                下一步
              </button>
            </div>
          </>
        )}

        {step === "confirm" && (
          <>
            <h2 className="modal-title">确 认 导 入</h2>
            <div className="modal-file">{fileName}</div>

            <div className="edit-layout">
              <div className="edit-side">
                <div className="edit-side-cover">
                  {coverOverridePath ? (
                    <FileImg className="edit-side-img" path={coverOverridePath} alt={title} />
                  ) : prepared?.cover_path ? (
                    <FileImg className="edit-side-img" path={prepared.cover_path} alt={title} />
                  ) : preview.has_epub_cover ? (
                    <EpubCover path={preview.path} title={title} className="edit-side-img" />
                  ) : (
                    <div className="edit-side-img placeholder">{title}</div>
                  )}
                </div>
                <button
                  className="btn small"
                  onClick={onUploadCover}
                  disabled={busy || !prepared}
                >
                  上传封面…
                </button>
                {isMerged && (
                  <span className="merged-tip">合并本 · 已选 {selected.length} 条，封面取第一本</span>
                )}
                {!selected.length && doubanLinks.length > 0 && prepared?.douban_preview && (
                  <span className="merged-tip dim">
                    豆瓣来源：《{prepared.douban_preview.title || "未知"}⟩
                    {prepared.douban_preview.author ? ` · ${prepared.douban_preview.author}` : ""}
                  </span>
                )}
              </div>

              <div className="edit-fields">
                <div className="modal-label">
                  <div className="label-row">
                    <span>书名</span>
                    <TranslateBtn field="title" />
                  </div>
                  <input
                    value={title}
                    onChange={(e) => setTitle(e.target.value)}
                    disabled={busy}
                    autoFocus
                  />
                </div>

                <div className="modal-label">
                  <div className="label-row">
                    <span>作者</span>
                    <TranslateBtn field="author" />
                  </div>
                  <input
                    value={author}
                    onChange={(e) => setAuthor(e.target.value)}
                    disabled={busy}
                  />
                </div>

                <label className="modal-label">
                  标签（逗号分隔）
                  <input
                    value={tagsStr}
                    onChange={(e) => setTagsStr(e.target.value)}
                    disabled={busy}
                  />
                </label>

                <div className="series-row">
                  <label className="modal-label">
                    系列
                    <input
                      value={seriesName}
                      placeholder="无系列"
                      onChange={(e) => setSeriesName(e.target.value)}
                      disabled={busy}
                    />
                  </label>
                  <label className="modal-label">
                    卷号
                    <input
                      type="number"
                      value={seriesOrder}
                      min={1}
                      placeholder="1"
                      onChange={(e) => setSeriesOrder(e.target.value)}
                      disabled={busy || !seriesName.trim()}
                    />
                  </label>
                </div>

                <div className="modal-label">
                  <div className="label-row">
                    <span>简介</span>
                    <span className="label-row-end">
                      {prepared?.fusion_error && (
                        <span className="label-warn">{prepared.fusion_error}</span>
                      )}
                      <TranslateBtn field="desc" />
                    </span>
                  </div>
                  <textarea
                    value={desc}
                    rows={7}
                    onChange={(e) => setDesc(e.target.value)}
                    disabled={busy}
                  />
                </div>

                {!preview.is_chinese && (
                  <div className="modal-warn">书名不是中文，请确认或修改后再导入</div>
                )}
              </div>
            </div>

            {committing && (
              <div className="busy-row">
                <div className="busy-text">入库中…</div>
              </div>
            )}
            {error && <div className="modal-error">{error}</div>}

            <div className="modal-actions">
              <button className="btn" onClick={backToSearch} disabled={busy}>
                重新搜索
              </button>
              <button className="btn" onClick={close} disabled={busy}>
                取消
              </button>
              <button
                className="btn primary"
                disabled={busy || !title.trim()}
                onClick={onImport}
              >
                {committing ? "导入中…" : "导 入"}
              </button>
            </div>
          </>
        )}
        {confirmElement}
      </div>
    </div>
  );
}

/** 解析豆瓣链接输入（按行/空白/逗号拆分，去重去空） */
function parseDoubanLinks(text: string): string[] {
  const out: string[] = [];
  for (const part of text.split(/[\r\n,，\t]+|\s{2,}/)) {
    const u = part.trim();
    if (u && !out.includes(u)) out.push(u);
  }
  return out;
}
