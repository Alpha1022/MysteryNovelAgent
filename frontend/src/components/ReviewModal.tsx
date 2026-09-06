import { useEffect, useRef, useState } from "react";
import { getComments, llmChat, saveBookReview } from "../api/tauri";
import type { ChatMessage } from "../types";

export interface ReviewTarget {
  id: number;
  title: string;
  author: string;
  tags: string;
}

interface Props {
  target: ReviewTarget;
  /** changed=true 表示已保存书评（调用方刷新数据） */
  onClose: (changed: boolean) => void;
}

type Mode = "choose" | "chat" | "manual";

interface ThreadItem {
  role: "assistant" | "user";
  content: string;
}

/** 短评弹窗：标记已读后询问填写方式 —— AI 对话生成 / 手动填写 / 暂不 */
export default function ReviewModal({ target, onClose }: Props) {
  const [mode, setMode] = useState<Mode>("choose");

  return (
    <div className="modal-overlay">
      <div className="modal">
        <h2 className="modal-title">标 记 已 读</h2>
        <div className="modal-file">《{target.title}》已标记为已读</div>

        {mode === "choose" && (
          <>
            <div className="review-ask">要为这本书写一条短评吗？</div>
            <div className="choose-col">
              <button className="btn primary wide-btn" onClick={() => setMode("chat")}>
                与 AI 对话讨论后生成
              </button>
              <button className="btn wide-btn" onClick={() => setMode("manual")}>
                手动填写短评
              </button>
              <button className="btn wide-btn" onClick={() => onClose(false)}>
                暂 不
              </button>
            </div>
          </>
        )}

        {mode === "chat" && (
          <ChatReview target={target} onBack={() => setMode("choose")} onClose={onClose} />
        )}

        {mode === "manual" && (
          <ManualReview target={target} onClose={onClose} />
        )}
      </div>
    </div>
  );
}

/** 参考短评（豆瓣 top5，与 CLI finish 一致） */
async function fetchRefComments(bookId: number): Promise<string[]> {
  try {
    const all = await getComments(bookId);
    return all
      .filter((c) => c.is_mine !== 1)
      .sort((a, b) => b.usefulness - a.usefulness)
      .slice(0, 5)
      .map((c) => c.content);
  } catch {
    return [];
  }
}

function buildSystemPrompt(t: ReviewTarget, comments: string[]): string {
  const commentsText =
    comments.length === 0
      ? "（暂无参考短评）"
      : comments.map((c, i) => `${i + 1}. ${c}`).join("\n");
  return `你是一位资深推理小说评论家，正在帮助读者复盘刚读完的《${t.title}》（作者：${t.author || "佚名"}）。
这本书的标签是：${t.tags || "推理小说"}。
以下是其他读者的一些观点（供参考，若用户提及可与其探讨）：
${commentsText}

你的任务：
1. 通过提问引导用户说出对这本书的感受、对诡计/反转的评价、对人物的看法。
2. 问题要具体（例如："你觉得凶手的动机是否合理？""哪个场景让你最毛骨悚然？""你对这个结局满意吗？"）。
3. 当用户表达观点后，可以结合参考短评进行追问（"有读者认为节奏拖沓，你认同吗？"）。
4. 保持对话自然，每次回复控制在 100 字以内。
5. 当用户表示想结束时，输出一段基于本次对话的总结性短评。`;
}

const OPENING_PROMPT = "请用一句话开场，引导我开始聊聊这本书。";
const REVIEW_PROMPT =
  '基于刚才我们所有的对话，请以第一人称"我"的口吻，写一篇 150~300 字的短评。要求逻辑清晰，包含对故事核心悬念或人物的评价，情感真挚。';

/** AI 对话式短评生成 */
function ChatReview({
  target,
  onBack,
  onClose,
}: {
  target: ReviewTarget;
  onBack: () => void;
  onClose: (changed: boolean) => void;
}) {
  const [thread, setThread] = useState<ThreadItem[]>([]);
  const [input, setInput] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [model, setModel] = useState<string | null>(null);
  const [review, setReview] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const historyRef = useRef<ChatMessage[]>([]);
  const bootRef = useRef(false);
  const threadEndRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (bootRef.current) return;
    bootRef.current = true;
    (async () => {
      const comments = await fetchRefComments(target.id);
      historyRef.current = [
        { role: "system", content: buildSystemPrompt(target, comments) },
        { role: "user", content: OPENING_PROMPT },
      ];
      await send(false);
    })();
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    threadEndRef.current?.scrollIntoView({ block: "end" });
  }, [thread, review]);

  const send = async (fromInput: boolean) => {
    if (busy) return;
    setBusy(true);
    setError(null);
    try {
      const reply = await llmChat(historyRef.current);
      historyRef.current.push({ role: "assistant", content: reply.content });
      setModel(reply.model);
      setThread((prev) => [...prev, { role: "assistant", content: reply.content }]);
      if (fromInput) setInput("");
    } catch (e) {
      // 失败时移除未送达的用户消息，允许重试
      if (fromInput) historyRef.current.pop();
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const onSend = async () => {
    const text = input.trim();
    if (!text || busy || review != null) return;
    historyRef.current.push({ role: "user", content: text });
    setThread((prev) => [...prev, { role: "user", content: text }]);
    await send(true);
  };

  const onGenerate = async () => {
    if (busy || review != null) return;
    historyRef.current.push({ role: "user", content: REVIEW_PROMPT });
    setThread((prev) => [...prev, { role: "user", content: "（请为我生成总结性短评）" }]);
    setBusy(true);
    setError(null);
    try {
      const reply = await llmChat(historyRef.current);
      historyRef.current.push({ role: "assistant", content: reply.content });
      setModel(reply.model);
      setReview(reply.content);
    } catch (e) {
      historyRef.current.pop();
      setError(String(e));
    } finally {
      setBusy(false);
    }
  };

  const onSave = async () => {
    const text = review?.trim();
    if (!text || saving) return;
    setSaving(true);
    setError(null);
    try {
      await saveBookReview(target.id, text);
      onClose(true);
    } catch (e) {
      setError(String(e));
      setSaving(false);
    }
  };

  return (
    <>
      <div className="chat-model">{model ? `模型：${model}` : "连接中…"}</div>
      <div className="chat-thread">
        {thread.map((m, i) => (
          <div key={i} className={`bubble ${m.role}`}>{m.content}</div>
        ))}
        {busy && <div className="bubble assistant typing">…</div>}
        <div ref={threadEndRef} />
      </div>

      {review != null ? (
        <>
          <label className="modal-label">
            生成的短评（可修改）
            <textarea
              rows={5}
              value={review}
              onChange={(e) => setReview(e.target.value)}
            />
          </label>
          <div className="modal-actions">
            <button className="btn" onClick={() => setReview(null)} disabled={saving}>
              继续对话
            </button>
            <button className="btn primary" onClick={onSave} disabled={saving || !review.trim()}>
              {saving ? "保存中…" : "保存书评"}
            </button>
          </div>
        </>
      ) : (
        <>
          <div className="chat-input-row">
            <input
              value={input}
              placeholder="说说你的感受…"
              onChange={(e) => setInput(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") onSend();
              }}
              disabled={busy}
            />
            <button className="btn" onClick={onSend} disabled={busy || !input.trim()}>
              发送
            </button>
          </div>
          {error && <div className="modal-error">{error}</div>}
          <div className="modal-actions">
            <button className="btn" onClick={onBack} disabled={busy}>
              返回
            </button>
            <button className="btn primary" onClick={onGenerate} disabled={busy || thread.length === 0}>
              生成书评
            </button>
          </div>
        </>
      )}
    </>
  );
}

/** 手动填写短评（标记已读流程内） */
function ManualReview({
  target,
  onClose,
}: {
  target: ReviewTarget;
  onClose: (changed: boolean) => void;
}) {
  const [text, setText] = useState("");
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const onSave = async () => {
    if (!text.trim() || saving) return;
    setSaving(true);
    setError(null);
    try {
      await saveBookReview(target.id, text);
      onClose(true);
    } catch (e) {
      setError(String(e));
      setSaving(false);
    }
  };

  return (
    <>
      <label className="modal-label">
        我的短评
        <textarea
          rows={6}
          value={text}
          placeholder="记录下你对这本书的想法…"
          onChange={(e) => setText(e.target.value)}
        />
      </label>
      {error && <div className="modal-error">{error}</div>}
      <div className="modal-actions">
        <button className="btn" onClick={() => onClose(false)} disabled={saving}>
          取消
        </button>
        <button className="btn primary" onClick={onSave} disabled={saving || !text.trim()}>
          {saving ? "保存中…" : "保存书评"}
        </button>
      </div>
    </>
  );
}

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
