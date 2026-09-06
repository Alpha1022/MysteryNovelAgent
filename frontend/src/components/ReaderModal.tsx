import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { getReaderContent } from "../api/tauri";
import type { ReaderContent } from "../types";

interface Props {
  bookId: number;
  title: string;
  author: string;
  onClose: () => void;
}

type ReadMode = "scroll" | "paged";
type PageDir = "h" | "v";

interface ReaderProgress {
  c: number;
  fs: number;
  y: number;
  /** 阅读模式：scroll = 上下滚动（默认）；paged = 自动分页 */
  mode?: ReadMode;
  /** 分页方向：h = 左右翻页（默认）；v = 上下翻页 */
  dir?: PageDir;
  /** 分页模式下章内页码（0 起） */
  pg?: number;
}

const FS_MIN = 12;
const FS_MAX = 30;
const FS_DEFAULT = 18;
/** 分页模式左右留白（px，需与 buildDoc 中 SIDE_PAD 一致） */
const SIDE_PAD = 24;
/** 分页模式左右翻页的列间距（px，即页与页之间的视觉间隔） */
const COL_GAP = 48;

function progressKey(bookId: number) {
  return `mna-reader:${bookId}`;
}

function loadProgress(bookId: number): ReaderProgress {
  try {
    const raw = localStorage.getItem(progressKey(bookId));
    if (raw) {
      const p = JSON.parse(raw);
      if (typeof p.c === "number" && typeof p.fs === "number") {
        return {
          c: Math.max(0, p.c),
          fs: Math.min(FS_MAX, Math.max(FS_MIN, p.fs)),
          y: typeof p.y === "number" ? p.y : 0,
          mode: p.mode === "paged" ? "paged" : "scroll",
          dir: p.dir === "v" ? "v" : "h",
          pg: typeof p.pg === "number" ? Math.max(0, p.pg) : 0,
        };
      }
    }
  } catch {
    // 进度损坏时从头开始
  }
  return { c: 0, fs: FS_DEFAULT, y: 0 };
}

/** 剥离书内 <style>/<link>（阅读弹窗使用统一排版，避免书内样式覆盖主题） */
function stripBookStyles(html: string) {
  return html
    .replace(/<style[\s\S]*?<\/style\s*>/gi, "")
    .replace(/<link[^>]*>/gi, "");
}

/** 文档与分页度量（stride = 左右翻页步长；pageH = 上下翻页页高） */
interface DocLayout {
  doc: string;
  stride: number;
  pageH: number;
}

/**
 * 构造 iframe srcdoc。
 *
 * - scroll：常规纵向滚动排版。
 * - paged-h：CSS 多列分页，每列一页，横向 transform 翻页。
 * - paged-v：常规流布局按页高切片，纵向 transform 翻页。
 *
 * 两种分页均把垂直间距约束为行高（1.9em）的整数倍、页高对齐行栅格，
 * 保证翻页时正文行不会被拦腰截断。
 */
function buildDoc(
  html: string,
  fs: number,
  mode: ReadMode,
  dir: PageDir,
  w: number,
  h: number,
): DocLayout {
  const base = `
    html,body{margin:0;background:#20232a;color:#d6d2c4;
      font-family:"Source Han Serif SC","Noto Serif CJK SC","SimSun",Georgia,serif;}
    a{color:#8fb4d9;text-decoration:none;pointer-events:none;}
  `;
  if (mode === "scroll") {
    return {
      doc: `<!doctype html><html><head><meta charset="utf-8"><style>
${base}
body{font-size:${fs}px;line-height:1.9;padding:2.2em 1.4em 4em;max-width:44em;margin:0 auto;word-break:break-word;}
img,svg,video{max-width:100%;height:auto;}
h1,h2,h3,h4{line-height:1.4;color:#e6e2d4;}
blockquote{margin:1em 0;padding:.2em 1em;border-left:3px solid #5b5e66;color:#b5b1a4;}
p{margin:.6em 0;text-align:justify;}
hr{border:none;border-top:1px solid #45484f;}
</style></head><body>${stripBookStyles(html)}</body></html>`,
      stride: 0,
      pageH: 0,
    };
  }

  // 分页排版：垂直节奏全部取整到行栅格
  const linePx = fs * 1.9;
  const padT = Math.round(fs * 2);
  const rows = Math.max(1, Math.floor((h - padT - 8) / linePx));
  const contentH = Math.round(rows * linePx);
  const padB = Math.max(8, Math.round(h - padT - contentH));
  const colW = Math.max(120, Math.round(w - SIDE_PAD * 2));
  const pagedText = `
    img,svg,video{max-width:100%;height:auto;display:block;margin:.5lh 0;}
    h1,h2,h3,h4{line-height:1.9;color:#e6e2d4;margin:.8lh 0 .4lh;}
    p{margin:0;text-align:justify;text-indent:2em;}
    blockquote{margin:.6lh 0;padding:0 1em;border-left:3px solid #5b5e66;color:#b5b1a4;}
    hr{border:none;border-top:1px solid #45484f;margin:.8lh 0;}
    ul,ol{margin:.3lh 0;padding-left:1.4em;}
    li{margin:.15lh 0;}
  `;
  const shell = (flowCss: string) =>
    `<!doctype html><html><head><meta charset="utf-8"><style>
${base}
html,body{height:100%;overflow:hidden;}
body{box-sizing:border-box;font-size:${fs}px;line-height:1.9;padding:${padT}px ${SIDE_PAD}px ${padB}px;word-break:break-word;}
${flowCss}
${pagedText}
</style></head><body><div id="flow">${stripBookStyles(html)}</div></body></html>`;

  if (dir === "h") {
    // 多列分页：容器内容宽恰好一列宽，溢出内容向右续列
    return {
      doc: shell(
        `#flow{height:100%;column-width:${colW}px;column-gap:${COL_GAP}px;column-fill:auto;}`,
      ),
      stride: colW + COL_GAP,
      pageH: contentH,
    };
  }
  return {
    doc: shell(`#flow{width:100%;}`),
    stride: 0,
    pageH: contentH,
  };
}

/** 内置 EPUB 阅读器：章节导航 + 字号调节 + 阅读模式（滚动/分页）+ 进度记忆 */
export default function ReaderModal({ bookId, title, author, onClose }: Props) {
  const [content, setContent] = useState<ReaderContent | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [chapter, setChapter] = useState(0);
  const [fontSize, setFontSize] = useState(FS_DEFAULT);
  const [scrollRatio, setScrollRatio] = useState(0);
  // 阅读模式设置（随进度持久化）
  const [mode, setMode] = useState<ReadMode>("scroll");
  const [dir, setDir] = useState<PageDir>("h");
  const [showSettings, setShowSettings] = useState(false);
  // 分页状态（仅 paged 模式使用）
  const [page, setPage] = useState(0);
  const [pages, setPages] = useState(1);
  const iframeRef = useRef<HTMLIFrameElement>(null);
  const bodyRef = useRef<HTMLDivElement>(null);
  // 待恢复的位置：滚动模式用 y；分页模式用页码（"last" = 章末页）
  const pendingScroll = useRef<number | null>(null);
  const pendingPage = useRef<number | "last" | null>(null);
  const touchCtl = useRef<AbortController | null>(null);
  // 手势/加载回调读取的实时状态（iframe 不重载时闭包会过期）
  const liveRef = useRef({ mode, dir, chapter, page, pages });
  liveRef.current = { mode, dir, chapter, page, pages };

  // 打开时加载内容与上次进度
  useEffect(() => {
    const p = loadProgress(bookId);
    setChapter(p.c);
    setFontSize(p.fs);
    setMode(p.mode ?? "scroll");
    setDir(p.dir ?? "h");
    if ((p.mode ?? "scroll") === "paged") {
      pendingPage.current = p.pg ?? 0;
    } else {
      pendingScroll.current = p.y;
    }
    let cancelled = false;
    getReaderContent(bookId)
      .then((c) => {
        if (cancelled) return;
        setContent(c);
      })
      .catch((e) => {
        if (!cancelled) setError(String(e));
      });
    return () => {
      cancelled = true;
    };
  }, [bookId]);

  // 测量阅读区域尺寸（分页排版依赖；窗口尺寸/朝向变化时重排）
  const [metrics, setMetrics] = useState<{ w: number; h: number } | null>(null);
  useEffect(() => {
    const measure = () => {
      const el = bodyRef.current;
      if (!el) return;
      setMetrics((prev) => {
        const next = { w: el.clientWidth, h: el.clientHeight };
        return prev && prev.w === next.w && prev.h === next.h ? prev : next;
      });
    };
    measure();
    window.addEventListener("resize", measure);
    return () => window.removeEventListener("resize", measure);
  }, []);

  const saveProgress = useCallback(
    (patch: Partial<ReaderProgress>) => {
      try {
        const cur = loadProgress(bookId);
        localStorage.setItem(
          progressKey(bookId),
          JSON.stringify({ ...cur, ...patch }),
        );
      } catch {
        // 存储不可用时静默
      }
    },
    [bookId],
  );

  const total = content?.chapters.length ?? 0;
  const cur = content?.chapters[Math.min(chapter, Math.max(0, total - 1))] ?? null;
  const layout = useMemo(
    () =>
      cur && metrics
        ? buildDoc(cur.html, fontSize, mode, dir, metrics.w, metrics.h)
        : { doc: "", stride: 0, pageH: 0 },
    [cur, fontSize, mode, dir, metrics],
  );

  // 章节越界保护
  useEffect(() => {
    if (total > 0 && chapter >= total) setChapter(total - 1);
  }, [chapter, total]);

  /** 立即把分页 transform 应用到当前文档（翻页不重载 iframe） */
  const applyPageNow = (p: number) => {
    const win = iframeRef.current?.contentWindow;
    const flow = win?.document.getElementById("flow");
    if (!win || !flow || mode !== "paged") return;
    flow.style.transform =
      dir === "h"
        ? `translateX(${-p * layout.stride}px)`
        : `translateY(${-p * layout.pageH}px)`;
  };

  // 字号改变后保持位置（srcdoc 重载触发 onLoad 恢复）
  const changeFont = (delta: number) => {
    if (mode === "paged") {
      pendingPage.current = page;
    } else {
      const frame = iframeRef.current;
      const win = frame?.contentWindow;
      const max = win
        ? Math.max(
            1,
            win.document.documentElement.scrollHeight - win.innerHeight,
          )
        : 1;
      pendingScroll.current = Math.round(((win?.scrollY ?? 0) / max) * max);
    }
    setFontSize((f) => Math.min(FS_MAX, Math.max(FS_MIN, f + delta)));
  };

  /** 章节切换：记录进度；at = 恢复到章首页还是章末页 */
  const gotoChapter = useCallback(
    (idx: number, at: "first" | "last" = "first") => {
      if (!content || idx < 0 || idx >= content.chapters.length) return;
      setChapter(idx);
      setPage(0);
      setPages(1);
      setScrollRatio(0);
      saveProgress({ c: idx, pg: 0 });
      if (mode === "paged") {
        pendingPage.current = at === "last" ? "last" : 0;
      } else {
        pendingPage.current = null;
        pendingScroll.current = 0;
      }
    },
    [content, mode, saveProgress],
  );

  /** 分页翻页：章内越界自动跨章（上一章末页 / 下一章首页） */
  const turnPage = useCallback(
    (delta: number) => {
      const np = page + delta;
      if (np < 0) {
        gotoChapter(chapter - 1, "last");
      } else if (np >= Math.max(1, pages)) {
        gotoChapter(chapter + 1, "first");
      } else {
        setPage(np);
        saveProgress({ pg: np });
        applyPageNow(np);
      }
    },
    [page, pages, chapter, gotoChapter, saveProgress, dir, mode, layout],
  );

  // 手势函数引用（iframe 内触摸监听跨渲染调用最新版本）
  const turnPageRef = useRef(turnPage);
  turnPageRef.current = turnPage;
  const gotoChapterRef = useRef(gotoChapter);
  gotoChapterRef.current = gotoChapter;

  // iframe 加载完成：度量分页 / 恢复位置 / 挂滚动与触摸监听
  const onFrameLoad = () => {
    const frame = iframeRef.current;
    const win = frame?.contentWindow;
    if (!win) return;
    const nav = liveRef.current;

    if (nav.mode === "paged") {
      const flow = win.document.getElementById("flow");
      if (flow) {
        const n =
          dir === "h"
            ? Math.max(
                1,
                Math.round((flow.scrollWidth - (layout.stride - COL_GAP)) / layout.stride) + 1,
              )
            : Math.max(1, Math.ceil(flow.scrollHeight / Math.max(1, layout.pageH)));
        setPages(n);
        let target = 0;
        if (pendingPage.current === "last") target = Math.max(0, n - 1);
        else if (typeof pendingPage.current === "number")
          target = Math.min(Math.max(0, pendingPage.current), n - 1);
        pendingPage.current = null;
        setPage(target);
        flow.style.transform =
          dir === "h"
            ? `translateX(${-target * layout.stride}px)`
            : `translateY(${-target * layout.pageH}px)`;
      }
    } else if (pendingScroll.current != null) {
      win.scrollTo(0, pendingScroll.current);
      pendingScroll.current = null;
    }

    // 滚动监听（仅滚动模式有滚动行为；节流保存进度）
    let last = 0;
    win.addEventListener("scroll", () => {
      if (liveRef.current.mode !== "scroll") return;
      const now = Date.now();
      if (now - last < 300) return;
      last = now;
      const max = Math.max(
        1,
        win.document.documentElement.scrollHeight - win.innerHeight,
      );
      const ratio = win.scrollY / max;
      setScrollRatio(ratio);
      saveProgress({ c: liveRef.current.chapter, y: win.scrollY });
    });

    // 触屏翻页/翻章（触摸事件不跨 iframe 冒泡，挂到内容文档；同源沙箱可访问）
    touchCtl.current?.abort();
    const ctl = new AbortController();
    touchCtl.current = ctl;
    const doc = win.document;
    let sx = 0;
    let sy = 0;
    let st = 0;
    doc.addEventListener(
      "touchstart",
      (e) => {
        const t = e.touches[0];
        sx = t.clientX;
        sy = t.clientY;
        st = Date.now();
      },
      { signal: ctl.signal, passive: true },
    );
    doc.addEventListener(
      "touchend",
      (e) => {
        const t = e.changedTouches[0];
        const dx = t.clientX - sx;
        const dy = t.clientY - sy;
        const swipe = Math.abs(dx) > Math.max(56, Math.abs(dy) * 1.5);
        const tap =
          Math.abs(dx) < 12 && Math.abs(dy) < 12 && Date.now() - st < 350;
        if (!swipe && !tap) return;
        const nav = liveRef.current;
        const forward = swipe ? dx < 0 : t.clientX > win.innerWidth * 0.75;
        const backward = swipe ? dx > 0 : t.clientX < win.innerWidth * 0.25;
        if (nav.mode === "paged") {
          if (forward) turnPageRef.current(1);
          else if (backward) turnPageRef.current(-1);
        } else if (forward) {
          gotoChapterRef.current(nav.chapter + 1);
        } else if (backward) {
          gotoChapterRef.current(nav.chapter - 1);
        }
      },
      { signal: ctl.signal, passive: true },
    );
  };

  // 卸载时清理 iframe 内挂载的触摸监听
  useEffect(() => {
    return () => touchCtl.current?.abort();
  }, []);

  // 键盘：分页模式翻页（跨章），滚动模式翻章
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "ArrowLeft") {
        if (mode === "paged") turnPage(-1);
        else gotoChapter(chapter - 1);
      }
      if (e.key === "ArrowRight") {
        if (mode === "paged") turnPage(1);
        else gotoChapter(chapter + 1);
      }
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  });

  /** 切换阅读模式/方向：重置页位置并持久化 */
  const changeMode = (m: ReadMode) => {
    if (m === mode) {
      setShowSettings(false);
      return;
    }
    setMode(m);
    setPages(1);
    setPage(0);
    setScrollRatio(0);
    saveProgress({ mode: m, pg: 0 });
    if (m === "paged") {
      pendingPage.current = 0;
      pendingScroll.current = null;
    } else {
      pendingScroll.current = 0;
      pendingPage.current = null;
    }
    setShowSettings(false);
  };

  const changeDir = (d: PageDir) => {
    if (d === dir) return;
    setDir(d);
    setPages(1);
    setPage(0);
    saveProgress({ dir: d, pg: 0 });
    pendingPage.current = 0;
    setShowSettings(false);
  };

  const atFirst = !content || (chapter <= 0 && (mode === "scroll" || page <= 0));
  const atLast =
    !content ||
    (chapter >= total - 1 && (mode === "scroll" || page >= pages - 1));
  const pct =
    total > 0
      ? Math.round(
          ((chapter +
            (mode === "paged" ? (page + 1) / Math.max(1, pages) : scrollRatio)) /
            total) *
            100,
        )
      : 0;

  return (
    <div className="reader-overlay">
      <div className="reader-head">
        <div className="reader-meta">
          <div className="reader-title" title={title}>
            {title}
          </div>
          <div className="reader-author">{author}</div>
        </div>
        {content && content.chapters.length > 0 && (
          <select
            className="reader-chapter-select"
            value={chapter}
            onChange={(e) => gotoChapter(Number(e.target.value))}
          >
            {content.chapters.map((c, i) => (
              <option key={i} value={i}>
                {c.title}
              </option>
            ))}
          </select>
        )}
        <div className="reader-settings-wrap">
          <button
            className="chat-tool-btn reader-gear"
            onClick={() => setShowSettings((s) => !s)}
            title="排版设置"
          >
            排 版
          </button>
          {showSettings && (
            <div className="reader-settings">
              <div className="rs-row">
                <span className="rs-label">阅读模式</span>
                <button
                  className={`rs-btn ${mode === "scroll" ? "on" : ""}`}
                  onClick={() => changeMode("scroll")}
                >
                  上下滚动
                </button>
                <button
                  className={`rs-btn ${mode === "paged" ? "on" : ""}`}
                  onClick={() => changeMode("paged")}
                >
                  自动分页
                </button>
              </div>
              {mode === "paged" && (
                <div className="rs-row">
                  <span className="rs-label">翻页方向</span>
                  <button
                    className={`rs-btn ${dir === "h" ? "on" : ""}`}
                    onClick={() => changeDir("h")}
                  >
                    左右翻页
                  </button>
                  <button
                    className={`rs-btn ${dir === "v" ? "on" : ""}`}
                    onClick={() => changeDir("v")}
                  >
                    上下翻页
                  </button>
                </div>
              )}
              {mode === "paged" && (
                <div className="rs-hint">
                  按屏幕大小自动分页；滑动或点按两侧翻页，页高对齐行栅格不切割文字。
                </div>
              )}
            </div>
          )}
        </div>
        <button className="chat-close reader-close" onClick={onClose} title="关闭">
          ×
        </button>
      </div>

      <div className="reader-body" ref={bodyRef}>
        {error ? (
          <div className="state reader-state">阅读器加载失败：{error}</div>
        ) : !content ? (
          <div className="state reader-state">正在打开书籍…</div>
        ) : (
          <iframe
            ref={iframeRef}
            className="reader-frame"
            title="阅读器"
            sandbox="allow-same-origin"
            srcDoc={layout.doc}
            onLoad={onFrameLoad}
          />
        )}
      </div>

      <div className="reader-foot">
        {mode === "paged" ? (
          <>
            <button className="btn small" onClick={() => turnPage(-1)} disabled={atFirst}>
              ‹ 上一页
            </button>
            <span className="reader-progress">
              {total > 0
                ? `${chapter + 1}/${total}章 · ${page + 1}/${pages}页`
                : "—"}
            </span>
          </>
        ) : (
          <>
            <button className="btn small" onClick={() => gotoChapter(chapter - 1)} disabled={atFirst}>
              ← 上一章
            </button>
            <span className="reader-progress">
              {total > 0 ? `${chapter + 1} / ${total} · ${pct}%` : "—"}
            </span>
          </>
        )}
        <div className="reader-fs">
          <button
            className="btn small"
            onClick={() => changeFont(-1)}
            disabled={fontSize <= FS_MIN}
            title="缩小字号"
          >
            A－
          </button>
          <span className="reader-fs-val">{fontSize}</span>
          <button
            className="btn small"
            onClick={() => changeFont(1)}
            disabled={fontSize >= FS_MAX}
            title="放大字号"
          >
            A＋
          </button>
        </div>
        {mode === "paged" ? (
          <button className="btn small" onClick={() => turnPage(1)} disabled={atLast}>
            下一页 ›
          </button>
        ) : (
          <button
            className="btn small"
            onClick={() => gotoChapter(chapter + 1)}
            disabled={!content || chapter >= total - 1}
          >
            下一章 →
          </button>
        )}
      </div>
    </div>
  );
}
