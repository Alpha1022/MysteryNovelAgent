import { useEffect, useRef, useState } from "react";
import {
  createFsDir,
  fsRoots,
  listFsDir,
  registerBrowserHost,
  type FsEntry,
  type PickRequest,
} from "../api/picker";

/** 格式化文件大小（浏览器列表展示） */
function fmtSize(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`;
}

/** 路径面包屑分段（兼容 / 与 \） */
function segments(path: string): string[] {
  return path.split(/[\\/]+/).filter(Boolean);
}

/**
 * 内置文件/目录浏览器（移动端替代系统对话框）：
 * - dir 模式：底部"选择当前目录"确认
 * - file 模式：点击 .epub 文件直接选定（其他文件置灰）
 */
export default function PathPickerHost() {
  const [req, setReq] = useState<PickRequest | null>(null);
  const [path, setPath] = useState("");
  const [entries, setEntries] = useState<FsEntry[]>([]);
  const [roots, setRoots] = useState<string[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  // 新建文件夹
  const [mkdirOpen, setMkdirOpen] = useState(false);
  const [mkdirName, setMkdirName] = useState("");
  const [mkdirBusy, setMkdirBusy] = useState(false);
  const reqRef = useRef<PickRequest | null>(null);
  reqRef.current = req;

  useEffect(() => {
    const off = registerBrowserHost((r) => {
      setReq(r);
    });
    return off;
  }, []);

  // 打开时初始化根目录与起始路径
  useEffect(() => {
    if (!req) return;
    setError(null);
    fsRoots()
      .then((rs) => {
        setRoots(rs);
        setPath((cur) => (cur && reqRef.current === req ? cur : rs[0] ?? "/"));
      })
      .catch((e) => setError(String(e)));
  }, [req]);

  // 路径变化加载目录内容
  useEffect(() => {
    if (!req || !path) return;
    let cancelled = false;
    setLoading(true);
    setError(null);
    listFsDir(path)
      .then((list) => {
        if (!cancelled) setEntries(list);
      })
      .catch((e) => {
        if (!cancelled) {
          setEntries([]);
          setError(String(e));
        }
      })
      .finally(() => {
        if (!cancelled) setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [req, path]);

  if (!req) return null;

  const close = (picked: string | null) => {
    req.resolve(picked);
    setReq(null);
    setEntries([]);
  };

  const onMkdir = async () => {
    const name = mkdirName.trim();
    if (!name || mkdirBusy) return;
    setMkdirBusy(true);
    setError(null);
    try {
      const created = await createFsDir(path, name);
      setMkdirOpen(false);
      setMkdirName("");
      // 刷新并进入新目录
      const list = await listFsDir(path);
      setEntries(list);
      setPath(created);
    } catch (e) {
      setError(String(e));
    } finally {
      setMkdirBusy(false);
    }
  };

  const crumbs = segments(path);
  const isFileMode = req.mode === "file";

  return (
    <div className="modal-overlay nested">
      <div className="modal fs-browser-modal">
        <h2 className="modal-title">{req.title}</h2>

        {/* 根目录快捷切换 */}
        <div className="fs-roots">
          {roots.map((r) => (
            <button
              key={r}
              className={`fs-root ${path === r ? "active" : ""}`}
              onClick={() => setPath(r)}
              title={r}
            >
              {r === "/storage/emulated/0" ? "内部存储" : r}
            </button>
          ))}
        </div>

        {/* 当前路径 + 上一级 + 新建文件夹 */}
        <div className="fs-path-row">
          <button
            className="btn small"
            onClick={() => {
              const parent = crumbs.slice(0, -1).join("/");
              setPath(parent ? `/${parent}` : "/");
            }}
            disabled={crumbs.length <= 1}
          >
            上一级
          </button>
          <span className="fs-path" title={path}>
            {path}
          </span>
          <button
            className="btn small"
            onClick={() => {
              setMkdirOpen((o) => !o);
              setMkdirName("");
            }}
            title="在当前目录新建文件夹"
          >
            ＋ 新建文件夹
          </button>
        </div>

        {mkdirOpen && (
          <div className="fs-mkdir-row">
            <input
              value={mkdirName}
              autoFocus
              placeholder="文件夹名称…"
              onChange={(e) => setMkdirName(e.target.value)}
              onKeyDown={(e) => {
                if (e.key === "Enter") void onMkdir();
                if (e.key === "Escape") setMkdirOpen(false);
              }}
              disabled={mkdirBusy}
            />
            <button className="btn small primary" onClick={() => void onMkdir()} disabled={mkdirBusy || !mkdirName.trim()}>
              {mkdirBusy ? "创建中…" : "创建并进入"}
            </button>
          </div>
        )}

        <div className="fs-list">
          {loading ? (
            <div className="state">加载中…</div>
          ) : error ? (
            <div className="state error-state">
              {error}
              <div className="form-hint">
                若为存储权限问题，请在系统设置中授予本应用「所有文件访问」权限
              </div>
            </div>
          ) : entries.length === 0 ? (
            <div className="state">空目录</div>
          ) : (
            entries.map((e) => {
              const pickable = !isFileMode || (!e.is_dir && e.name.toLowerCase().endsWith(".epub"));
              return (
                <button
                  key={e.path}
                  className={`fs-entry ${e.is_dir ? "dir" : "file"} ${pickable ? "" : "disabled"}`}
                  onClick={() => {
                    if (e.is_dir) setPath(e.path);
                    else if (pickable) close(e.path);
                  }}
                  disabled={!pickable && !e.is_dir}
                >
                  <span className="fs-icon">{e.is_dir ? "📁" : "📄"}</span>
                  <span className="fs-name">{e.name}</span>
                  {e.size != null && <span className="fs-size">{fmtSize(e.size)}</span>}
                </button>
              );
            })
          )}
        </div>

        <div className="modal-actions">
          <button className="btn" onClick={() => close(null)}>
            取 消
          </button>
          {!isFileMode && (
            <button className="btn primary" onClick={() => close(path)}>
              选择此目录
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
