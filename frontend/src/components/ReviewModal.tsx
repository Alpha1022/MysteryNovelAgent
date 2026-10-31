import { useState } from "react";
import { saveBookReview } from "../api/tauri";

/** 详情页"我的书评"编辑弹窗（initial 为现有书评，可为空串表示新写） */
export function ReviewEditModal({
  bookId,
  initial,
  onClose,
}: {
  bookId: number;
  initial: string;
  onClose: (changed: boolean) => void;
}) {
  const [text, setText] = useState(initial);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const onSave = async () => {
    if (!text.trim() || saving) return;
    setSaving(true);
    setError(null);
    try {
      await saveBookReview(bookId, text);
      onClose(true);
    } catch (e) {
      setError(String(e));
      setSaving(false);
    }
  };

  return (
    <div className="modal-overlay">
      <div className="modal" onClick={(e) => e.stopPropagation()}>
        <h2 className="modal-title">编 辑 书 评</h2>
        <label className="modal-label">
          我的短评
          <textarea
            rows={7}
            value={text}
            placeholder="记录下你对这本书的想法…"
            onChange={(e) => setText(e.target.value)}
            autoFocus
          />
        </label>
        {error && <div className="modal-error">{error}</div>}
        <div className="modal-actions">
          <button className="btn" onClick={() => onClose(false)} disabled={saving}>
            取消
          </button>
          <button className="btn primary" onClick={onSave} disabled={saving || !text.trim()}>
            {saving ? "保存中…" : "保 存"}
          </button>
        </div>
      </div>
    </div>
  );
}
