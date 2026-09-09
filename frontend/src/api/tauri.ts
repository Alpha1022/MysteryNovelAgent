import { invoke } from "@tauri-apps/api/core";
import type {
  BookCard,
  BookDetail,
  ChatMessage,
  ChatReply,
  ClaspMatch,
  ClaspSearchPage,
  CommentRow,
  ConfigInfo,
  DescriptionResult,
  EpubPreview,
  ImportResult,
  LibraryProfile,
  LlmSettings,
  LlmStatus,
  MergeResult,
  PreparedPreview,
  ReaderContent,
  SettingsInfo,
  SourceItem,
  WebDavCfg,
  WebDavReport,
} from "../types";

export function getBooks(
  status: string | null,
  search: string | null,
): Promise<BookCard[]> {
  return invoke("get_books", { status, search });
}

export function getBookDetail(id: number): Promise<BookDetail | null> {
  return invoke("get_book_detail", { id });
}

export function getComments(bookId: number): Promise<CommentRow[]> {
  return invoke("get_comments", { bookId });
}

export function getConfig(): Promise<ConfigInfo> {
  return invoke("get_config");
}

// ============================= //
//  书库管理
// ============================= //

/** 新建书库（书库名仅 [A-Za-z0-9_] 且唯一），返回更新后的书库列表 */
export function addLibrary(
  name: string,
  title: string,
  path: string,
  theme?: LibraryProfile["theme"],
  defaultTags?: string[],
  webdav?: LibraryProfile["webdav"],
): Promise<LibraryProfile[]> {
  return invoke("add_library", { name, title, path, theme, defaultTags, webdav });
}

/** 修改书库路径（migrate = 迁移原书库数据到新路径） */
export function changeLibraryPath(
  id: string,
  newPath: string,
  migrate: boolean,
): Promise<void> {
  return invoke("change_library_path", { id, newPath, migrate });
}

/** 保存书库配置（标题/路径/主题/默认标签） */
export function saveLibrary(lib: LibraryProfile): Promise<LibraryProfile[]> {
  return invoke("save_library", { lib });
}

/** 删除书库配置（不删除磁盘文件） */
export function deleteLibrary(id: string): Promise<LibraryProfile[]> {
  return invoke("delete_library", { id });
}

/** 切换当前书库 */
export function switchLibrary(id: string): Promise<string> {
  return invoke("switch_library", { id });
}

/** Chatbot 对话（系统提示由后端按数据库实时构建） */
/** 书虫对话（agent 循环；进度经 chat-progress 事件实时推送，task_id 供打断） */
export function chatbotChat(messages: ChatMessage[], taskId?: string): Promise<ChatReply> {
  return invoke("chatbot_chat", { messages, taskId });
}

/** 分析 EPUB（提取元数据 + claspclub 静默匹配） */
export function analyzeEpub(path: string): Promise<EpubPreview> {
  return invoke("analyze_epub", { path });
}

/**
 * 保存 HTML5 拖入的文件内容到临时文件（WebView 拿不到本地路径，只能读字节）
 *
 * 字节走 IPC 原始请求体（大文件避免 JSON 数组序列化）；文件名经
 * encodeURIComponent 放在请求头（HTTP 头不允许非 ASCII）。返回临时文件路径。
 */
export function saveDroppedFile(filename: string, bytes: Uint8Array): Promise<string> {
  return invoke("save_dropped_file", bytes, {
    headers: { filename: encodeURIComponent(filename) },
  });
}

/** 确认导入（批量自动导入快速路径）：预处理 → 落盘 → 入库一次完成 */
export function importEpub(
  path: string,
  originalPath: string,
  title: string,
  author: string,
  tags: string[],
  matched: ClaspMatch[],
  doubanLinks: string[],
  mergeSummaries: boolean,
  taskId?: string,
): Promise<ImportResult> {
  return invoke("import_epub", {
    path,
    originalPath,
    title,
    author,
    tags,
    matched,
    doubanLinks,
    mergeSummaries,
    taskId,
  });
}

/** 导入预处理：完成全部爬取与简介合并（确认弹窗前执行；可打断，进度经事件推送） */
export function prepareImport(
  path: string,
  originalPath: string,
  title: string,
  author: string,
  tags: string[],
  matched: ClaspMatch[],
  doubanLinks: string[],
  mergeSummaries: boolean,
  taskId: string,
): Promise<PreparedPreview> {
  return invoke("prepare_import", {
    path,
    originalPath,
    title,
    author,
    tags,
    matched,
    doubanLinks,
    mergeSummaries,
    taskId,
  });
}

/** 提交导入：按确认弹窗中编辑的元数据落盘并入库（系列空白视为无系列） */
export function commitImport(
  taskId: string,
  title: string,
  author: string,
  tags: string[],
  description: string | null,
  seriesName: string | null = null,
  seriesOrder: number | null = null,
): Promise<ImportResult> {
  return invoke("commit_import", {
    taskId,
    title,
    author,
    tags,
    description,
    seriesName,
    seriesOrder,
  });
}

/** 为待确认导入设置手动上传封面 */
export function setPendingCover(taskId: string, imagePath: string): Promise<void> {
  return invoke("set_pending_cover", { taskId, imagePath });
}

/** 丢弃待确认的导入会话（取消导入时调用） */
export function discardImport(taskId: string): Promise<boolean> {
  return invoke("discard_import", { taskId });
}

/** 通用文本翻译为简体中文（LLM；各文本框翻译按钮） */
export function translateText(text: string): Promise<string> {
  return invoke("translate_text", { text });
}

/** 取消一个进行中的长任务 */
export function cancelTask(taskId: string): Promise<boolean> {
  return invoke("cancel_task", { taskId });
}

/** 切换阅读状态（想读/在读/已读） */
export function setBookStatus(id: number, status: string): Promise<void> {
  return invoke("set_book_status", { id, status });
}

/** 通用 LLM 对话（消息序列须 system 开头；token 用量自动入库） */
export function llmChat(messages: ChatMessage[]): Promise<ChatReply> {
  return invoke("llm_chat", { messages });
}

/** 保存书评（仅写入 my_review） */
export function saveBookReview(id: number, review: string): Promise<void> {
  return invoke("save_book_review", { id, review });
}

/** claspclub 分页搜索（pageSize=5，按豆瓣评分排序，结果含简介/封面） */
export function searchClasp(keyword: string, page = 1): Promise<ClaspSearchPage> {
  return invoke("search_clasp", { keyword, page });
}

/** 递归收集文件夹下所有 EPUB（批量加书） */
export function collectEpubs(dir: string): Promise<string[]> {
  return invoke("collect_epubs", { dir });
}

/** 详情页元数据编辑（tags 为逗号分隔字符串；系列空白视为无系列） */
export function updateBookMeta(
  id: number,
  title: string,
  author: string,
  tags: string,
  description: string | null,
  seriesName: string | null = null,
  seriesOrder: number | null = null,
): Promise<void> {
  return invoke("update_book_meta", {
    id,
    title,
    author,
    tags,
    description,
    seriesName,
    seriesOrder,
  });
}

/** 读取设置（LLM 配置 + token 用量统计） */
export function getSettings(): Promise<SettingsInfo> {
  return invoke("get_settings");
}

/** 保存 LLM 设置到 config.toml */
export function saveSettings(llm: LlmSettings): Promise<void> {
  return invoke("save_settings", { settings: llm });
}

/** LLM 可用性预检（加书弹窗合并简介提示用） */
export function llmStatus(): Promise<LlmStatus> {
  return invoke("llm_status");
}

/** 删除书籍记录（书库文件与封面缓存一并删除） */
export function deleteBook(id: number): Promise<void> {
  return invoke("delete_book", { id });
}

/** 打开本地 EPUB 文件（书库副本优先，原始文件兜底） */
export function openBookFile(id: number): Promise<void> {
  return invoke("open_book_file", { id });
}

/** 手动上传封面替换，返回新的封面路径 */
export function uploadCover(
  bookId: number,
  imagePath: string,
): Promise<{ cover_path: string }> {
  return invoke("upload_cover", { bookId, imagePath });
}

/** 读取 EPUB 内嵌封面字节（未匹配时默认封面预览） */
export function fetchEpubCover(path: string): Promise<ArrayBuffer> {
  return invoke("fetch_epub_cover", { path });
}

// ============================= //
//  来源管理
// ============================= //

/** 列出书籍的全部来源项目（clasp 在前、豆瓣在后，各自有序） */
export function listSources(bookId: number): Promise<SourceItem[]> {
  return invoke("list_sources", { bookId });
}

/** 添加 claspclub 来源（搜索多选 ID 或粘贴多行页面链接；豆瓣来源自动按顺序补充） */
export function addClaspSources(
  bookId: number,
  items: string[],
): Promise<SourceItem[]> {
  return invoke("add_clasp_sources", { bookId, items });
}

/** 添加豆瓣来源（粘贴多行书籍页链接） */
export function addDoubanSources(
  bookId: number,
  items: string[],
): Promise<SourceItem[]> {
  return invoke("add_douban_sources", { bookId, items });
}

/** 保存来源排序（拖拽后回传该类别的有序 ref 列表） */
export function saveSourceOrder(
  bookId: number,
  kind: string,
  refs: string[],
): Promise<void> {
  return invoke("save_source_order", { bookId, kind, refs });
}

/** 删除单个来源项目（其短评一并删除），返回更新后的列表 */
export function deleteSource(
  bookId: number,
  kind: string,
  refKey: string,
): Promise<SourceItem[]> {
  return invoke("delete_source", { bookId, kind, refKey });
}

/** 清空某一类来源项目及其短评，返回更新后的列表 */
export function clearSources(
  bookId: number,
  kind: string,
): Promise<SourceItem[]> {
  return invoke("clear_sources", { bookId, kind });
}

/** 更新来源项目数据：重新爬取元数据（书名/作者/封面/简介/版本封面/标签） */
export function refreshSourceMeta(
  bookId: number,
  kind: string,
  refKey: string,
): Promise<void> {
  return invoke("refresh_source_meta", { bookId, kind, refKey });
}

/** 重设书籍标签：按 claspclub 来源标签并集 + 书库默认标签重建 */
export function resetBookTags(bookId: number): Promise<string[]> {
  return invoke("reset_book_tags", { bookId });
}

/** 重设书籍系列：按 claspclub 来源系列信息继承规则重建（与导入一致） */
export function resetBookSeries(
  bookId: number,
): Promise<[string, number | null]> {
  return invoke("reset_book_series", { bookId });
}

/** 内置阅读器：提取书籍 EPUB 章节正文 */
export function getReaderContent(bookId: number): Promise<ReaderContent> {
  return invoke("get_reader_content", { id: bookId });
}

// ============================= //
//  WebDav 同步
// ============================= //

/** WebDav 连接测试（按书库名创建远程子目录 + 列取内容） */
export function webdavTest(wd: WebDavCfg, name: string): Promise<string> {
  return invoke("webdav_test", {
    url: wd.url,
    username: wd.username,
    password: wd.password,
    remoteDir: wd.remote_dir,
    name,
  });
}

/** 打断一个进行中的 WebDav 同步任务 */
export function cancelSyncTask(taskId: string): Promise<boolean> {
  return invoke("cancel_sync_task", { taskId });
}

/** 是否已授予「所有文件访问」权限（Android 11+ 检测；桌面端恒 true） */
export function hasStoragePermission(): Promise<boolean> {
  return invoke("has_all_files_access");
}

/** 打开本应用的「所有文件访问」系统授权页（仅移动端有效；用户开关后返回重试同步） */
export function openStoragePermissionSettings(): Promise<void> {
  return invoke("open_all_files_access_settings");
}

/** 同步到云端：本地书库完整覆盖云端（EPUB + covers + 数据库快照）；进度经 task-progress 推送 */
export function webdavPush(libraryId: string, taskId?: string): Promise<WebDavReport> {
  return invoke("webdav_push", { libraryId, taskId });
}

/** 从云端同步：云端完整覆盖本地（EPUB + covers + 数据库）；进度经 task-progress 推送 */
export function webdavPull(libraryId: string, taskId?: string): Promise<WebDavReport> {
  return invoke("webdav_pull", { libraryId, taskId });
}

/** 重新爬取单个来源项目的短评（并刷新其元数据），返回新增条数 */
export function refreshSourceComments(
  bookId: number,
  kind: string,
  refKey: string,
): Promise<number> {
  return invoke("refresh_source_comments", { bookId, kind, refKey });
}

/** 将来源项目封面设为本书封面（clasp 项目可传版本封面本地路径） */
export function setSourceAsCover(
  bookId: number,
  kind: string,
  refKey: string,
  editionPath?: string,
): Promise<{ cover_path: string }> {
  return invoke("set_source_as_cover", { bookId, kind, refKey, editionPath });
}

/** 将来源项目简介设为本书简介 */
export function setSourceAsDescription(
  bookId: number,
  kind: string,
  refKey: string,
): Promise<string> {
  return invoke("set_source_as_description", { bookId, kind, refKey });
}

/** 合并全部来源项目的简介为一段简体中文简介（LLM） */
export function mergeSourceDescriptions(
  bookId: number,
): Promise<DescriptionResult> {
  return invoke("merge_source_descriptions", { bookId });
}

/** GUI 多选合并模式：按顺序合并多本书（EPUB + 来源 + 短评，删除原书） */
export function mergeBooks(
  ids: number[],
  mergeDescription: boolean,
): Promise<MergeResult> {
  return invoke("merge_books_gui", { ids, mergeDescription });
}
