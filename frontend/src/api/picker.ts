import { invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";

/** 选择请求（内置浏览器弹窗的模块级总线） */
export interface PickRequest {
  mode: "dir" | "file";
  title: string;
  resolve: (path: string | null) => void;
}

let browserListener: ((req: PickRequest) => void) | null = null;
let mobileCache: boolean | null = null;

/** 是否移动端（决定用系统原生对话框还是内置浏览器；结果缓存） */
export async function isMobilePlatform(): Promise<boolean> {
  if (mobileCache == null) {
    mobileCache = await invoke<boolean>("is_mobile").catch(() => false);
  }
  return mobileCache;
}

/** 注册内置浏览器宿主（App 挂载 PathPickerHost 时调用；返回注销函数） */
export function registerBrowserHost(
  fn: (req: PickRequest) => void,
): () => void {
  browserListener = fn;
  return () => {
    if (browserListener === fn) browserListener = null;
  };
}

/** 经内置浏览器选择（未挂载宿主时返回 null） */
function requestBrowserPick(req: Omit<PickRequest, "resolve">): Promise<string | null> {
  if (!browserListener) return Promise.resolve(null);
  return new Promise((resolve) => {
    browserListener!({ ...req, resolve });
  });
}

/** 选择一个 EPUB 文件：移动端用内置浏览器，桌面用系统原生对话框 */
export async function pickEpub(title: string): Promise<string | null> {
  if (await isMobilePlatform()) {
    return requestBrowserPick({ mode: "file", title });
  }
  const r = await open({
    multiple: false,
    title,
    filters: [{ name: "EPUB 电子书", extensions: ["epub"] }],
  }).catch(() => null);
  return typeof r === "string" ? r : null;
}

/** 选择一个目录：移动端用内置浏览器，桌面用系统原生对话框 */
export async function pickDirectory(title: string): Promise<string | null> {
  if (await isMobilePlatform()) {
    return requestBrowserPick({ mode: "dir", title });
  }
  const r = await open({ directory: true, title }).catch(() => null);
  return typeof r === "string" ? r : null;
}

/** 内置浏览器：根目录候选 */
export function fsRoots(): Promise<string[]> {
  return invoke<string[]>("fs_roots");
}

/** 内置浏览器：目录条目 */
export interface FsEntry {
  name: string;
  path: string;
  is_dir: boolean;
  size: number | null;
}

export function listFsDir(path: string): Promise<FsEntry[]> {
  return invoke<FsEntry[]>("list_fs_dir", { path });
}

/** 内置浏览器：在父目录下新建文件夹，返回新目录路径 */
export function createFsDir(parent: string, name: string): Promise<string> {
  return invoke<string>("create_fs_dir", { parent, name });
}

/** 会话导出：写文本文件（路径经系统保存对话框取得） */
export function writeTextFile(path: string, content: string): Promise<void> {
  return invoke("write_text_file", { path, content });
}

/** 会话导入：读文本文件（路径经系统打开对话框取得） */
export function readTextFile(path: string): Promise<string> {
  return invoke("read_text_file", { path });
}
