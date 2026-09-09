import { useEffect, useRef, useState } from "react";
import { listen } from "@tauri-apps/api/event";
import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";
import { cancelTask, chatbotChat } from "../api/tauri";
import { readTextFile, writeTextFile } from "../api/picker";
import { renderMarkdown } from "../utils/markdown";
import type { ChatMessage, ChatSession, ChatStep } from "../types";

/**
 * 书虫 · 阅读助手：右下角悬浮入口。
 * - 回复内容支持基础 Markdown 渲染
 * - 展示 Agent 实际工具调用轨迹（工具名 / 实参 / 结果，可折叠）
 * - 多会话管理：历史任务可查看、切换、删除；会话可导出/导入 JSON（完整上下文）
 */

const SESSIONS_KEY = "mna-chat-sessions";
const CURRENT_KEY = "mna-chat-current";

function loadSessions(): ChatSession[] {
  try {
    const raw = localStorage.getItem(SESSIONS_KEY);
    if (raw) {
      const list = JSON.parse(raw) as ChatSession[];
      if (Array.isArray(list)) return list;
    }
  } catch {
    // 历史损坏时从空白开始
  }
  return [];
}

function saveSessions(list: ChatSession[]) {
  try {
    localStorage.setItem(SESSIONS_KEY, JSON.stringify(list));
  } catch {
    // 存储不可用时静默
  }
}

function newSession(): ChatSession {
  return {
    id: `chat-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`,
    title: "新会话",
    savedAt: Date.now(),
    messages: [],
  };
}

/** 工具调用轨迹列表（消息气泡与生成中的实时展示共用） */
function StepsList({ steps }: { steps: ChatStep[] }) {
  return (
    <div className="chat-steps">
      {steps.map((s, i) => (
        <details key={i} className="chat-step" open={i === steps.length - 1}>
          <summary>
            <span className="chat-step-tool">🔧 {s.tool}</span>
          </summary>
          <div className="chat-step-body">
            <div className="chat-step-label">实参</div>
            <pre>{s.args}</pre>
            <div className="chat-step-label">结果</div>
            <pre>{s.result}</pre>
          </div>
        </details>
      ))}
    </div>
  );
}

/** 单条消息气泡（assistant 附带工具轨迹 + Markdown 渲染） */
function MessageBubble({ m }: { m: ChatMessage }) {
  const steps = m.role === "assistant" ? (m.steps ?? []) : [];
  return (
    <div className={`chat-msg ${m.role}`}>
      <div className="chat-bubble">
        {steps.length > 0 && <StepsList steps={steps} />}
        <div
          className="chat-md"
          dangerouslySetInnerHTML={{
            __html: renderMarkdown(m.content),
          }}
        />
      </div>
    </div>
  );
}

export default function Chatbot() {
  const [open, setOpen] = useState(false);
  const [sessions, setSessions] = useState<ChatSession[]>(loadSessions);
  const [currentId, setCurrentId] = useState<string>(
    () => localStorage.getItem(CURRENT_KEY) ?? "",
  );
  const [showHistory, setShowHistory] = useState(false);
  const [input, setInput] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  /** 生成中的实时工具调用轨迹（chat-progress 事件推送） */
  const [liveSteps, setLiveSteps] = useState<ChatStep[]>([]);
  const listRef = useRef<HTMLDivElement>(null);
  const chatTaskIdRef = useRef<string>("");

  // 订阅书虫 agent 的实时工具轨迹
  useEffect(() => {
    const un = listen<{ steps: ChatStep[] }>("chat-progress", (e) => {
      setLiveSteps(e.payload.steps ?? []);
    });
    return () => {
      un.then((fn) => fn()).catch(() => undefined);
    };
  }, []);

  // 当前会话（无则懒创建）
  const current =
    sessions.find((s) => s.id === currentId) ?? sessions[0] ?? null;

  /** 按 id 修补会话并持久化 */
  const mutateSession = (id: string, patch: (s: ChatSession) => ChatSession) => {
    setSessions((prev) => {
      const list = prev.map((s) => (s.id === id ? patch(s) : s));
      saveSessions(list);
      return list;
    });
  };

  useEffect(() => {
    if (current) localStorage.setItem(CURRENT_KEY, current.id);
  }, [current?.id]);

  // 新消息自动滚动到底部（生成中的实时工具轨迹到达时同样滚动）
  useEffect(() => {
    listRef.current?.scrollTo({ top: listRef.current.scrollHeight });
  }, [current?.messages, busy, showHistory, liveSteps.length]);

  const send = async () => {
    const text = input.trim();
    if (!text || busy) return;
    // 无会话时先创建（保证后续回复写入同一会话）
    let base = current;
    if (!base) {
      base = newSession();
      setSessions((prev) => {
        const list = [base!, ...prev];
        saveSessions(list);
        return list;
      });
      setCurrentId(base.id);
    }
    const sid = base.id;
    const next: ChatMessage[] = [
      ...base.messages,
      { role: "user", content: text },
    ];
    mutateSession(sid, (s) => ({
      ...s,
      messages: next,
      // 标题取首条用户消息
      title: s.messages.length === 0 ? text.slice(0, 24) : s.title,
      savedAt: Date.now(),
    }));
    setInput("");
    setBusy(true);
    setError(null);
    setNotice(null);
    setLiveSteps([]);
    const taskId = `chat-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`;
    chatTaskIdRef.current = taskId;
    try {
      const history = next.filter(
        (m) => m.role === "user" || m.role === "assistant",
      );
      const res = await chatbotChat(history, taskId);
      mutateSession(sid, (s) => ({
        ...s,
        messages: [
          ...s.messages,
          {
            role: "assistant",
            content: res.content,
            steps: res.steps ?? [],
          },
        ],
        savedAt: Date.now(),
      }));
    } catch (e) {
      // 打断不算错误：给出友好提示
      if (String(e).includes("已取消") || String(e).includes("任务已取消")) {
        setNotice("已打断本次思考");
      } else {
        setError(String(e));
      }
    } finally {
      setLiveSteps([]);
      setBusy(false);
    }
  };

  /** 打断书虫的思考过程（中止进行中的 LLM 请求与后续工具调用） */
  const onChatCancel = () => {
    if (chatTaskIdRef.current) void cancelTask(chatTaskIdRef.current);
  };

  const onCreate = () => {
    const s = newSession();
    setSessions((prev) => {
      const list = [s, ...prev];
      saveSessions(list);
      return list;
    });
    setCurrentId(s.id);
    setShowHistory(false);
  };

  const onLoad = (id: string) => {
    setCurrentId(id);
    setShowHistory(false);
  };

  const onDelete = (id: string) => {
    setSessions((prev) => {
      const list = prev.filter((s) => s.id !== id);
      saveSessions(list);
      return list;
    });
    if (currentId === id) setCurrentId("");
  };

  const onExport = async () => {
    if (!current || current.messages.length === 0) {
      setNotice("当前会话为空，无可导出内容");
      return;
    }
    const date = new Date().toISOString().slice(0, 10);
    const path = await saveDialog({
      title: "导出会话",
      defaultPath: `书虫会话-${current.title || date}-${date}.json`,
      filters: [{ name: "JSON", extensions: ["json"] }],
    }).catch(() => null);
    if (typeof path !== "string") return;
    try {
      await writeTextFile(
        path,
        JSON.stringify({ version: 1, session: current }, null, 2),
      );
      setNotice(`会话已导出：${path}`);
    } catch (e) {
      setError(String(e));
    }
  };

  const onImport = async () => {
    const path = await openDialog({
      multiple: false,
      title: "导入会话 JSON",
      filters: [{ name: "JSON", extensions: ["json"] }],
    }).catch(() => null);
    if (typeof path !== "string") return;
    try {
      const raw = await readTextFile(path);
      const parsed = JSON.parse(raw) as { session?: ChatSession };
      const s = parsed.session;
      if (!s || !Array.isArray(s.messages)) {
        throw new Error("文件格式不正确（缺少 session.messages）");
      }
      const session: ChatSession = {
        ...newSession(),
        ...s,
        id: `chat-${Date.now()}-${Math.random().toString(36).slice(2, 8)}`,
      };
      setSessions((prev) => {
        const list = [session, ...prev];
        saveSessions(list);
        return list;
      });
      setCurrentId(session.id);
      setNotice(`已导入会话「${session.title}」（${session.messages.length} 条消息）`);
    } catch (e) {
      setError(`导入失败：${String(e)}`);
    }
  };

  const messages = current?.messages ?? [];

  return (
    <>
      {open && (
        <div className="chat-panel">
          <div className="chat-head">
            <span className="chat-title">书 虫 · 阅 读 助 手</span>
            <span className="bulk-flex" />
            <button
              className="chat-tool-btn"
              onClick={onCreate}
              title="开始新会话"
            >
              新会话
            </button>
            <button
              className="chat-tool-btn"
              onClick={() => setShowHistory((h) => !h)}
              title="查看历史会话"
            >
              历史 {sessions.length > 0 ? `(${sessions.length})` : ""}
            </button>
            <button className="chat-tool-btn" onClick={onExport} title="导出当前会话为 JSON">
              导出
            </button>
            <button className="chat-tool-btn" onClick={onImport} title="从 JSON 导入会话">
              导入
            </button>
            <button className="chat-close" onClick={() => setOpen(false)} title="收起">
              ×
            </button>
          </div>

          {showHistory && (
            <div className="chat-history">
              {sessions.length === 0 ? (
                <div className="chat-empty">暂无历史会话</div>
              ) : (
                sessions.map((s) => (
                  <div
                    key={s.id}
                    className={`chat-history-item ${s.id === current?.id ? "active" : ""}`}
                    onClick={() => onLoad(s.id)}
                  >
                    <div className="chat-history-title">
                      {s.title || "未命名会话"}
                      {s.id === current?.id && <span className="subtab-dot">●</span>}
                    </div>
                    <div className="chat-history-meta">
                      {new Date(s.savedAt).toLocaleString()} · {s.messages.length} 条
                    </div>
                    <button
                      className="chat-history-del"
                      title="删除该会话"
                      onClick={(e) => {
                        e.stopPropagation();
                        onDelete(s.id);
                      }}
                    >
                      ×
                    </button>
                  </div>
                ))
              )}
            </div>
          )}

          <div className="chat-list" ref={listRef}>
            {messages.length === 0 && !busy && (
              <div className="chat-empty">
                你好，我是书虫 📚
                <br />
                可以问我读了什么、让我推荐下一本书，或聊聊书架上的任何一部作品。
              </div>
            )}
            {messages.map((m, i) => (
              <MessageBubble key={i} m={m} />
            ))}
            {busy && (
              <div className="chat-msg assistant">
                <div className="chat-bubble">
                  {liveSteps.length > 0 && <StepsList steps={liveSteps} />}
                  <div className="chat-thinking-row">
                    <span className="chat-thinking-dots">
                      <span />
                      <span />
                      <span />
                    </span>
                    <button
                      className="chat-cancel"
                      onClick={onChatCancel}
                      title="打断思考过程（中止进行中的请求与工具调用）"
                    >
                      打断
                    </button>
                  </div>
                </div>
              </div>
            )}
          </div>

          {notice && <div className="chat-notice">{notice}</div>}
          {error && <div className="chat-error">{error}</div>}

          <div className="chat-input-row">
            <textarea
              value={input}
              rows={2}
              placeholder="输入消息，Enter 发送…"
              onChange={(e) => setInput(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter" && !e.shiftKey) {
                  e.preventDefault();
                  void send();
                }
              }}
              disabled={busy}
            />
            <button
              className="btn primary chat-send"
              onClick={() => void send()}
              disabled={busy || !input.trim()}
            >
              发送
            </button>
          </div>
        </div>
      )}

      <button
        className={`chat-fab ${open ? "open" : ""}`}
        onClick={() => setOpen((o) => !o)}
        title={open ? "收起助手" : "阅读助手"}
      >
        {open ? "×" : "🤖"}
      </button>
    </>
  );
}
