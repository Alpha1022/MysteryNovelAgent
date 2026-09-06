import { useState } from "react";
import { open } from "@tauri-apps/plugin-dialog";
import { translateText, uploadCover } from "../api/tauri";
import type { BookDetail } from "../types";
import CoverImage from "./CoverImage";

export interface MetaEditParams {
  title: string;
  author: string;
  tags: string;
  description: string | null;
  seriesName: string | null;
  seriesOrder: number | null;
}

interface Props {
  book: BookDetail;
  busy: boolean;
  error: string | null;
  /** LLM 可用性（翻译按钮显示条件） */
  llmReady?: boolean | null;
  onSave: (params: MetaEditParams) => void;
  onClose: () => void;
}

/** 详情页元数据编辑弹窗：封面 / 书名 / 作者 / 标签 / 简介（各字段可翻译为中文） */
export default function EditMetaModal({
  book,
  busy,
  error,
  llmReady,
  onSave,
  onClose,
}: Props) {
  const [title, setTitle] = useState(book.title);
  const [author, setAuthor] = useState(book.author);
  const [tagsStr, setTagsStr] = useState(book.tags);
  const [desc, setDesc] = useState(book.description ?? "");
  const [seriesName, setSeriesName] = useState(book.series_name ?? "");
  const [seriesOrder, setSeriesOrder] = useState<string>(
    book.series_order != null ? String(book.series_order) : "",
  );
  const [coverBusy, setCoverBusy] = useState(false);
  const [translating, setTranslating] = useState<string | null>(null);
  const [transError, setTransError] = useState<string | null>(null);

  /** 手动上传封面（复制入封面缓存 + 同步 EPUB 副本 + 更新数据库） */
  const onUploadCover = async () => {
    setTransError(null);
    const path = await open({
      multiple: false,
      title: "选择封面图片",
      filters: [{ name: "图片", extensions: ["jpg", "jpeg", "png", "webp"] }],
    }).catch(() => null);
    if (typeof path !== "string") return;
    setCoverBusy(true);
    try {
      await uploadCover(book.id, path);
      // 封面已更新，关闭弹窗让详情页刷新
      onClose();
    } catch (e) {
      setTransError(String(e));
    } finally {
      setCoverBusy(false);
    }
  };

  /** 翻译指定字段为简体中文（LLM） */
  const translateField = async (field: "title" | "author" | "desc") => {
    if (translating) return;
    const cur = field === "title" ? title : field === "author" ? author : desc;
    if (!cur.trim()) return;
    setTranslating(field);
    setTransError(null);
    try {
      const out = await translateText(cur);
      if (field === "title") setTitle(out);
      else if (field === "author") setAuthor(out);
      else setDesc(out);
    } catch (e) {
      setTransError(String(e));
    } finally {
      setTranslating(null);
    }
  };

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

  // 不启用"点击外部关闭"：避免拖拽选择文本时误触关闭
  return (
    <div className="modal-overlay">
      <div className="modal mid">
        <h2 className="modal-title">编 辑 元 数 据</h2>

        <div className="edit-layout">
          <div className="edit-side">
            <div className="edit-side-cover">
              <CoverImage coverPath={book.cover_path} title={book.title} />
            </div>
            <button
              className="btn small"
              onClick={onUploadCover}
              disabled={busy || coverBusy}
            >
              {coverBusy ? "上传中…" : "上传封面…"}
            </button>
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
                <TranslateBtn field="desc" />
              </div>
              <textarea
                value={desc}
                rows={7}
                onChange={(e) => setDesc(e.target.value)}
                disabled={busy}
              />
            </div>
          </div>
        </div>

        {(error || transError) && (
          <div className="modal-error">{transError ?? error}</div>
        )}

        <div className="modal-actions">
          <button className="btn" onClick={onClose} disabled={busy}>
            取消
          </button>
          <button
            className="btn primary"
            disabled={busy || !title.trim()}
            onClick={() =>
              onSave({
                title,
                author,
                tags: tagsStr,
                description: desc.trim() ? desc : null,
                seriesName: seriesName.trim() ? seriesName : null,
                seriesOrder: seriesName.trim() && seriesOrder.trim() ? Number(seriesOrder) : null,
              })
            }
          >
            {busy ? "保存中…" : "保 存"}
          </button>
        </div>
      </div>
    </div>
  );
}
