import { useEffect, useState } from "react";
import { useNavigate, useParams } from "react-router-dom";
import {
  deleteBook,
  getBookDetail,
  getComments,
  listSources,
  llmStatus,
  openBookFile,
  setBookStatus,
  updateBookMeta,
} from "../api/tauri";
import type { BookDetail, CommentRow } from "../types";
import CoverImage from "../components/CoverImage";
import CommentItem, { type CommentSourceInfo } from "../components/CommentItem";
import StateView from "../components/StateView";
import EditMetaModal, { type MetaEditParams } from "../components/EditMetaModal";
import ReaderModal from "../components/ReaderModal";
import ReviewModal, { ReviewEditModal, type ReviewTarget } from "../components/ReviewModal";
import StatusBadge from "../components/StatusBadge";
import SourcesModal from "../components/SourcesModal";
import { useConfirm } from "../components/ConfirmDialog";

export default function BookDetailPage() {
  const { confirm, confirmElement } = useConfirm();
  const { id } = useParams();
  const navigate = useNavigate();
  const bookId = Number(id);

  const [book, setBook] = useState<BookDetail | null>(null);
  const [comments, setComments] = useState<CommentRow[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [deleteBusy, setDeleteBusy] = useState(false);
  const [metaBusy, setMetaBusy] = useState(false);
  const [editing, setEditing] = useState(false);
  const [sourcesOpen, setSourcesOpen] = useState(false);
  const [readerOpen, setReaderOpen] = useState(false);
  const [metaError, setMetaError] = useState<string | null>(null);
  const [actionError, setActionError] = useState<string | null>(null);
  const [reviewTarget, setReviewTarget] = useState<ReviewTarget | null>(null);
  const [reviewEditing, setReviewEditing] = useState(false);
  const [refresh, setRefresh] = useState(0);
  const [llmReady, setLlmReady] = useState<boolean | null>(null);

  useEffect(() => {
    llmStatus()
      .then((s) => setLlmReady(s.configured))
      .catch(() => setLlmReady(false));
  }, [refresh]);

  // 来源项目映射（短评悬停展示来源封面与书名）
  const [sourceMap, setSourceMap] = useState<Map<string, CommentSourceInfo>>(new Map());
  useEffect(() => {
    if (!Number.isFinite(bookId)) return;
    let cancelled = false;
    listSources(bookId)
      .then((list) => {
        if (cancelled) return;
        const m = new Map<string, CommentSourceInfo>();
        for (const s of list) {
          m.set(s.ref_key, {
            kind: s.kind,
            title: s.title || s.ref_key,
            cover_path: s.cover_path,
          });
        }
        setSourceMap(m);
      })
      .catch(() => undefined);
    return () => {
      cancelled = true;
    };
  }, [bookId, refresh]);

  useEffect(() => {
    if (!Number.isFinite(bookId)) {
      setError("无效的书籍 ID");
      setLoading(false);
      return;
    }
    let cancelled = false;
    setLoading(true);
    Promise.all([getBookDetail(bookId), getComments(bookId)])
      .then(([b, cs]) => {
        if (cancelled) return;
        setBook(b);
        setComments(cs);
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
  }, [bookId, refresh]);

  /** 切换阅读状态；切到"已读"时弹出短评询问 */
  const onStatusChange = async (newStatus: string) => {
    if (!book) return;
    setActionError(null);
    try {
      await setBookStatus(book.id, newStatus);
      setBook({ ...book, status: newStatus });
      if (newStatus === "已读") {
        setReviewTarget({
          id: book.id,
          title: book.title,
          author: book.author,
          tags: book.tags,
        });
      }
    } catch (e) {
      setActionError(String(e));
    }
  };

  if (loading && !book) {
    return (
      <div className="page">
        <StateView kind="loading" />
      </div>
    );
  }
  if (!book) {
    return (
      <div className="page">
        <StateView kind="error" message={error ?? "未找到该书"} />
      </div>
    );
  }

  /** 打开本地 EPUB（书库副本优先，原始文件兜底） */
  const onOpenFile = () => {
    setActionError(null);
    openBookFile(book.id).catch((e) => setActionError(String(e)));
  };

  /** 打开内置阅读器 */
  const onRead = () => {
    setActionError(null);
    setReaderOpen(true);
  };

  /** 删除书籍（应用内确认框；书库 EPUB 删除，封面按引用计数清理，原始文件不动） */
  const onDelete = async () => {
    setActionError(null);
    const ok = await confirm({
      title: "删除确认",
      message: `确定将《${book.title}》移出书架吗？\n\n书库中的 EPUB 文件将删除，封面缓存按引用计数清理（原始导入文件不受影响）。`,
      okLabel: "删除",
      danger: true,
    });
    if (!ok) return;
    setDeleteBusy(true);
    try {
      await deleteBook(book.id);
      navigate("/", { replace: true });
    } catch (e) {
      setActionError(String(e));
      setDeleteBusy(false);
    }
  };

  /** 元数据编辑保存（DB + EPUB 书库副本同步，系列一并保存） */
  const onSaveMeta = async (p: MetaEditParams) => {
    setMetaBusy(true);
    setMetaError(null);
    try {
      await updateBookMeta(
        book.id,
        p.title,
        p.author,
        p.tags,
        p.description,
        p.seriesName,
        p.seriesOrder,
      );
      setBook({
        ...book,
        title: p.title.trim(),
        author: p.author,
        tags: p.tags,
        description: p.description,
        series_name: p.seriesName,
        series_order: p.seriesOrder,
      });
      setEditing(false);
    } catch (e) {
      setMetaError(String(e));
    } finally {
      setMetaBusy(false);
    }
  };

  const tags = book.tags
    ? book.tags
        .split(/[,，]/)
        .map((s) => s.trim())
        .filter(Boolean)
    : [];
  const netCount = comments.filter((c) => c.is_mine !== 1).length;

  return (
    <div className="page">
      <button className="back" onClick={() => navigate(-1)}>
        ← 返回书架
      </button>

      <div className="detail-top">
        <div className="detail-cover">
          <CoverImage coverPath={book.cover_path} title={book.title} />
        </div>
        <div>
          <h1 className="detail-title">{book.title}</h1>
          <div className="detail-author">{book.author || "佚名"}</div>
          <div className="detail-badges">
            <StatusBadge status={book.status} onChange={onStatusChange} />
            {book.series_name && (
              <span className="tag-chip">
                {book.series_name}
                {book.series_order != null ? ` #${book.series_order}` : ""}
              </span>
            )}
            {book.merged_from && <span className="tag-chip">合并本</span>}
            {tags.map((t) => (
              <span key={t} className="tag-chip">
                {t}
              </span>
            ))}
          </div>
          {book.status === "已读" && book.finished_date && (
            <div className="review-meta">读完于 {book.finished_date}</div>
          )}
          <div className="action-bar">
            <button className="link-btn primary-link" onClick={onRead}>
              阅 读
            </button>
            <button className="link-btn" onClick={() => setEditing(true)}>
              编辑元数据
            </button>
            <button
              className="link-btn"
              onClick={() => setSourcesOpen(true)}
              title="管理 claspclub / 豆瓣来源：排序、设为封面、设为简介、重抓短评"
            >
              来源管理
            </button>
            <button className="link-btn" onClick={onOpenFile}>
              打开本地文件 ↗
            </button>
            <button
              className="link-btn danger"
              onClick={onDelete}
              disabled={deleteBusy}
            >
              {deleteBusy ? "删除中…" : "删除本书"}
            </button>
          </div>
          {actionError && <div className="action-error">{actionError}</div>}
        </div>
      </div>

      {book.description && (
        <section className="section">
          <h2>简 介</h2>
          <div className="description">{book.description}</div>
        </section>
      )}

      <section className="section">
        <h2>
          网 络 短 评 <span className="count">{netCount} 条</span>
        </h2>
        {comments.length === 0 ? (
          <div className="state">暂无短评</div>
        ) : (
          comments.map((c) => <CommentItem key={c.id} c={c} sourceMap={sourceMap} />)
        )}
      </section>

      {(book.my_review && book.my_review.trim()) || book.status === "已读" ? (
        <section className="section">
          <h2>
            我 的 书 评
            <button
              className="link-btn small-link"
              onClick={() => setReviewEditing(true)}
            >
              {book.my_review && book.my_review.trim() ? "编辑" : "写书评"}
            </button>
          </h2>
          {book.my_review && book.my_review.trim() ? (
            <div className="review">{book.my_review}</div>
          ) : (
            <div className="state-inline">尚未填写书评，点击上方"写书评"记录你的想法</div>
          )}
        </section>
      ) : null}

      {editing && (
        <EditMetaModal
          book={book}
          busy={metaBusy}
          error={metaError}
          llmReady={llmReady}
          onSave={onSaveMeta}
          onClose={(changed) => {
            if (!metaBusy) {
              setEditing(false);
              // changed=true：封面已上传更新（DB 已换新 cover_path），刷新详情
              if (changed) setRefresh((k) => k + 1);
            }
          }}
        />
      )}

      {sourcesOpen && (
        <SourcesModal
          bookId={book.id}
          bookTitle={book.title}
          onClose={(changed) => {
            setSourcesOpen(false);
            if (changed) setRefresh((k) => k + 1);
          }}
        />
      )}

      {readerOpen && (
        <ReaderModal
          bookId={book.id}
          title={book.title}
          author={book.author}
          onClose={() => setReaderOpen(false)}
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

      {reviewEditing && (
        <ReviewEditModal
          bookId={book.id}
          initial={book.my_review ?? ""}
          onClose={(changed) => {
            setReviewEditing(false);
            if (changed) setRefresh((k) => k + 1);
          }}
        />
      )}
      {confirmElement}
    </div>
  );
}
