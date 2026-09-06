import { useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { listen } from "@tauri-apps/api/event";
import { getCurrentWebview } from "@tauri-apps/api/webview";
import {
  analyzeEpub,
  cancelSyncTask,
  cancelTask,
  collectEpubs,
  deleteBook,
  getBooks,
  importEpub,
  llmStatus,
  mergeBooks,
  setBookStatus,
  webdavPull,
  webdavPush,
} from "../api/tauri";
import type { BookCard, EpubPreview, ImportResult, TaskProgressEvent } from "../types";
import { getConfig } from "../api/tauri";
import { pickDirectory, pickEpub } from "../api/picker";
import BookCardItem from "../components/BookCard";
import SearchBar from "../components/SearchBar";
import StateView from "../components/StateView";
import AddBookModal, { ensureMysteryTag } from "../components/AddBookModal";
import FilterSidebar, { type FilterGroup } from "../components/FilterSidebar";
import { applyTheme } from "../theme";
import SettingsModal from "../components/SettingsModal";
import ReviewModal, { type ReviewTarget } from "../components/ReviewModal";
import { useConfirm } from "../components/ConfirmDialog";

/** 批量模式中等待用户确认的一本书（Promise 挂起，弹窗回调解锁） */
interface PendingConfirm {
  preview: EpubPreview;
  resolve: (res: ImportResult | null) => void;
}

const CANCELLED_MSG = "任务已取消";

/** 排序键（前端排序：中文按拼音序） */
type SortKey = "default" | "recent" | "title" | "author" | "series";

const SORT_OPTIONS: Array<{ value: SortKey; label: string }> = [
  { value: "default", label: "排序：添加顺序" },
  { value: "recent", label: "排序：最近添加" },
  { value: "title", label: "排序：书名" },
  { value: "author", label: "排序：作者" },
  { value: "series", label: "排序：系列" },
];

const zhCollator = new Intl.Collator("zh-Hans-CN");

/** 排序偏好持久化键（localStorage，跨会话保持） */
const SORT_PREF_KEY = "library.sortKey";

/** 书架显示模式：grid = 网格（封面+信息）；list = 列表（封面左、信息右）；cover = 仅封面 */
type ViewMode = "grid" | "list" | "cover";
/** 封面大小（网格/仅封面视图生效） */
type CoverSize = "large" | "medium" | "small";

const VIEW_KEY = "library.view";
const SIZE_KEY = "library.coverSize";

function loadView(): ViewMode {
  const v = localStorage.getItem(VIEW_KEY);
  return v === "list" || v === "cover" || v === "grid" ? v : "grid";
}

/** 未手动设置过时：窄屏（移动端）默认较小封面 */
function loadCoverSize(): CoverSize {
  const v = localStorage.getItem(SIZE_KEY);
  if (v === "large" || v === "medium" || v === "small") return v;
  return typeof window !== "undefined" && window.innerWidth < 720
    ? "small"
    : "medium";
}

const VIEW_LABELS: Record<ViewMode, string> = {
  grid: "网格视图",
  list: "列表视图",
  cover: "仅封面",
};

function loadSortPref(): SortKey {
  const v = localStorage.getItem(SORT_PREF_KEY);
  return SORT_OPTIONS.some((o) => o.value === v) ? (v as SortKey) : "default";
}

/**
 * 工具栏下拉菜单：默认点击展开；提供 onMain 时为分裂按钮（主区域执行动作，
 * 箭头/悬浮展开菜单）。悬浮展开同时兼容触屏（无 hover 时点击箭头）。
 */
function ToolbarMenu({
  label,
  disabled,
  active,
  hint,
  onMain,
  children,
}: {
  label: ReactNode;
  disabled?: boolean;
  active?: boolean;
  hint?: string;
  /** 提供时为分裂按钮：主按钮执行动作，箭头展开菜单 */
  onMain?: () => void;
  children: ReactNode;
}) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);

  // 点击菜单外区域时收起
  useEffect(() => {
    if (!open) return;
    const onDoc = (e: MouseEvent) => {
      if (!ref.current?.contains(e.target as Node)) setOpen(false);
    };
    document.addEventListener("mousedown", onDoc);
    return () => document.removeEventListener("mousedown", onDoc);
  }, [open]);

  return (
    <div
      className="tb-menu"
      ref={ref}
      onMouseEnter={() => !disabled && setOpen(true)}
      onMouseLeave={() => setOpen(false)}
    >
      {onMain !== undefined ? (
        <>
          <button
            className={`btn add-btn ${active ? "primary" : ""}`}
            onClick={onMain}
            disabled={disabled}
            title={hint}
          >
            {label}
          </button>
          <button
            className="btn add-btn tb-caret"
            onClick={() => setOpen((o) => !o)}
            disabled={disabled}
            title="更多选项"
          >
            ▾
          </button>
        </>
      ) : (
        <button
          className={`btn add-btn ${active ? "primary" : ""}`}
          onClick={() => setOpen((o) => !o)}
          disabled={disabled}
          title={hint}
        >
          {label} <span className="tb-caret-inline">▾</span>
        </button>
      )}
      {open && (
        <div
          className="tb-menu-list"
          onClick={(e) => {
            // 点击菜单项后收起（带 keep-open 标识的开关项除外）
            const item = (e.target as HTMLElement).closest(".tb-menu-item");
            if (item && !item.classList.contains("keep-open")) setOpen(false);
          }}
        >
          {children}
        </div>
      )}
    </div>
  );
}

/** 菜单项：on = 当前开启（主题色标识开关状态）；普通动作项不传 on */
function ToolbarMenuItem({
  on,
  onClick,
  keepOpen,
  children,
  title,
}: {
  /** 开关状态；undefined = 普通动作项（无状态标识） */
  on?: boolean;
  onClick: () => void;
  /** 开关类菜单项点击后不收起菜单 */
  keepOpen?: boolean;
  children: ReactNode;
  title?: string;
}) {
  return (
    <button
      type="button"
      className={`tb-menu-item ${on === true ? "on" : ""} ${keepOpen ? "keep-open" : ""}`}
      onClick={onClick}
      title={title}
    >
      {children}
    </button>
  );
}

export default function LibraryPage() {
  const { confirm, confirmElement } = useConfirm();
  const [books, setBooks] = useState<BookCard[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  // 侧栏筛选：每组已勾选项（空 = 全选）
  const [filters, setFilters] = useState<Record<string, string[]>>({});
  const [defaultTags, setDefaultTags] = useState<string[]>(["推理小说"]);
  const [settingsOpen, setSettingsOpen] = useState(false);
  const [libTitle, setLibTitle] = useState("推理小说");
  /** 是否尚未创建任何书库（隐藏工具栏/筛选，主页提示） */
  const [noLibrary, setNoLibrary] = useState(false);
  /** 当前书库 WebDav 状态（启用时工具栏显示同步按钮） */
  const [currentLibId, setCurrentLibId] = useState<string | null>(null);
  const [webdavEnabled, setWebdavEnabled] = useState(false);
  const [syncBusy, setSyncBusy] = useState(false);
  /** 当前同步方向（null = 空闲；用于进度条文案） */
  const [syncDir, setSyncDir] = useState<"push" | "pull" | null>(null);
  /** 同步实时进度（task-progress 事件中 phase 以 webdav- 开头的负载） */
  const [syncPhase, setSyncPhase] = useState<TaskProgressEvent | null>(null);
  const syncTaskIdRef = useRef<string>("");
  const [query, setQuery] = useState("");
  const [debounced, setDebounced] = useState("");

  // 加书流程状态
  const [refresh, setRefresh] = useState(0);
  const [analyzing, setAnalyzing] = useState(false);
  const [preview, setPreview] = useState<EpubPreview | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [warnNotice, setWarnNotice] = useState<string | null>(null);

  // 长任务进度（task-progress 事件实时推送）
  const [phase, setPhase] = useState<TaskProgressEvent | null>(null);
  const taskSeq = useRef(0);

  // 批量模式
  const [autoImport, setAutoImport] = useState(true);
  const [pending, setPending] = useState<PendingConfirm | null>(null);
  const [batchStatus, setBatchStatus] = useState<string | null>(null);
  const [batchProgress, setBatchProgress] = useState<{ current: number; total: number } | null>(null);
  const batchCancelRef = useRef(false);
  const batchCurrentTaskRef = useRef<string | null>(null);

  // 拖拽导入
  const [dragging, setDragging] = useState(false);

  // LLM 可用性（合并本简介预检；null = 检测中）
  const [llmReady, setLlmReady] = useState<boolean | null>(null);

  // 阅读状态切换 → 已读短评弹窗
  const [reviewTarget, setReviewTarget] = useState<ReviewTarget | null>(null);

  // 排序 / 多选批量操作（排序偏好持久化，重新进入页面时维持上次选项）
  const [sortKey, setSortKey] = useState<SortKey>(loadSortPref);
  // 书架显示方式（持久化；窄屏未设置时默认小封面）
  const [view, setView] = useState<ViewMode>(loadView);
  const [coverSize, setCoverSize] = useState<CoverSize>(loadCoverSize);
  const [selectMode, setSelectMode] = useState(false);
  const [selected, setSelected] = useState<Set<number>>(new Set());
  const [bulkBusy, setBulkBusy] = useState(false);
  // 合并模式：有序选择，确认后按顺序合并
  const [mergeMode, setMergeMode] = useState(false);
  const [mergeIds, setMergeIds] = useState<number[]>([]);

  useEffect(() => {
    localStorage.setItem(SORT_PREF_KEY, sortKey);
  }, [sortKey]);

  useEffect(() => {
    localStorage.setItem(VIEW_KEY, view);
    localStorage.setItem(SIZE_KEY, coverSize);
  }, [view, coverSize]);

  // 侧栏筛选项（从全部书籍提取 facet；多作者按顿号/逗号拆分）
  const filterGroups: FilterGroup[] = useMemo(() => {
    const authors = new Set<string>();
    const tags = new Set<string>();
    for (const b of books) {
      for (const a of b.author.split(/[、,，/]/)) {
        const t = a.trim();
        if (t) authors.add(t);
      }
      for (const t of (b.tags || "").split(/[,，]/)) {
        const s = t.trim();
        if (s) tags.add(s);
      }
    }
    return [
      { key: "status", label: "阅读状态", options: ["想读", "在读", "已读"].sort((a, b) => zhCollator.compare(a, b)) },
      { key: "author", label: "作者", options: [...authors].sort((a, b) => zhCollator.compare(a, b)) },
      { key: "tag", label: "标签", options: [...tags].sort((a, b) => zhCollator.compare(a, b)) },
    ];
  }, [books]);

  const toggleFilter = (key: string, option: string) => {
    setFilters((prev) => {
      const cur = prev[key] ?? [];
      const next = cur.includes(option)
        ? cur.filter((x) => x !== option)
        : [...cur, option];
      return { ...prev, [key]: next };
    });
  };

  const clearFilterGroup = (key: string) =>
    setFilters((prev) => ({ ...prev, [key]: [] }));

  // 筛选（客户端执行；每组全未选 = 全选）
  const filteredBooks = useMemo(() => {
    return books.filter((b) => {
      const st = b.status || "想读";
      const selStatus = filters.status ?? [];
      if (selStatus.length > 0 && !selStatus.includes(st)) return false;
      const selAuthor = filters.author ?? [];
      if (selAuthor.length > 0) {
        const as = b.author.split(/[、,，/]/).map((s) => s.trim());
        if (!selAuthor.some((a) => as.includes(a))) return false;
      }
      const selTag = filters.tag ?? [];
      if (selTag.length > 0) {
        const ts = (b.tags || "").split(/[,，]/).map((s) => s.trim());
        if (!selTag.some((t) => ts.includes(t))) return false;
      }
      return true;
    });
  }, [books, filters]);

  // 排序（客户端执行；中文经 Intl.Collator 按拼音序）
  const sortedBooks = useMemo(() => {
    const arr = [...filteredBooks];
    switch (sortKey) {
      case "recent":
        return arr.sort((a, b) => b.id - a.id);
      case "title":
        return arr.sort((a, b) => zhCollator.compare(a.title, b.title));
      case "author":
        return arr.sort((a, b) => zhCollator.compare(a.author, b.author));
      case "series":
        return arr.sort(
          (a, b) =>
            zhCollator.compare(a.series_name, b.series_name) ||
            (a.series_order ?? 0) - (b.series_order ?? 0) ||
            a.id - b.id,
        );
      default:
        return arr; // 数据库 id 顺序（添加顺序）
    }
  }, [filteredBooks, sortKey]);

  // 搜索防抖 300ms
  useEffect(() => {
    const t = setTimeout(() => setDebounced(query.trim()), 300);
    return () => clearTimeout(t);
  }, [query]);

  useEffect(() => {
    llmStatus()
      .then((s) => setLlmReady(s.configured))
      .catch(() => setLlmReady(false));
  }, [refresh]);

  // 订阅后端长任务进度事件（导入类 → phase；WebDav 同步类 → syncPhase）
  useEffect(() => {
    const un = listen<TaskProgressEvent>("task-progress", (e) => {
      if (e.payload.phase.startsWith("webdav")) setSyncPhase(e.payload);
      else setPhase(e.payload);
    });
    return () => {
      un.then((fn) => fn()).catch(() => undefined);
    };
  }, []);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    getBooks(null, debounced || null)
      .then((rows) => {
        if (cancelled) return;
        setBooks(rows);
        setError(null);
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
  }, [debounced, refresh]);

  // 加载应用配置：应用当前书库主题 + 默认标签
  useEffect(() => {
    let cancelled = false;
    getConfig()
      .then((cfg) => {
        if (cancelled) return;
        setNoLibrary(cfg.libraries.length === 0);
        const cur = cfg.libraries.find((l) => l.id === cfg.current_library);
        setCurrentLibId(cfg.current_library);
        setWebdavEnabled(!!cur?.webdav?.enabled);
        applyTheme(cur?.theme ?? null);
        const title = cur?.title?.trim() || "推理小说";
        setLibTitle(title);
        document.title = `${title} · 推理小说书架`;
        setDefaultTags(cfg.default_tags);
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, [refresh]);

  const shortName = (p: string) => p.split(/[\\/]/).pop() ?? p;
  const nextTaskId = () => `import-${Date.now()}-${++taskSeq.current}`;
  const isCancelledErr = (e: unknown) => String(e) === CANCELLED_MSG;

  /** 首页一键 WebDav 同步（当前书库已启用时显示；push = 本地覆盖云端，pull = 云端覆盖本地） */
  const onWebdavSync = async (dir: "push" | "pull") => {
    if (!currentLibId || syncBusy) return;
    setNotice(null);
    setWarnNotice(null);
    setError(null);
    setSyncBusy(true);
    setSyncDir(dir);
    setSyncPhase(null);
    const taskId = `webdav-${Date.now()}-${++taskSeq.current}`;
    syncTaskIdRef.current = taskId;
    try {
      const r =
        dir === "push"
          ? await webdavPush(currentLibId, taskId)
          : await webdavPull(currentLibId, taskId);
      const base =
        dir === "push"
          ? `推送完成：本地 ${r.total} 本，上传 ${r.uploaded}，删除云端多余 ${r.deleted}，跳过 ${r.skipped}`
          : `拉取完成：云端 ${r.total} 本，下载 ${r.downloaded}，删除本地多余 ${r.deleted}，跳过 ${r.skipped}`;
      const dbPart =
        r.books_imported + r.books_updated + r.books_deleted > 0
          ? `；数据库：入库 ${r.books_imported}，覆盖 ${r.books_updated}，移除 ${r.books_deleted}`
          : "";
      const failPart = r.failed > 0 ? `，失败 ${r.failed}` : "";
      setNotice(base + dbPart + failPart);
      // 数据/文件有变动时刷新书架
      if (r.books_imported + r.books_deleted > 0 || dir === "pull") setRefresh((k) => k + 1);
    } catch (e) {
      if (!isCancelledErr(e)) setError(String(e));
      else setNotice("同步已打断");
    } finally {
      setSyncBusy(false);
      setSyncDir(null);
      setSyncPhase(null);
    }
  };

  /** 打断进行中的 WebDav 同步 */
  const onSyncCancel = () => {
    if (syncTaskIdRef.current) void cancelSyncTask(syncTaskIdRef.current);
  };

  /** 加书：选择单个 EPUB → 分析元数据 → 确认弹窗 */
  const onAddBook = async () => {
    setNotice(null);
    setWarnNotice(null);
    setError(null);
    const path = await pickEpub("选择要导入的 EPUB 文件");
    if (!path) return;

    setAnalyzing(true);
    try {
      setPreview(await analyzeEpub(path));
    } catch (e) {
      // 分析失败不阻断书架展示（notice 提示，书籍列表保持可见）
      setNotice(`加书失败：${shortName(path)} — ${String(e)}`);
    } finally {
      setAnalyzing(false);
    }
  };

  /** 批量加书：选择文件夹 → 递归收集 EPUB → 逐本尝试导入 */
  const onBatchAdd = async () => {
    setNotice(null);
    setWarnNotice(null);
    setError(null);
    const dir = await pickDirectory("选择包含 EPUB 的文件夹（递归扫描）");
    if (!dir) return;

    setBatchStatus("扫描文件夹…");
    let list: string[] = [];
    try {
      list = await collectEpubs(dir);
    } catch (e) {
      setBatchStatus(null);
      setError(String(e));
      return;
    }
    if (list.length === 0) {
      setBatchStatus(null);
      setNotice("该文件夹下没有 EPUB 文件");
      return;
    }
    await runBatch(list);
  };

  /** 批量导入：唯一结果可免确认直连导入，其余逐本弹窗确认（与 CLI batch 逻辑一致） */
  const runBatch = async (list: string[]) => {
    batchCancelRef.current = false;
    setBatchProgress(null);
    let ok = 0;
    let skip = 0;
    let fail = 0;
    const failed: string[] = [];
    const fusionWarns: string[] = [];

    for (let i = 0; i < list.length; i++) {
      // 用户打断：跳过剩余书籍
      if (batchCancelRef.current) {
        skip += list.length - i;
        break;
      }
      setBatchProgress({ current: i + 1, total: list.length });
      const path = list[i];
      setBatchStatus(`分析中 ${i + 1}/${list.length}`);
      let pv: EpubPreview;
      try {
        pv = await analyzeEpub(path);
      } catch {
        fail++;
        failed.push(shortName(path));
        continue;
      }

      if (autoImport && pv.matched) {
        // 唯一匹配直连导入（快速路径）
        const params = {
          title: pv.title,
          author: pv.author,
          tags: ensureMysteryTag(pv.matched.tags, defaultTags),
        };
        const taskId = nextTaskId();
        batchCurrentTaskRef.current = taskId;
        setBatchStatus(`导入中 ${i + 1}/${list.length}`);
        try {
          const res = await importEpub(
            pv.effective_path ?? pv.path,
            pv.path,
            params.title,
            params.author,
            params.tags,
            [pv.matched],
            [],
            false,
            taskId,
          );
          ok++;
          if (res.fusion_error) {
            fusionWarns.push(`《${res.title}》${res.fusion_error}`);
          }
        } catch (e) {
          if (isCancelledErr(e)) {
            skip++;
          } else {
            fail++;
            failed.push(shortName(path));
          }
        } finally {
          batchCurrentTaskRef.current = null;
        }
      } else {
        // 弹窗确认（弹窗内完成预处理/确认/提交）
        setBatchStatus(`等待确认 ${i + 1}/${list.length}`);
        const res = await new Promise<ImportResult | null>((resolve) =>
          setPending({ preview: pv, resolve }),
        );
        if (!res) {
          skip++;
          continue;
        }
        ok++;
        if (res.fusion_error) {
          fusionWarns.push(`《${res.title}》${res.fusion_error}`);
        }
      }
    }

    const cancelled = batchCancelRef.current;
    batchCancelRef.current = false;
    setPending(null);
    setBatchStatus(null);
    setBatchProgress(null);
    setRefresh((k) => k + 1);
    setNotice(
      (cancelled ? "批量导入已中断：" : "批量导入完成：") +
        `成功 ${ok} 本，跳过 ${skip} 本，失败 ${fail} 本` +
        (failed.length > 0 ? `（${failed.join("、")}）` : ""),
    );
    if (fusionWarns.length > 0) {
      setWarnNotice(`简介融合降级：${fusionWarns.join("；")}`);
    }
  };

  /** 拖拽导入：EPUB 文件走单本/批量流程，文件夹递归收集 */
  const handleDrop = async (paths: string[]) => {
    if (addBusy) return;
    const epubs: string[] = [];
    for (const p of paths) {
      if (p.toLowerCase().endsWith(".epub")) {
        epubs.push(p);
      } else {
        // 非文件夹（普通文件）会收集失败，静默忽略
        const inner = await collectEpubs(p).catch(() => [] as string[]);
        epubs.push(...inner);
      }
    }
    if (epubs.length === 0) {
      setNotice("拖拽内容中没有 EPUB 文件或文件夹");
      return;
    }
    setNotice(null);
    setWarnNotice(null);
    setError(null);
    if (epubs.length === 1) {
      setAnalyzing(true);
      try {
        setPreview(await analyzeEpub(epubs[0]));
      } catch (e) {
        // 分析失败不阻断书架展示
        setNotice(`加书失败：${shortName(epubs[0])} — ${String(e)}`);
      } finally {
        setAnalyzing(false);
      }
      return;
    }
    await runBatch(epubs);
  };

  // 订阅 WebView 拖拽事件
  useEffect(() => {
    const un = getCurrentWebview().onDragDropEvent((event) => {
      if (event.payload.type === "enter" || event.payload.type === "over") {
        setDragging(true);
      } else if (event.payload.type === "leave") {
        setDragging(false);
      } else if (event.payload.type === "drop") {
        setDragging(false);
        void handleDrop(event.payload.paths);
      }
    });
    return () => {
      un.then((fn) => fn()).catch(() => undefined);
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [autoImport, batchStatus != null]);

  /** 打断批量导入：停止后续书籍 + 打断当前导入任务 */
  const onBatchCancel = () => {
    batchCancelRef.current = true;
    if (batchCurrentTaskRef.current) {
      cancelTask(batchCurrentTaskRef.current).catch(() => undefined);
    }
  };

  /** 单本模式：弹窗内完成导入，此处仅处理结果与提示 */
  const onImported = (res: ImportResult | null) => {
    setPreview(null);
    setPhase(null);
    if (!res) return;
    setRefresh((k) => k + 1);
    if (res.fusion_error) {
      setWarnNotice(`《${res.title}》简介未融合：${res.fusion_error}`);
      setNotice(null);
    } else {
      setWarnNotice(null);
      setNotice(
        `《${res.title}》已入库（网络短评 ${res.comments} 条` +
          (res.fusion_model ? ` · 简介融合：${res.fusion_model}` : "") +
          "）",
      );
    }
  };

  /** 批量模式：弹窗完成导入，回传结果继续处理剩余书籍 */
  const onBatchFinished = (res: ImportResult | null) => {
    pending?.resolve(res);
    setPending(null);
  };

  /** 切换阅读状态；切到"已读"时弹出短评询问 */
  const handleStatusChange = (id: number, newStatus: string, title: string) => {
    setBookStatus(id, newStatus)
      .then(() => {
        setBooks((prev) => prev.map((b) => (b.id === id ? { ...b, status: newStatus } : b)));
        if (newStatus === "已读") {
          const b = books.find((x) => x.id === id);
          setReviewTarget({
            id,
            title,
            author: b?.author ?? "",
            tags: b?.tags ?? "",
          });
        }
      })
      .catch((e) => setError(String(e)));
  };

  // =========================== //
  //  多选批量操作
  // =========================== //

  /** 点击卡片切换选中状态（合并模式下记录顺序） */
  const toggleSelect = (id: number) => {
    if (mergeMode) {
      setMergeIds((prev) =>
        prev.includes(id) ? prev.filter((x) => x !== id) : [...prev, id],
      );
      return;
    }
    setSelected((prev) => {
      const next = new Set(prev);
      if (next.has(id)) {
        next.delete(id);
      } else {
        next.add(id);
      }
      return next;
    });
  };

  /** 全选当前筛选结果 / 全不选 */
  const toggleSelectAll = () => {
    setSelected((prev) =>
      prev.size === sortedBooks.length && sortedBooks.length > 0
        ? new Set()
        : new Set(sortedBooks.map((b) => b.id)),
    );
  };

  /** 退出多选并清空选择 */
  const exitSelectMode = () => {
    setSelectMode(false);
    setMergeMode(false);
    setSelected(new Set());
    setMergeIds([]);
  };

  /** 合并模式确认：按顺序合并所选书籍（EPUB + 来源 + 短评），删除原书 */
  const onMergeSelected = async () => {
    if (mergeIds.length < 2 || bulkBusy) return;
    const ok = await confirm({
      title: "合并确认",
      message: `确认按当前顺序合并选中的 ${mergeIds.length} 本书吗？\n\n将创建合集（封面取第一本、来源项目与短评按顺序合并），原书将从书架移除（原始导入文件不受影响）。`,
      okLabel: "合并",
      danger: true,
    });
    if (!ok) return;
    // 元数据保留逻辑：确认是否调用 LLM 合并多个简介
    let mergeDesc = false;
    if (llmReady) {
      mergeDesc = await confirm({
        title: "简介合并",
        message:
          "是否调用 LLM 将多本书的简介合并为一段？\n选择「取第一本」则仅保留第一本书的简介。",
        okLabel: "合并简介",
        cancelLabel: "取第一本",
      });
    }
    setBulkBusy(true);
    setError(null);
    try {
      const res = await mergeBooks([...mergeIds], mergeDesc);
      setMergeIds([]);
      setSelectMode(false);
      setMergeMode(false);
      setRefresh((k) => k + 1);
      setNotice(`已合并为《${res.title}》`);
    } catch (e) {
      setError(String(e));
    } finally {
      setBulkBusy(false);
    }
  };

  /** 批量标记阅读状态（直接入库，不弹短评询问） */
  const bulkSetStatus = async (s: string) => {
    if (selected.size === 0 || bulkBusy) return;
    setBulkBusy(true);
    setError(null);
    let ok = 0;
    let fail = 0;
    for (const id of selected) {
      try {
        await setBookStatus(id, s);
        ok++;
      } catch {
        fail++;
      }
    }
    setBulkBusy(false);
    setSelected(new Set());
    setRefresh((k) => k + 1);
    setNotice(
      `已将 ${ok} 本书标记为「${s}」` + (fail > 0 ? `（${fail} 本失败）` : ""),
    );
  };

  /** 批量删除（应用内确认框；书库 EPUB 与封面缓存按引用计数清理，原始文件不动） */
  const bulkDelete = async () => {
    if (selected.size === 0 || bulkBusy) return;
    const ok = await confirm({
      title: "删除确认",
      message: `确定删除选中的 ${selected.size} 本书吗？\n\n书库中的 EPUB 文件与封面缓存将一并删除（原始导入文件不受影响）。`,
      okLabel: "删除",
      danger: true,
    });
    if (!ok) return;
    setBulkBusy(true);
    setError(null);
    let done = 0;
    let fail = 0;
    for (const id of selected) {
      try {
        await deleteBook(id);
        done++;
      } catch {
        fail++;
      }
    }
    setBulkBusy(false);
    setSelected(new Set());
    setRefresh((k) => k + 1);
    setNotice(`已删除 ${done} 本` + (fail > 0 ? `，${fail} 本失败` : ""));
  };

  const addBusy = analyzing || batchStatus != null;
  const phaseText = phase ? phase.message : null;
  const phasePct =
    phase && phase.total > 0 ? Math.round((phase.current / phase.total) * 100) : null;
  // 同步进度百分比（首个事件到达前显示 0%）
  const syncPct =
    syncPhase && syncPhase.total > 0
      ? Math.min(100, Math.round((syncPhase.current / syncPhase.total) * 100))
      : 0;

  return (
    <div className="page">
      <header className="header">
        <button className="settings-link" onClick={() => setSettingsOpen(true)}>
          设 置
        </button>
        <h1>
          {libTitle} <span>·</span> 书 架
        </h1>
        <div className="sub">{sortedBooks.length} 册</div>
        <div className="rule" />
      </header>

      <div className="library-main">
          {noLibrary ? (
            <div className="empty-library">
              <div className="empty-library-icon">📚</div>
              <h2>还没有书库</h2>
              <p>先在设置中创建一个书库（选择书库名与本地目录），再开始导入书籍。</p>
              <button className="btn primary" onClick={() => setSettingsOpen(true)}>
                打开设置，创建书库
              </button>
            </div>
          ) : (
            <>
          <div className="toolbar">
            <div className="toolbar-left">
              <select
                className="sort-select"
                value={sortKey}
                onChange={(e) => setSortKey(e.target.value as SortKey)}
                disabled={addBusy}
                title="选择排序方式"
              >
                {SORT_OPTIONS.map((o) => (
                  <option key={o.value} value={o.value}>
                    {o.label}
                  </option>
                ))}
              </select>
              <button
                className={`btn add-btn ${selectMode ? "primary" : ""}`}
                onClick={() => (selectMode ? exitSelectMode() : setSelectMode(true))}
                disabled={addBusy || bulkBusy}
              >
                {selectMode ? "退出多选" : "多选"}
              </button>
              <button className="btn primary add-btn" onClick={onAddBook} disabled={addBusy || selectMode}>
                ＋ 加书
              </button>
              <ToolbarMenu
                label="批量加书"
                disabled={addBusy || selectMode}
                hint="选择包含 EPUB 的文件夹（递归扫描）"
                onMain={onBatchAdd}
              >
                <ToolbarMenuItem
                  on={autoImport}
                  keepOpen
                  onClick={() => setAutoImport((v) => !v)}
                  title="批量导入时，claspclub 唯一匹配结果不弹窗直接入库"
                >
                  唯一结果自动导入
                </ToolbarMenuItem>
              </ToolbarMenu>
              {webdavEnabled && (
                <ToolbarMenu
                  label={syncBusy ? "同步中…" : "同 步"}
                  active={syncBusy}
                  disabled={addBusy || selectMode || syncBusy}
                  hint="同步到云端 = 本地完整覆盖云端；从云端同步 = 云端完整覆盖本地"
                >
                  <ToolbarMenuItem
                    onClick={() => void onWebdavSync("push")}
                    title="把本地书库完整覆盖到云端：上传全部 EPUB/封面/数据库，删除云端多出的文件"
                  >
                    ↑ 同步到云端
                  </ToolbarMenuItem>
                  <ToolbarMenuItem
                    onClick={() => void onWebdavSync("pull")}
                    title="用云端完整覆盖本地书库：下载 EPUB/封面/数据库，删除本地多出的文件"
                  >
                    ↓ 从云端同步
                  </ToolbarMenuItem>
                </ToolbarMenu>
              )}
              <ToolbarMenu label="显 示" hint="调整书架显示方式与封面大小">
                <ToolbarMenuItem
                  on={view === "grid"}
                  onClick={() => setView("grid")}
                  title="一行多本，封面下方显示书名/作者/系列"
                >
                  {VIEW_LABELS.grid}
                </ToolbarMenuItem>
                <ToolbarMenuItem
                  on={view === "list"}
                  onClick={() => setView("list")}
                  title="一行一本：封面在左，书名与系列信息在右"
                >
                  {VIEW_LABELS.list}
                </ToolbarMenuItem>
                <ToolbarMenuItem
                  on={view === "cover"}
                  onClick={() => setView("cover")}
                  title="一行多本，仅显示封面"
                >
                  {VIEW_LABELS.cover}
                </ToolbarMenuItem>
                {view !== "list" && (
                  <>
                    <span className="tb-menu-sep" />
                    <ToolbarMenuItem
                      on={coverSize === "large"}
                      keepOpen
                      onClick={() => setCoverSize("large")}
                    >
                      封面大小：大
                    </ToolbarMenuItem>
                    <ToolbarMenuItem
                      on={coverSize === "medium"}
                      keepOpen
                      onClick={() => setCoverSize("medium")}
                    >
                      封面大小：中
                    </ToolbarMenuItem>
                    <ToolbarMenuItem
                      on={coverSize === "small"}
                      keepOpen
                      onClick={() => setCoverSize("small")}
                    >
                      封面大小：小
                    </ToolbarMenuItem>
                  </>
                )}
              </ToolbarMenu>
            </div>
            <SearchBar value={query} onChange={setQuery} />
          </div>

          <FilterSidebar
            groups={filterGroups}
            selected={filters}
            onToggle={toggleFilter}
            onClearGroup={clearFilterGroup}
          />
            </>
          )}

      {selectMode && (
        <div className="bulk-bar">
          <span className="bulk-count">
            {mergeMode ? `已选 ${mergeIds.length} 项（有序）` : `已选 ${selected.size} 项`}
          </span>
          {!mergeMode && (
            <button
              className="btn small"
              onClick={toggleSelectAll}
              disabled={bulkBusy || sortedBooks.length === 0}
            >
              {sortedBooks.length > 0 && selected.size === sortedBooks.length ? "全不选" : "全选"}
            </button>
          )}
          <label className="check" title="开启后点击卡片按顺序选择，确认后合并为合集">
            <input
              type="checkbox"
              checked={mergeMode}
              onChange={(e) => {
                setMergeMode(e.target.checked);
                setSelected(new Set());
                if (!e.target.checked) setMergeIds([]);
              }}
              disabled={bulkBusy}
            />
            合并模式
          </label>
          <span className="bulk-sep" />
          {mergeMode ? (
            <button
              className="btn small primary"
              onClick={onMergeSelected}
              disabled={bulkBusy || mergeIds.length < 2}
              title="按顺序合并为合集：封面取第一本，来源与短评按顺序拼接"
            >
              {bulkBusy ? "合并中…" : "合并所选"}
            </button>
          ) : (
            <>
              <button
                className="btn small"
                onClick={() => bulkSetStatus("想读")}
                disabled={bulkBusy || selected.size === 0}
                title="批量标记不会弹出短评询问"
              >
                设为想读
              </button>
              <button
                className="btn small"
                onClick={() => bulkSetStatus("在读")}
                disabled={bulkBusy || selected.size === 0}
                title="批量标记不会弹出短评询问"
              >
                设为在读
              </button>
              <button
                className="btn small"
                onClick={() => bulkSetStatus("已读")}
                disabled={bulkBusy || selected.size === 0}
                title="批量标记不会弹出短评询问"
              >
                设为已读
              </button>
              <span className="bulk-sep" />
              <button
                className="btn small danger-btn"
                onClick={bulkDelete}
                disabled={bulkBusy || selected.size === 0}
              >
                删除所选
              </button>
            </>
          )}
          <span className="bulk-flex" />
          {bulkBusy && <span className="bulk-hint">处理中…</span>}
          <button className="btn small" onClick={exitSelectMode} disabled={bulkBusy}>
            完成
          </button>
        </div>
      )}

      {syncBusy && syncDir && (
        <div className="notice running batch-bar">
          <div className="batch-info">
            <div className="batch-text">
              {syncDir === "push" ? "正在同步到云端…" : "正在从云端同步…"}
              {syncPhase
                ? ` · ${syncPhase.message}（${syncPhase.current}/${syncPhase.total}，${syncPct}%）`
                : " · 准备中…"}
            </div>
            <div className="progress-bar batch">
              <div className="progress-fill" style={{ width: `${syncPct}%` }} />
            </div>
          </div>
          <button className="btn small danger-btn" onClick={onSyncCancel}>
            打断
          </button>
        </div>
      )}
      {batchStatus && (
        <div className="notice running batch-bar">
          <div className="batch-info">
            <div className="batch-text">
              {batchStatus}
              {batchStatus.startsWith("导入中") && phase ? ` · ${phase.message}` : ""}
            </div>
            {batchProgress && (
              <div className="progress-bar batch">
                <div
                  className="progress-fill"
                  style={{
                    width: `${Math.round(
                      (batchProgress.current / batchProgress.total) * 100,
                    )}%`,
                  }}
                />
              </div>
            )}
          </div>
          <button className="btn small danger-btn" onClick={onBatchCancel}>
            打断
          </button>
        </div>
      )}
      {!batchStatus && warnNotice && <div className="notice warn">{warnNotice}</div>}
      {!batchStatus && !warnNotice && notice && <div className="notice">{notice}</div>}

      {error ? (
        <StateView kind="error" message={error} />
      ) : loading && books.length === 0 ? (
        <StateView kind="loading" />
      ) : filteredBooks.length === 0 ? (
        <StateView kind="empty" />
      ) : (
        <div className={`grid view-${view} ${view !== "list" ? `size-${coverSize}` : ""}`}>
          {sortedBooks.map((b, i) => (
            <BookCardItem
              key={b.id}
              book={b}
              index={i}
              selectMode={selectMode}
              selected={selected.has(b.id)}
              orderIndex={
                mergeMode && mergeIds.includes(b.id) ? mergeIds.indexOf(b.id) : null
              }
              onToggle={toggleSelect}
              onStatusChange={handleStatusChange}
            />
          ))}
        </div>
      )}
      </div>

      {preview && (
        <AddBookModal
          preview={preview}
          llmReady={llmReady}
          progressText={phaseText}
          progressPct={phasePct}
          onFinished={onImported}
        />
      )}

      {pending && (
        <AddBookModal
          preview={pending.preview}
          llmReady={llmReady}
          progressText={phaseText}
          progressPct={phasePct}
          onFinished={onBatchFinished}
        />
      )}

      {reviewTarget && (
        <ReviewModal
          target={reviewTarget}
          onClose={(changed) => {
            setReviewTarget(null);
            if (changed) setRefresh((k) => k + 1);
          }}
        />
      )}
      {settingsOpen && (
        <SettingsModal
          onClose={() => {
            setSettingsOpen(false);
            setRefresh((k) => k + 1);
          }}
        />
      )}
      {confirmElement}

      {dragging && (
        <div className="drop-overlay">
          <div className="drop-box">
            <div className="drop-icon">⤓</div>
            <div>松开以导入 EPUB 文件 / 文件夹</div>
          </div>
        </div>
      )}
    </div>
  );
}
