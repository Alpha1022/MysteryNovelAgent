use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use serde::{Deserialize, Serialize};
use tauri::{Emitter, State};
use tracing::{info, warn};

use mystery_novel_agent::config::AppConfig;
use mystery_novel_agent::{agent, db, ingestion, merge, spider, utils};

/// 取消返回的固定错误标记（前端据此与真实错误区分）
const CANCELLED_MSG: &str = "任务已取消";

/// 前端诊断日志转发（写入 app.log，排查滚动/拖拽等问题时在前端加一行即可）
#[tauri::command]
pub fn debug_log(msg: String) {
  tracing::info!("[frontend] {msg}");
}

/// 拖入文件的临时落盘目录（HTML5 拖拽导入；内容并入书库后副本无保留价值）
pub fn drag_import_dir() -> std::path::PathBuf {
  mystery_novel_agent::config::temp_dir().join("drag-import")
}

/// 启动清扫：上一会话遗留的拖入临时文件（导入流程已复制入库，副本可删）
pub fn cleanup_drag_import_dir() {
  use tracing::warn;
  let dir = drag_import_dir();
  if dir.exists() {
    if let Err(e) = std::fs::remove_dir_all(&dir) {
      warn!("清扫拖入临时目录失败: {e}");
    }
  }
}

/// 保存前端 HTML5 拖拽导入的文件内容到临时目录
///
/// WebView 的 HTML5 drop 事件拿不到本地路径（安全限制，只有文件内容），
/// 因此前端读出字节经 IPC 原始请求体传给本命令落盘，返回临时文件路径后
/// 复用既有的 analyze_epub / 批量导入流程。
/// 文件名经 encodeURIComponent 后放在 "filename" 请求头（HTTP 头不允许非 ASCII）。
#[tauri::command]
pub fn save_dropped_file(request: tauri::ipc::Request) -> Result<String, String> {
  use percent_encoding::percent_decode_str;
  use std::sync::atomic::{AtomicU64, Ordering};

  // 文件名：URL 解码 → 仅保留 basename → 剔除 Windows 非法字符（防御性处理）
  let raw_name = request
    .headers()
    .get("filename")
    .and_then(|v| v.to_str().ok())
    .map(|s| percent_decode_str(s).decode_utf8_lossy().into_owned())
    .unwrap_or_default();
  let basename = raw_name.rsplit(['/', '\\']).next().unwrap_or_default();
  let sanitized: String = basename
    .chars()
    .map(|c| {
      if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*') {
        '_'
      } else {
        c
      }
    })
    .collect();
  let filename = if sanitized.trim().is_empty() {
    "dropped.epub".to_string()
  } else {
    sanitized
  };

  let bytes = match request.body() {
    tauri::ipc::InvokeBody::Raw(b) if !b.is_empty() => b,
    tauri::ipc::InvokeBody::Raw(_) => return Err("拖入的文件内容为空".into()),
    // JSON 主体：非原始字节通道（理论上仅 Android postMessage 路径），拖拽不支持
    _ => return Err("拖入内容编码不受支持".into()),
  };

  static SEQ: AtomicU64 = AtomicU64::new(0);
  let dir = drag_import_dir();
  std::fs::create_dir_all(&dir).map_err(|e| format!("创建临时目录失败: {e}"))?;
  let seq = SEQ.fetch_add(1, Ordering::Relaxed);
  let path = dir.join(format!("{}-{seq}-{filename}", unix_secs()));
  std::fs::write(&path, bytes).map_err(|e| format!("写入临时文件失败: {e}"))?;
  Ok(path.to_string_lossy().into_owned())
}

/// 待确认的导入会话（prepare 与 commit 之间暂存爬取产物）
#[derive(Default)]
pub struct ImportCache(Mutex<HashMap<String, ingestion::PreparedImport>>);

// ============================= //
//  查询命令
// ============================= //

#[tauri::command]
pub async fn get_books(
  state: State<'_, Mutex<rusqlite::Connection>>,
  status: Option<String>,
  search: Option<String>,
) -> Result<Vec<db::BookCardRow>, String> {
  let conn = state.lock().map_err(|e| e.to_string())?;
  let search_lower = search.map(|s| s.to_lowercase());
  let library_id = AppConfig::load().current_library_id().map(str::to_string);
  db::get_book_cards(
    &conn,
    status.as_deref(),
    search_lower.as_deref(),
    library_id.as_deref(),
  )
  .map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_book_detail(
  state: State<'_, Mutex<rusqlite::Connection>>,
  id: i64,
) -> Result<Option<db::BookDetailRow>, String> {
  let conn = state.lock().map_err(|e| e.to_string())?;
  db::get_book_detail(&conn, id).map_err(|e| e.to_string())
}

#[tauri::command]
pub async fn get_comments(
  state: State<'_, Mutex<rusqlite::Connection>>,
  book_id: i64,
) -> Result<Vec<db::CommentRow>, String> {
  let conn = state.lock().map_err(|e| e.to_string())?;
  db::get_comments_for_book(&conn, book_id).map_err(|e| e.to_string())
}

#[derive(Serialize)]
pub struct ConfigDto {
  pub library_path: Option<String>,
  pub covers_dir: Option<String>,
  /// 书库列表（多书库管理）
  pub libraries: Vec<LibraryDto>,
  /// 当前书库 ID
  pub current_library: Option<String>,
  /// 当前书库的固定标签（导入时强制添加）
  pub default_tags: Vec<String>,
}

/// 书库配置项
#[derive(Serialize, Deserialize, Clone)]
pub struct LibraryDto {
  #[serde(default)]
  pub id: String,
  /// 书库名（slug）：仅 [A-Za-z0-9_]，本地唯一；WebDav 远程子目录名
  #[serde(default)]
  pub name: String,
  #[serde(default)]
  pub title: String,
  #[serde(default)]
  pub path: String,
  #[serde(default)]
  pub theme: LibraryThemeDto,
  #[serde(default)]
  pub default_tags: Vec<String>,
  /// WebDav 同步配置（每书库独立开关与凭据）
  #[serde(default)]
  pub webdav: WebDavDto,
}

/// WebDav 同步配置（GUI 设置页每书库一节）
#[derive(Serialize, Deserialize, Clone, Default)]
pub struct WebDavDto {
  #[serde(default)]
  pub enabled: bool,
  #[serde(default)]
  pub url: String,
  #[serde(default)]
  pub username: String,
  #[serde(default)]
  pub password: String,
  #[serde(default)]
  pub remote_dir: String,
}

impl WebDavDto {
  fn to_config(&self) -> mystery_novel_agent::config::WebDavConfig {
    mystery_novel_agent::config::WebDavConfig {
      enabled: self.enabled,
      url: self.url.trim().to_string(),
      username: self.username.trim().to_string(),
      password: self.password.clone(),
      remote_dir: self.remote_dir.trim().to_string(),
    }
  }

  fn from_config(w: &mystery_novel_agent::config::WebDavConfig) -> Self {
    Self {
      enabled: w.enabled,
      url: w.url.clone(),
      username: w.username.clone(),
      password: w.password.clone(),
      remote_dir: w.remote_dir.clone(),
    }
  }
}

#[derive(Serialize, Deserialize, Clone, Default)]
pub struct LibraryThemeDto {
  #[serde(default)]
  pub accent: Option<String>,
  #[serde(default)]
  pub bg: Option<String>,
  #[serde(default)]
  pub panel: Option<String>,
  #[serde(rename = "panel2", default)]
  pub panel_2: Option<String>,
  #[serde(default)]
  pub ink: Option<String>,
  #[serde(default)]
  pub muted: Option<String>,
  #[serde(default)]
  pub read: Option<String>,
  #[serde(default)]
  pub reading: Option<String>,
  #[serde(default)]
  pub wish: Option<String>,
}

fn library_to_dto(l: &mystery_novel_agent::config::LibraryConfig) -> LibraryDto {
  LibraryDto {
    id: l.id.clone(),
    name: l.name.clone(),
    title: l.title.clone(),
    path: l.path.to_string_lossy().into_owned(),
    theme: LibraryThemeDto {
      accent: l.theme.accent.clone(),
      bg: l.theme.bg.clone(),
      panel: l.theme.panel.clone(),
      panel_2: l.theme.panel_2.clone(),
      ink: l.theme.ink.clone(),
      muted: l.theme.muted.clone(),
      read: l.theme.read.clone(),
      reading: l.theme.reading.clone(),
      wish: l.theme.wish.clone(),
    },
    default_tags: l.default_tags.clone(),
    webdav: WebDavDto::from_config(&l.webdav),
  }
}

#[tauri::command]
pub fn get_config() -> ConfigDto {
  let cfg = AppConfig::load();
  ConfigDto {
    library_path: cfg.library_path.as_ref().map(|p| p.display().to_string()),
    covers_dir: cfg.covers_dir().ok().map(|p| p.display().to_string()),
    libraries: cfg.libraries.iter().map(library_to_dto).collect(),
    current_library: cfg.current_library.clone(),
    default_tags: cfg.default_tags(),
  }
}

/// LibraryThemeDto → 配置结构
fn theme_dto_to_config(t: LibraryThemeDto) -> mystery_novel_agent::config::LibraryTheme {
  mystery_novel_agent::config::LibraryTheme {
    accent: t.accent,
    bg: t.bg,
    panel: t.panel,
    panel_2: t.panel_2,
    ink: t.ink,
    muted: t.muted,
    read: t.read,
    reading: t.reading,
    wish: t.wish,
  }
}

/// 规范化固定标签（去空格去空；空则回退"推理小说"）
fn normalize_default_tags(tags: Option<Vec<String>>) -> Vec<String> {
  let list: Vec<String> = tags
    .unwrap_or_default()
    .into_iter()
    .map(|t| t.trim().to_string())
    .filter(|t| !t.is_empty())
    .collect();
  if list.is_empty() {
    vec!["推理小说".to_string()]
  } else {
    list
  }
}

/// 校验书库名：合法字符 + 本地唯一（大小写不敏感；exclude_id 用于更新场景）
fn validate_library_name(
  cfg: &AppConfig,
  name: &str,
  exclude_id: Option<&str>,
) -> Result<(), String> {
  if !mystery_novel_agent::config::is_valid_library_name(name) {
    return Err("书库名只能使用大小写字母、数字和下划线，且不能为空（≤64 字符）".into());
  }
  if cfg.library_name_taken(name, exclude_id) {
    return Err(format!("书库名「{name}」已存在（与大小写写法无关），请换一个"));
  }
  Ok(())
}

/// 新建书库（保存书库名/标题/主题/固定标签与 WebDav；自动创建目录），返回更新后的书库列表
#[tauri::command]
pub fn add_library(
  name: String,
  title: String,
  path: String,
  theme: Option<LibraryThemeDto>,
  default_tags: Option<Vec<String>>,
  webdav: Option<WebDavDto>,
) -> Result<Vec<LibraryDto>, String> {
  let name = name.trim().to_string();
  let title = title.trim().to_string();
  let path = path.trim().to_string();
  if title.is_empty() {
    return Err("书库名称不能为空".into());
  }
  if path.is_empty() {
    return Err("书库路径不能为空".into());
  }
  let mut cfg = AppConfig::load();
  validate_library_name(&cfg, &name, None)?;
  let id = format!("lib-{}-{}", unix_secs(), cfg.libraries.len() + 1);
  std::fs::create_dir_all(&path).map_err(|e| format!("目录创建失败: {e}"))?;
  cfg
    .libraries
    .push(mystery_novel_agent::config::LibraryConfig {
      id,
      name,
      title,
      path: PathBuf::from(&path),
      theme: theme.map(theme_dto_to_config).unwrap_or_default(),
      default_tags: normalize_default_tags(default_tags),
      webdav: webdav
        .map(|w| w.to_config())
        .unwrap_or_default(),
    });
  if cfg.current_library.is_none() {
    cfg.current_library = cfg.libraries.first().map(|l| l.id.clone());
  }
  cfg.save().map_err(|e| e.to_string())?;
  Ok(cfg.libraries.iter().map(library_to_dto).collect())
}

/// 保存书库配置（书库名/标题/路径/主题/默认标签/WebDav），返回更新后的书库列表
#[tauri::command]
pub fn save_library(lib: LibraryDto) -> Result<Vec<LibraryDto>, String> {
  let mut cfg = AppConfig::load();
  let title = lib.title.trim().to_string();
  if title.is_empty() {
    return Err("书库名称不能为空".into());
  }
  let path = lib.path.trim().to_string();
  if path.is_empty() {
    return Err("书库路径不能为空".into());
  }
  let name = lib.name.trim().to_string();
  // 校验需在取得可变借用前完成（避免与 existing 的借用冲突）
  validate_library_name(&cfg, &name, Some(&lib.id))?;
  let Some(existing) = cfg.libraries.iter_mut().find(|l| l.id == lib.id) else {
    return Err("书库不存在".into());
  };
  existing.name = name;
  existing.title = title;
  existing.path = PathBuf::from(path);
  existing.theme = theme_dto_to_config(lib.theme);
  existing.default_tags = normalize_default_tags(Some(lib.default_tags));
  existing.webdav = lib.webdav.to_config();
  cfg.save().map_err(|e| e.to_string())?;
  Ok(cfg.libraries.iter().map(library_to_dto).collect())
}

/// 删除书库配置（不删除磁盘文件；至少保留一个书库）
#[tauri::command]
pub fn delete_library(id: String) -> Result<Vec<LibraryDto>, String> {
  let mut cfg = AppConfig::load();
  if cfg.libraries.len() <= 1 {
    return Err("至少保留一个书库，无法删除".into());
  }
  cfg.libraries.retain(|l| l.id != id);
  if cfg.current_library.as_deref() == Some(id.as_str()) {
    cfg.current_library = cfg.libraries.first().map(|l| l.id.clone());
  }
  cfg.save().map_err(|e| e.to_string())?;
  Ok(cfg.libraries.iter().map(library_to_dto).collect())
}

// ============================= //
//  内置文件浏览器（移动端选书/选书库目录）
// ============================= //

/// 是否移动端构建（前端据此决定用系统原生对话框还是内置浏览器）
#[tauri::command]
pub fn is_mobile() -> bool {
  cfg!(mobile)
}

/// 文件浏览器的根目录候选（移动端：公共外部存储 + 应用私有目录；桌面：各磁盘挂载点）
#[tauri::command]
pub fn fs_roots() -> Vec<String> {
  #[cfg(mobile)]
  {
    let mut roots = vec![
      "/storage/emulated/0".to_string(),
      "/sdcard".to_string(),
    ];
    if let Some(dir) = mystery_novel_agent::config::data_dir_override() {
      roots.push(dir.to_string_lossy().into_owned());
    }
    roots
  }
  #[cfg(not(mobile))]
  {
    // 桌面端使用系统原生目录对话框，不会调用内置浏览器
    let _ = &mut Vec::<String>::new();
    Vec::new()
  }
}

/// 文件系统条目（内置浏览器列表项）
#[derive(Serialize)]
pub struct FsEntry {
  pub name: String,
  pub path: String,
  pub is_dir: bool,
  /// 文件大小（目录为 None）
  pub size: Option<u64>,
}

/// 在指定父目录下新建文件夹（内置浏览器"新建文件夹"用），返回新目录路径
#[tauri::command]
pub fn create_fs_dir(parent: String, name: String) -> Result<String, String> {
  let name = name.trim();
  if name.is_empty() {
    return Err("文件夹名不能为空".into());
  }
  if name.contains('/') || name.contains('\\') || name.starts_with('.') {
    return Err("文件夹名不能包含路径分隔符或以 . 开头".into());
  }
  let dir = PathBuf::from(parent.trim()).join(name);
  std::fs::create_dir_all(&dir).map_err(|e| format!("创建失败: {e}"))?;
  Ok(dir.to_string_lossy().into_owned())
}

/// 写文本文件（会话导出等；路径经系统保存对话框取得）
#[tauri::command]
pub fn write_text_file(path: String, content: String) -> Result<(), String> {
  let p = PathBuf::from(&path);
  if let Some(parent) = p.parent() {
    let _ = std::fs::create_dir_all(parent);
  }
  std::fs::write(&p, content).map_err(|e| format!("写入失败: {e}"))
}

/// 读文本文件（会话导入等；路径经系统打开对话框取得）
#[tauri::command]
pub fn read_text_file(path: String) -> Result<String, String> {
  std::fs::read_to_string(&path).map_err(|e| format!("读取失败: {e}"))
}
/// 列出目录内容（内置浏览器；目录在前、名称升序）
#[tauri::command]
pub fn list_fs_dir(path: String) -> Result<Vec<FsEntry>, String> {  let p = PathBuf::from(&path);
  if !p.is_dir() {
    return Err(format!("目录不存在或不可读: {path}"));
  }
  let mut entries: Vec<FsEntry> = Vec::new();
  let rd =
    std::fs::read_dir(&p).map_err(|e| format!("读取目录失败（可能缺少存储权限）: {e}"))?;
  for entry in rd.flatten() {
    let name = entry.file_name().to_string_lossy().into_owned();
    if name.starts_with('.') {
      continue; // 隐藏文件不展示
    }
    let is_dir = entry.file_type().map(|t| t.is_dir()).unwrap_or(false);
    let size = if is_dir {
      None
    } else {
      entry.metadata().ok().map(|m| m.len())
    };
    entries.push(FsEntry {
      path: entry.path().to_string_lossy().into_owned(),
      name,
      is_dir,
      size,
    });
  }
  entries.sort_by(|a, b| match (b.is_dir, a.is_dir) {
    (true, false) => std::cmp::Ordering::Less,
    (false, true) => std::cmp::Ordering::Greater,
    _ => a.name.to_lowercase().cmp(&b.name.to_lowercase()),
  });
  Ok(entries)
}

// ============================= //
//  WebDav 同步（每书库独立开关；仅手动触发：推送 / 拉取，无自动同步）
// ============================= //

/// WebDav 同步结果
#[derive(Serialize)]
pub struct WebDavSyncReportDto {
  pub total: usize,
  pub uploaded: usize,
  pub downloaded: usize,
  pub deleted: usize,
  pub skipped: usize,
  pub failed: usize,
  pub books_imported: usize,
  pub books_updated: usize,
  pub books_deleted: usize,
  pub errors: Vec<String>,
}

/// WebDav 连接测试（按书库名创建远程子目录 + PROPFIND；保存前即可用草稿值测试）
#[tauri::command]
pub async fn webdav_test(
  url: String,
  username: String,
  password: String,
  remote_dir: String,
  name: String,
) -> Result<String, String> {
  if !mystery_novel_agent::config::is_valid_library_name(&name) {
    return Err("书库名非法（仅大小写字母/数字/下划线），请先在「书库」页签修正".into());
  }
  let cfg = mystery_novel_agent::config::WebDavConfig {
    enabled: true,
    url,
    username,
    password,
    remote_dir,
  };
  mystery_novel_agent::webdav::test_connection(&cfg, &name)
    .await
    .map_err(|e| format!("连接失败: {e}"))?;
  Ok("连接成功".to_string())
}

/// 同步到云端：本地书库完整覆盖云端（EPUB + covers + 数据库快照；云端多余文件删除）
///
/// 仅允许对当前书库执行；进度经 task-progress 事件实时推送（phase 以 webdav- 开头），
/// task_id 注册取消令牌供前端「打断」。
#[tauri::command]
pub async fn webdav_push(
  registry: State<'_, SyncTaskRegistry>,
  app: tauri::AppHandle,
  library_id: String,
  task_id: Option<String>,
) -> Result<WebDavSyncReportDto, String> {
  run_webdav_sync(registry, app, library_id, task_id, true).await
}

/// 从云端同步：云端完整覆盖本地（EPUB + covers + 数据库按目标书库整库覆盖）
///
/// 仅允许对当前书库执行；进度/打断同 webdav_push。
#[tauri::command]
pub async fn webdav_pull(
  registry: State<'_, SyncTaskRegistry>,
  app: tauri::AppHandle,
  library_id: String,
  task_id: Option<String>,
) -> Result<WebDavSyncReportDto, String> {
  run_webdav_sync(registry, app, library_id, task_id, false).await
}

/// 推送/拉取共用执行体：当前书库校验 → 取消令牌注册 → 进度事件推送
async fn run_webdav_sync(
  registry: State<'_, SyncTaskRegistry>,
  app: tauri::AppHandle,
  library_id: String,
  task_id: Option<String>,
  push: bool,
) -> Result<WebDavSyncReportDto, String> {
  let cfg = AppConfig::load();
  let lib = cfg
    .libraries
    .iter()
    .find(|l| l.id == library_id)
    .ok_or_else(|| "书库不存在".to_string())?
    .clone();
  // 同步仅对当前书库生效（覆盖语义危险，防止误同步非当前书库）
  if cfg.current_library.as_deref() != Some(lib.id.as_str()) {
    return Err("仅允许同步当前书库（请先在设置中切换到该书库）".into());
  }
  // 封面缓存目录（移动端为应用私有目录，避免被图库收录且不受书库目录权限限制）
  let covers_dir = cfg.covers_dir().map_err(|e| e.to_string())?;
  let ctx = mystery_novel_agent::webdav::ensure_ctx(&lib, covers_dir).map_err(|e| e.to_string())?;

  let cancel = task_id
    .as_deref()
    .map(|id| register_sync_task(&registry, id))
    .unwrap_or_default();
  let emitter = app;
  let progress = move |p: mystery_novel_agent::webdav::SyncProgress| {
    let _ = emitter.emit("task-progress", &p);
  };
  let result = if push {
    mystery_novel_agent::webdav::push_library(&lib.webdav, &ctx, &cancel, progress).await
  } else {
    mystery_novel_agent::webdav::pull_library(&lib.webdav, &ctx, &cancel, progress).await
  };
  finish_sync_task(&registry, task_id.as_deref());
  match result {
    Ok(r) => Ok(report_to_dto(r)),
    Err(e) if e.to_string() == "已取消" => Err(CANCELLED_MSG.into()),
    Err(e) => Err(format!(
      "{}失败: {e}",
      if push { "同步到云端" } else { "从云端同步" }
    )),
  }
}

fn report_to_dto(r: mystery_novel_agent::webdav::SyncReport) -> WebDavSyncReportDto {
  WebDavSyncReportDto {
    total: r.total,
    uploaded: r.uploaded,
    downloaded: r.downloaded,
    deleted: r.deleted,
    skipped: r.skipped,
    failed: r.failed,
    books_imported: r.books_imported,
    books_updated: r.books_updated,
    books_deleted: r.books_deleted,
    errors: r.errors,
  }
}

/// 递归复制目录内容（书库数据迁移用）
fn copy_dir_recursive(src: &Path, dst: &Path) -> std::io::Result<()> {
  std::fs::create_dir_all(dst)?;
  for entry in std::fs::read_dir(src)? {
    let entry = entry?;
    let ty = entry.file_type()?;
    let dest = dst.join(entry.file_name());
    if ty.is_dir() {
      copy_dir_recursive(&entry.path(), &dest)?;
    } else {
      std::fs::copy(entry.path(), &dest)?;
    }
  }
  Ok(())
}

/// 修改书库路径
///
/// `migrate = true`：将原书库目录内容（EPUB 与封面缓存）复制到新路径后切换；
/// `migrate = false`：仅切换路径（目录内容保留不动，不要求为空）。
#[tauri::command]
pub fn change_library_path(id: String, new_path: String, migrate: bool) -> Result<(), String> {
  let new_path = new_path.trim().to_string();
  if new_path.is_empty() {
    return Err("路径不能为空".into());
  }
  let mut cfg = AppConfig::load();
  let Some(lib) = cfg.libraries.iter_mut().find(|l| l.id == id) else {
    return Err("书库不存在".into());
  };
  let old_path = lib.path.clone();
  let np = PathBuf::from(&new_path);
  if np == old_path {
    return Ok(());
  }
  if migrate && old_path.exists() {
    copy_dir_recursive(&old_path, &np).map_err(|e| format!("数据迁移失败: {e}"))?;
    info!(
      "书库数据已迁移: {} → {}",
      old_path.display(),
      np.display()
    );
  } else {
    std::fs::create_dir_all(&np).map_err(|e| format!("目录创建失败: {e}"))?;
  }
  lib.path = np;
  cfg.save().map_err(|e| e.to_string())
}

/// 切换当前书库，返回当前书库 ID
#[tauri::command]
pub fn switch_library(id: String) -> Result<String, String> {
  let mut cfg = AppConfig::load();
  if !cfg.libraries.iter().any(|l| l.id == id) {
    return Err("书库不存在".into());
  }
  cfg.current_library = Some(id.clone());
  cfg.save().map_err(|e| e.to_string())?;
  Ok(id)
}

// ============================= //
//  加书 / 删书 / 封面替换
// ============================= //

/// 前端确认弹窗回传的 claspclub 匹配条目
#[derive(Deserialize)]
pub struct MatchInfo {
  pub id: String,
  #[serde(default)]
  pub title: String,
  #[serde(default)]
  pub author: String,
  #[serde(default)]
  pub tags: Vec<String>,
  #[serde(default)]
  pub cover_url: Option<String>,
  /// 无剧透简介（分页搜索接口携带，导入时省去详情调用）
  #[serde(default)]
  pub summary: Option<String>,
  /// 豆瓣书籍页链接
  #[serde(default)]
  pub douban_url: Option<String>,
}

/// 匹配条目展示信息
#[derive(Serialize)]
pub struct MatchDto {
  pub id: String,
  pub title: String,
  pub author: String,
  pub tags: Vec<String>,
  pub cover_url: Option<String>,
  /// 无剧透简介（分页搜索接口携带）
  pub summary: Option<String>,
  /// 豆瓣书籍页链接
  pub douban_url: Option<String>,
}

impl MatchDto {
  fn from_suggestion(s: &spider::ClaspBookSuggestion) -> Self {
    MatchDto {
      id: s.id.clone(),
      title: s.title.clone(),
      author: s.author_name.clone(),
      tags: s.tags.clone(),
      cover_url: s.cover_url.clone(),
      summary: s.summary.clone(),
      douban_url: s.douban_url.clone(),
    }
  }
}

/// 前端匹配条目 → 导入/重新匹配共用的建议结构
fn to_suggestion_matches(matched: Vec<MatchInfo>) -> Vec<ingestion::SuggestionMatch> {
  matched
    .into_iter()
    .map(|m| ingestion::SuggestionMatch {
      id: m.id,
      title: m.title,
      author: m.author,
      tags: m.tags,
      cover_url: m.cover_url,
      summary: m.summary,
      douban_url: m.douban_url,
    })
    .collect()
}

/// 分页搜索结果（一页）
#[derive(Serialize)]
pub struct SearchPageDto {
  pub items: Vec<MatchDto>,
  pub page: i64,
  pub total_pages: i64,
  pub total: i64,
  /// true = claspclub 无精确匹配，正在返回相近结果
  pub fuzzy: bool,
}

/// EPUB 分析结果（导入确认弹窗的数据源）
#[derive(Serialize)]
pub struct EpubPreviewDto {
  pub path: String,
  pub title: String,
  pub author: String,
  pub is_chinese: bool,
  /// 是否有内嵌封面（未匹配时前端默认采用 EPUB 封面）
  pub has_epub_cover: bool,
  /// zip 重建副本路径（原 EPUB 含重复条目时存在；导入应使用该路径）
  pub effective_path: Option<String>,
  pub search_results: usize,
  pub matched: Option<MatchDto>,
}

/// 导入完成结果
#[derive(Serialize)]
pub struct ImportResultDto {
  pub id: i64,
  pub title: String,
  pub author: String,
  pub library_file: String,
  pub cover_path: Option<String>,
  pub comments: usize,
  /// 简介融合所用模型（未融合/降级时为 None）
  pub fusion_model: Option<String>,
  /// 简介融合降级/失败原因（None 表示融合正常）
  pub fusion_error: Option<String>,
}

/// 分析 EPUB：提取内嵌书名/作者（繁体自动转简体）+ claspclub 静默搜索匹配
///
/// 非中文书名时不落库，由前端弹窗确认后才允许调用 import_epub。
#[tauri::command]
pub async fn analyze_epub(path: String) -> Result<EpubPreviewDto, String> {
  let a = ingestion::analyze_epub(Path::new(&path))
    .await
    .map_err(|e| e.to_string())?;
  Ok(EpubPreviewDto {
    path,
    title: a.title,
    author: a.author,
    is_chinese: a.is_chinese_title,
    has_epub_cover: a.has_epub_cover,
    effective_path: a
      .effective_path
      .as_ref()
      .map(|p| p.to_string_lossy().into_owned()),
    search_results: a.search_results,
    matched: a.suggestion.as_ref().map(MatchDto::from_suggestion),
  })
}

/// 读取 EPUB 内嵌封面字节（加书弹窗"未匹配时默认封面"预览）
#[tauri::command]
pub fn fetch_epub_cover(path: String) -> Result<tauri::ipc::Response, String> {
  let bytes = ingestion::extract_epub_cover_bytes(Path::new(&path))
    .map(|(b, _)| b)
    .ok_or_else(|| "EPUB 无内嵌封面".to_string())?;
  Ok(tauri::ipc::Response::new(bytes))
}

/// 确认导入（批量自动导入快速路径）：预处理 → 落盘 → 入库一次完成
///
/// - `matched`：claspclub 匹配条目（多条 = 合并本）
/// - `douban_links`：豆瓣书籍页链接（可多条）
/// - `merge_summaries`：多条简介时是否调用 LLM 合并（取消/未配置 → 取第一本）
/// - `task_id`：长任务标识，配合 `cancel_task` 打断；进度经 task-progress 事件实时推送
#[tauri::command]
pub async fn import_epub(
  state: State<'_, Mutex<rusqlite::Connection>>,
  registry: State<'_, TaskRegistry>,
  app: tauri::AppHandle,
  path: String,
  original_path: Option<String>,
  title: String,
  author: String,
  tags: Option<Vec<String>>,
  matched: Option<Vec<MatchInfo>>,
  douban_links: Option<Vec<String>>,
  merge_summaries: Option<bool>,
  task_id: Option<String>,
) -> Result<ImportResultDto, String> {
  let title = title.trim().to_string();
  if title.is_empty() {
    return Err("书名不能为空".into());
  }
  let matches: Vec<ingestion::SuggestionMatch> = to_suggestion_matches(matched.unwrap_or_default());
  let douban_links = douban_links.unwrap_or_default();

  // 注册取消令牌；进度经 task-progress 事件推送到前端实时渲染
  let cancel = task_id
    .as_deref()
    .map(|id| register_task(&registry, id))
    .unwrap_or_default();
  let emitter = app.clone();
  let progress = move |p: ingestion::TaskProgress| {
    let _ = emitter.emit("task-progress", &p);
  };

  // 网络 + 文件操作不持锁，避免阻塞其他查询命令
  let mut prepared = match ingestion::prepare_import(
    PathBuf::from(&path),
    original_path.as_deref(),
    &title,
    author.trim(),
    &tags.unwrap_or_default(),
    &matches,
    &douban_links,
    merge_summaries.unwrap_or(false),
    &cancel,
    &progress,
  )
  .await
  {
    Ok(p) => p,
    Err(ingestion::IngestionError::Cancelled) => {
      finish_task(&registry, task_id.as_deref());
      return Err(CANCELLED_MSG.into());
    }
    Err(e) => {
      finish_task(&registry, task_id.as_deref());
      return Err(e.to_string());
    }
  };
  finish_task(&registry, task_id.as_deref());

  // 落盘（复制入书库 + 写 EPUB 副本；快速路径无封面上传）
  ingestion::finalize_import(&mut prepared, None).map_err(|e| e.to_string())?;
  finish(&state, prepared)
}

/// 导入预处理结果（确认弹窗数据源）
#[derive(Serialize)]
pub struct DoubanPreviewDto {
  pub title: Option<String>,
  pub author: Option<String>,
  pub cover_path: Option<String>,
  pub cover_url: Option<String>,
}

#[derive(Serialize)]
pub struct PreparedImportDto {
  pub task_id: String,
  pub title: String,
  pub author: String,
  pub tags: Vec<String>,
  pub description: String,
  pub fusion_model: Option<String>,
  pub fusion_error: Option<String>,
  pub cover_path: Option<String>,
  pub comments: usize,
  /// 系列信息（claspclub 详情继承；确认弹窗可编辑）
  pub series_name: Option<String>,
  pub series_order: Option<i64>,
  /// 豆瓣首来源（无 clasp 匹配时的封面/标题/作者预览）
  pub douban_preview: Option<DoubanPreviewDto>,
}

/// 导入预处理（交互式确认弹窗前完成全部爬取与简介合并；不写书库与数据库）
///
/// 进度经 task-progress 实时推送（可打断）；结果暂存于 ImportCache，
/// 前端确认后调用 commit_import 落盘入库，取消时调用 discard_import 丢弃。
#[tauri::command]
pub async fn prepare_import(
  registry: State<'_, TaskRegistry>,
  cache: State<'_, ImportCache>,
  app: tauri::AppHandle,
  path: String,
  original_path: Option<String>,
  title: String,
  author: String,
  tags: Option<Vec<String>>,
  matched: Option<Vec<MatchInfo>>,
  douban_links: Option<Vec<String>>,
  merge_summaries: Option<bool>,
  task_id: Option<String>,
) -> Result<PreparedImportDto, String> {
  let matches: Vec<ingestion::SuggestionMatch> = to_suggestion_matches(matched.unwrap_or_default());
  let douban_links = douban_links.unwrap_or_default();

  let cancel = task_id
    .as_deref()
    .map(|id| register_task(&registry, id))
    .unwrap_or_default();
  let emitter = app.clone();
  let progress = move |p: ingestion::TaskProgress| {
    let _ = emitter.emit("task-progress", &p);
  };

  let prepared = ingestion::prepare_import(
    PathBuf::from(&path),
    original_path.as_deref(),
    &title,
    author.trim(),
    &tags.unwrap_or_default(),
    &matches,
    &douban_links,
    merge_summaries.unwrap_or(false),
    &cancel,
    &progress,
  )
  .await;
  finish_task(&registry, task_id.as_deref());
  let prepared = match prepared {
    Ok(p) => p,
    Err(ingestion::IngestionError::Cancelled) => return Err(CANCELLED_MSG.into()),
    Err(e) => return Err(e.to_string()),
  };

  let task_id = task_id.unwrap_or_else(|| format!("prep-{}", unix_secs()));
  let douban_preview = prepared.sources_douban.first().map(|s| DoubanPreviewDto {
    title: s.title.clone(),
    author: s.author.clone(),
    cover_path: s.cover_path.as_ref().map(|p| p.to_string_lossy().into_owned()),
    cover_url: s.cover_url.clone(),
  });
  let dto = PreparedImportDto {
    task_id: task_id.clone(),
    title: prepared.title.clone(),
    author: prepared.author.clone(),
    tags: prepared.tags.clone(),
    description: prepared.description.clone(),
    fusion_model: prepared.fusion_model.clone(),
    fusion_error: prepared.fusion_error.clone(),
    cover_path: prepared
      .cover_path
      .as_ref()
      .map(|p| p.to_string_lossy().into_owned()),
    comments: prepared
      .sources_clasp
      .iter()
      .chain(prepared.sources_douban.iter())
      .map(|s| s.comments.len())
      .sum(),
    series_name: prepared.series_name.clone(),
    series_order: prepared.series_order,
    douban_preview,
  };
  cache.0.lock().map_err(|e| e.to_string())?.insert(task_id, prepared);
  Ok(dto)
}

/// 为待确认导入设置手动上传封面（提交时复制入封面缓存并采用）
#[tauri::command]
pub fn set_pending_cover(
  cache: State<'_, ImportCache>,
  task_id: String,
  image_path: String,
) -> Result<(), String> {
  let src = PathBuf::from(&image_path);
  if !src.exists() {
    return Err("图片文件不存在".into());
  }
  let ext = src
    .extension()
    .and_then(|e| e.to_str())
    .map(|e| e.to_lowercase())
    .ok_or_else(|| "不支持的图片格式".to_string())?;
  if !COVER_EXTS.contains(&ext.as_str()) {
    return Err(format!("不支持的图片格式 .{ext}（仅支持 jpg/png/webp）"));
  }
  let mut guard = cache.0.lock().map_err(|e| e.to_string())?;
  let session = guard
    .get_mut(&task_id)
    .ok_or_else(|| "导入会话不存在或已过期".to_string())?;
  session.cover_override = Some(src);
  Ok(())
}

/// 提交导入：按确认弹窗中编辑的元数据落盘（复制入书库 + 写 EPUB 副本）并入库
#[tauri::command]
pub async fn commit_import(
  state: State<'_, Mutex<rusqlite::Connection>>,
  cache: State<'_, ImportCache>,
  task_id: String,
  title: String,
  author: String,
  tags: Vec<String>,
  description: Option<String>,
  series_name: Option<String>,
  series_order: Option<i64>,
) -> Result<ImportResultDto, String> {
  let title = title.trim().to_string();
  if title.is_empty() {
    return Err("书名不能为空".into());
  }
  let mut p = cache
    .0
    .lock()
    .map_err(|e| e.to_string())?
    .remove(&task_id)
    .ok_or_else(|| "导入会话不存在或已过期".to_string())?;

  // 应用确认弹窗中的编辑（标签规范化 + 当前书库固定标签）
  let mut tag_list: Vec<String> =
    tags.iter().map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect();
  for t in AppConfig::load().default_tags() {
    if !tag_list.contains(&t) {
      tag_list.push(t);
    }
  }
  p.title = title;
  p.author = author.trim().to_string();
  p.tags = tag_list;
  p.description = description.unwrap_or_default();
  // 系列：空白系列名视为无系列
  p.series_name = series_name
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty());
  p.series_order = p.series_name.as_ref().map(|_| series_order.unwrap_or(1));

  // 落盘为阻塞本地 IO，放到阻塞线程池避免卡 UI
  let p = tauri::async_runtime::spawn_blocking(move || {
    let cover_override = p.cover_override.clone();
    ingestion::finalize_import(&mut p, cover_override.as_deref())?;
    Ok::<_, ingestion::IngestionError>(p)
  })
  .await
  .map_err(|e| e.to_string())?
  .map_err(|e| e.to_string())?;

  finish(&state, p)
}

/// 丢弃待确认的导入会话（取消导入时调用）
#[tauri::command]
pub fn discard_import(cache: State<'_, ImportCache>, task_id: String) -> bool {
  cache
    .0
    .lock()
    .map(|mut g| g.remove(&task_id).is_some())
    .unwrap_or(false)
}

/// 入库尾段（持久化 + 结果 DTO 组装）
fn finish(
  state: &State<'_, Mutex<rusqlite::Connection>>,
  p: ingestion::PreparedImport,
) -> Result<ImportResultDto, String> {
  let comments = p
    .sources_clasp
    .iter()
    .chain(p.sources_douban.iter())
    .map(|s| s.comments.len())
    .sum();
  let partial = ImportResultDto {
    id: 0,
    title: p.title.clone(),
    author: p.author.clone(),
    library_file: p.library_file.clone(),
    cover_path: p
      .cover_path
      .as_ref()
      .map(|c| c.to_string_lossy().into_owned()),
    comments,
    fusion_model: p.fusion_model.clone(),
    fusion_error: p.fusion_error.clone(),
  };
  let conn = state.lock().map_err(|e| e.to_string())?;
  let id = ingestion::persist_import(&conn, &p).map_err(|e| e.to_string())?;
  Ok(ImportResultDto { id, ..partial })
}

// ============================= //
//  长任务注册表（取消令牌）
// ============================= //

/// 长任务注册表：task_id → 取消令牌（配合前端"打断"按钮）
#[derive(Default)]
pub struct TaskRegistry(Mutex<HashMap<String, ingestion::CancelToken>>);

/// 注册任务并返回其取消令牌
fn register_task(registry: &TaskRegistry, id: &str) -> ingestion::CancelToken {
  let token = ingestion::CancelToken::new();
  registry
    .0
    .lock()
    .unwrap_or_else(|e| e.into_inner())
    .insert(id.to_string(), token.clone());
  token
}

/// 任务结束（成功/失败/取消）后注销
fn finish_task(registry: &TaskRegistry, id: Option<&str>) {
  if let Some(id) = id {
    registry
      .0
      .lock()
      .unwrap_or_else(|e| e.into_inner())
      .remove(id);
  }
}

/// 取消一个进行中的长任务（导入 / 批量导入等）
#[tauri::command]
pub fn cancel_task(registry: State<'_, TaskRegistry>, task_id: String) -> bool {
  let guard = registry.0.lock().unwrap_or_else(|e| e.into_inner());
  match guard.get(&task_id) {
    Some(token) => {
      token.cancel();
      true
    }
    None => false,
  }
}

/// WebDav 同步任务注册表：task_id → 取消令牌
///
/// 与导入 TaskRegistry 分离：同步令牌为 webdav::SyncCancel（核心层自包含，
/// 不与导入的 ingestion::CancelToken 互相依赖）
#[derive(Default)]
pub struct SyncTaskRegistry(Mutex<HashMap<String, mystery_novel_agent::webdav::SyncCancel>>);

/// 注册同步任务并返回其取消令牌
fn register_sync_task(
  registry: &SyncTaskRegistry,
  id: &str,
) -> mystery_novel_agent::webdav::SyncCancel {
  let token = mystery_novel_agent::webdav::SyncCancel::new();
  registry
    .0
    .lock()
    .unwrap_or_else(|e| e.into_inner())
    .insert(id.to_string(), token.clone());
  token
}

/// 同步任务结束后注销
fn finish_sync_task(registry: &SyncTaskRegistry, id: Option<&str>) {
  if let Some(id) = id {
    registry
      .0
      .lock()
      .unwrap_or_else(|e| e.into_inner())
      .remove(id);
  }
}

/// 打断一个进行中的 WebDav 同步任务
#[tauri::command]
pub fn cancel_sync_task(
  registry: State<'_, SyncTaskRegistry>,
  task_id: String,
) -> bool {
  let guard = registry.0.lock().unwrap_or_else(|e| e.into_inner());
  match guard.get(&task_id) {
    Some(token) => {
      token.cancel();
      true
    }
    None => false,
  }
}

/// GUI 搜索页：claspclub 分页搜索（pageSize=5，按豆瓣评分排序，结果含简介/封面）
#[tauri::command]
pub async fn search_clasp(keyword: String, page: Option<i64>) -> Result<SearchPageDto, String> {
  let kw = keyword.trim();
  if kw.is_empty() {
    return Err("搜索关键词不能为空".into());
  }
  let page = page.unwrap_or(1).max(1);
  let resp = ingestion::search_clasp_page(kw, page)
    .await
    .map_err(|e| e.to_string())?;
  Ok(SearchPageDto {
    items: resp.items.iter().map(MatchDto::from_suggestion).collect(),
    page,
    total_pages: resp.total_pages,
    total: resp.total,
    fuzzy: resp.fuzzy,
  })
}

/// 代理下载远程封面图片（伪装 Referer 过防盗链），以原始字节返回给前端渲染
#[tauri::command]
pub async fn fetch_cover_image(url: String) -> Result<tauri::ipc::Response, String> {
  let bytes = ingestion::fetch_remote_image(&url)
    .await
    .map_err(|e| e.to_string())?;
  Ok(tauri::ipc::Response::new(bytes))
}

/// 读取本地封面缓存文件字节（封面统一经此通道渲染）
///
/// 走 invoke + blob 而非 asset 协议：Android WebView 下 asset 协议对
/// 绝对路径（/storage 或应用私有目录）不可靠，命令通道全平台一致。
/// 安全校验：仅允许读取各书库 covers 目录内的图片文件。
#[tauri::command]
pub async fn read_cover_file(path: String) -> Result<tauri::ipc::Response, String> {
  let cfg = AppConfig::load();
  let mut allowed: Vec<PathBuf> = cfg
    .libraries
    .iter()
    .map(|l| l.path.join("covers"))
    .collect();
  if let Ok(d) = cfg.covers_dir() {
    allowed.push(d);
  }
  if let Some(cp) = &cfg.covers_path {
    allowed.push(cp.clone());
  }
  // 文件 IO 与路径校验放阻塞线程（封面为小文件，短暂占用无碍）
  tauri::async_runtime::spawn_blocking(move || {
    let p = PathBuf::from(path.trim());
    let canonical = dunce::canonicalize(&p).map_err(|_| "封面文件不存在".to_string())?;
    let in_scope = allowed
      .iter()
      .filter_map(|d| dunce::canonicalize(d).ok())
      .any(|d| canonical.starts_with(&d));
    if !in_scope {
      return Err("路径不在封面缓存目录内".into());
    }
    let ext = p
      .extension()
      .and_then(|e| e.to_str())
      .map(|e| e.to_ascii_lowercase())
      .unwrap_or_default();
    if !matches!(ext.as_str(), "jpg" | "jpeg" | "png" | "webp" | "gif") {
      return Err("不支持的图片格式".into());
    }
    std::fs::read(&canonical).map_err(|e| format!("封面读取失败: {e}"))
  })
  .await
  .map_err(|e| format!("封面读取失败: {e}"))?
  .map(tauri::ipc::Response::new)
}

/// 递归收集目录下所有 EPUB 文件（按路径排序，供批量加书）
#[tauri::command]
pub fn collect_epubs(dir: String) -> Result<Vec<String>, String> {
  if !Path::new(&dir).is_dir() {
    return Err(format!("不是有效的文件夹: {dir}"));
  }
  let mut out: Vec<String> = Vec::new();
  collect_epubs_impl(Path::new(&dir), &mut out).map_err(|e| e.to_string())?;
  out.sort();
  Ok(out)
}

fn collect_epubs_impl(dir: &Path, out: &mut Vec<String>) -> std::io::Result<()> {
  for entry in std::fs::read_dir(dir)? {
    let path = entry?.path();
    if path.is_dir() {
      collect_epubs_impl(&path, out)?;
    } else if path
      .extension()
      .map(|x| x.eq_ignore_ascii_case("epub"))
      .unwrap_or(false)
    {
      out.push(path.to_string_lossy().into_owned());
    }
  }
  Ok(())
}

/// 删除书籍：书库 EPUB 副本删除，封面按引用计数清理，原始文件永不触碰
#[tauri::command]
pub async fn delete_book(
  state: State<'_, Mutex<rusqlite::Connection>>,
  id: i64,
) -> Result<(), String> {
  // 读取文件路径（短暂持锁）
  let (library_file, covers_dir) = {
    let conn = state.lock().map_err(|e| e.to_string())?;
    let d = db::get_book_detail(&conn, id)
      .map_err(|e| e.to_string())?
      .ok_or_else(|| "书籍不存在".to_string())?;
    (d.library_file, AppConfig::load().covers_dir().ok())
  };

  let cfg = AppConfig::load();

  // 删除书库中的 EPUB 副本（file_path 指向的原始文件不动；失败降级继续删记录）
  if let Some(lib) = cfg.library_path.as_ref() {
    if let Some(f) = library_file.as_deref().filter(|f| !f.is_empty()) {
      let p = lib.join(f);
      if p.is_file() {
        if let Err(e) = std::fs::remove_file(&p) {
          warn!("书库文件删除失败（继续删除记录）: {e}");
        }
      }
    }
  }

  // 删除记录并释放封面引用（计数归零才删文件）
  let conn = state.lock().map_err(|e| e.to_string())?;
  let orphaned = db::delete_book(&conn, id).map_err(|e| e.to_string())?;
  if let Some(dir) = covers_dir {
    release_cover_files(&conn, orphaned, &dir);
  }
  Ok(())
}

/// 解析书籍 EPUB 路径：书库副本（按书籍归属书库定位目录）优先，原始文件兜底。
///
/// 多书库下旧逻辑用 `library_path`（遗留单书库字段）会定位错目录，
/// 移动端新建书库时该字段为空导致"未找到本地 EPUB 文件"。
fn resolve_book_epub(
  cfg: &AppConfig,
  library_file: &str,
  file_path: &str,
  book_library_id: Option<&str>,
) -> Option<PathBuf> {
  // 目录优先级：书籍归属书库 → 当前书库 → 遗留单书库字段
  let lib_dir = cfg
    .libraries
    .iter()
    .find(|l| Some(l.id.as_str()) == book_library_id)
    .or_else(|| {
      cfg
        .libraries
        .iter()
        .find(|l| Some(l.id.as_str()) == cfg.current_library.as_deref())
    })
    .map(|l| l.path.clone())
    .or_else(|| cfg.library_path.clone());

  let mut candidates: Vec<PathBuf> = Vec::new();
  if let Some(dir) = lib_dir {
    let f = library_file.trim();
    if !f.is_empty() {
      candidates.push(dir.join(f));
    }
  }
  if !file_path.trim().is_empty() {
    candidates.push(PathBuf::from(file_path));
  }
  candidates.into_iter().find(|p| p.is_file())
}

/// 打开本地 EPUB 文件（书库副本优先，原始导入文件兜底；系统默认应用）
#[tauri::command]
pub async fn open_book_file(
  app: tauri::AppHandle,
  state: State<'_, Mutex<rusqlite::Connection>>,
  id: i64,
) -> Result<(), String> {
  use tauri_plugin_opener::OpenerExt;

  let (library_file, file_path, book_lib) = {
    let conn = state.lock().map_err(|e| e.to_string())?;
    let d = db::get_book_detail(&conn, id)
      .map_err(|e| e.to_string())?
      .ok_or_else(|| "书籍不存在".to_string())?;
    (d.library_file, d.file_path, d.library_id)
  };

  let cfg = AppConfig::load();
  let target = resolve_book_epub(
    &cfg,
    library_file.as_deref().unwrap_or(""),
    file_path.as_deref().unwrap_or(""),
    book_lib.as_deref(),
  )
  .ok_or_else(|| "未找到本地 EPUB 文件（书库副本与原始文件均不存在）".to_string())?;
  app
    .opener()
    .open_path(target.to_string_lossy().into_owned(), None::<&str>)
    .map_err(|e| format!("打开文件失败: {e}"))
}

/// 阅读器章节数据
#[derive(Serialize)]
pub struct ReaderChapterDto {
  pub title: String,
  pub html: String,
}

/// 阅读器内容（书名 + 全部章节）
#[derive(Serialize)]
pub struct ReaderContentDto {
  pub title: String,
  pub chapters: Vec<ReaderChapterDto>,
}

/// 内置阅读器：提取书籍 EPUB 的章节正文（书库副本优先，原始文件兜底）
#[tauri::command]
pub async fn get_reader_content(
  state: State<'_, Mutex<rusqlite::Connection>>,
  id: i64,
) -> Result<ReaderContentDto, String> {
  let (library_file, file_path, book_lib) = {
    let conn = state.lock().map_err(|e| e.to_string())?;
    let d = db::get_book_detail(&conn, id)
      .map_err(|e| e.to_string())?
      .ok_or_else(|| "书籍不存在".to_string())?;
    (d.library_file, d.file_path, d.library_id)
  };

  let cfg = AppConfig::load();
  let target = resolve_book_epub(
    &cfg,
    library_file.as_deref().unwrap_or(""),
    file_path.as_deref().unwrap_or(""),
    book_lib.as_deref(),
  )
  .ok_or_else(|| "未找到本地 EPUB 文件（书库副本与原始文件均不存在）".to_string())?;

  // 大书解析为 CPU 密集操作，放阻塞线程池避免卡 UI
  let content = tauri::async_runtime::spawn_blocking(move || {
    mystery_novel_agent::reader::extract_content(&target)
  })
  .await
  .map_err(|e| format!("解析任务失败: {e}"))?
  .map_err(|e| e.to_string())?;

  Ok(ReaderContentDto {
    title: content.title,
    chapters: content
      .chapters
      .into_iter()
      .map(|c| ReaderChapterDto { title: c.title, html: c.html })
      .collect(),
  })
}

/// 详情页元数据编辑：更新数据库并同步 EPUB 书库副本（失败降级为仅更新 DB）
#[tauri::command]
pub async fn update_book_meta(
  state: State<'_, Mutex<rusqlite::Connection>>,
  id: i64,
  title: String,
  author: String,
  tags: String,
  description: Option<String>,
  series_name: Option<String>,
  series_order: Option<i64>,
) -> Result<(), String> {
  let title = title.trim().to_string();
  if title.is_empty() {
    return Err("书名不能为空".into());
  }
  // 标签规范化 + 当前书库固定标签
  let mut tag_list = utils::split_tags(&tags);
  for t in AppConfig::load().default_tags() {
    if !tag_list.contains(&t) {
      tag_list.push(t);
    }
  }
  let desc = description.map(|d| d.trim().to_string()).filter(|d| !d.is_empty());
  // 系列：空白系列名视为无系列
  let series_name = series_name
    .map(|s| s.trim().to_string())
    .filter(|s| !s.is_empty());
  let series_order = series_name.as_ref().map(|_| series_order.unwrap_or(1));

  // 单次持锁：读取 library_file + 更新数据库（含系列）
  let library_file = {
    let conn = state.lock().map_err(|e| e.to_string())?;
    let d = db::get_book_detail(&conn, id)
      .map_err(|e| e.to_string())?
      .ok_or_else(|| "书籍不存在".to_string())?;
    db::update_book_meta(&conn, id, &title, author.trim(), &utils::join_tags(&tag_list), desc.as_deref())
      .map_err(|e| e.to_string())?;
    db::set_book_series(&conn, id, series_name.as_deref(), series_order)
      .map_err(|e| e.to_string())?;
    d.library_file
  };

  // 同步 EPUB 书库副本（不修改原文件；失败仅降级更新数据库）
  if let Some(file) = library_file.as_deref().filter(|f| !f.is_empty()) {
    if let Some(lib) = AppConfig::load().library_path.as_ref() {
      let series = series_name.as_deref().map(|n| (n, series_order.unwrap_or(1)));

      // 书名/作者变更 → 按命名规则重命名书库文件
      let mut epub_path = lib.join(file);
      let new_name = mystery_novel_agent::library::to_ascii_filename(
        author.trim(),
        &title,
        series,
      );
      if new_name != file {
        let dest = mystery_novel_agent::library::resolve_collision(&lib, &new_name);
        if std::fs::rename(&epub_path, &dest).is_ok() {
          info!("书库文件已重命名: {file} → {}", dest.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default());
          epub_path = dest;
          let conn = state.lock().map_err(|e| e.to_string())?;
          conn
            .execute(
              "UPDATE books SET library_file = ?1 WHERE id = ?2",
              rusqlite::params![
                epub_path.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default(),
                id
              ],
            )
            .map_err(|e| e.to_string())?;
        } else {
          warn!("书库文件重命名失败（保留原文件名）: {file}");
        }
      }

      if epub_path.exists() {
        if let Err(e) = ingestion::write_epub_metadata(
          &epub_path,
          &title,
          author.trim(),
          &tag_list,
          desc.as_deref(),
          series,
          None,
        ) {
          warn!("EPUB 元数据同步失败（数据库已更新）: {e}");
        }
      }
    }
  }
  Ok(())
}

/// 上传封面支持的图片格式
const COVER_EXTS: [&str; 4] = ["jpg", "jpeg", "png", "webp"];

/// 当前 Unix 时间戳（秒；时钟异常时回退 0）
fn unix_secs() -> u64 {
  std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map(|d| d.as_secs())
    .unwrap_or(0)
}

/// 封面替换结果
#[derive(Serialize)]
pub struct CoverUpdateDto {
  pub cover_path: String,
}

/// 封面同步所需的书籍元数据（详情行中与本任务相关的字段）
struct CoverSyncInfo {
  title: String,
  author: String,
  tags: String,
  description: Option<String>,
  series_name: Option<String>,
  series_order: Option<i64>,
  library_file: Option<String>,
}

/// 读取封面同步所需的书籍元数据（短暂持锁调用）
fn read_cover_sync_info(
  conn: &rusqlite::Connection,
  book_id: i64,
) -> Result<CoverSyncInfo, String> {
  let d = db::get_book_detail(conn, book_id)
    .map_err(|e| e.to_string())?
    .ok_or_else(|| "书籍不存在".to_string())?;
  Ok(CoverSyncInfo {
    title: d.title,
    author: d.author,
    tags: d.tags,
    description: d.description,
    series_name: d.series_name,
    series_order: d.series_order,
    library_file: d.library_file,
  })
}

/// 释放封面引用；计数归零且无残留引用时删除缓存文件（仅限 covers 目录内）
fn release_cover_file(conn: &rusqlite::Connection, path: &str, covers_dir: &Path) {
  match db::cover_ref_release(conn, path) {
    Ok(true) => {
      let p = Path::new(path);
      if p.starts_with(covers_dir) && p.is_file() {
        let _ = std::fs::remove_file(p);
      }
    }
    Ok(false) => {}
    Err(e) => warn!("封面引用释放失败 [{path}]: {e}"),
  }
}

/// 释放一组封面引用（delete_book / clear_sources 返回的孤儿路径）
fn release_cover_files(conn: &rusqlite::Connection, paths: Vec<String>, covers_dir: &Path) {
  for p in paths {
    release_cover_file(conn, &p, covers_dir);
  }
}

/// 启动清扫：删除封面缓存目录中无任何引用登记的文件（历史版本遗留的孤儿文件）
pub fn cleanup_covers_dir(conn: &rusqlite::Connection, dir: &Path) {
  let Ok(entries) = std::fs::read_dir(dir) else {
    return;
  };
  for entry in entries.flatten() {
    let p = entry.path();
    if !p.is_file() {
      continue;
    }
    let s = p.to_string_lossy().into_owned();
    match db::cover_has_ref(conn, &s) {
      Ok(true) => {}
      Ok(false) => {
        let _ = std::fs::remove_file(&p);
      }
      Err(e) => warn!("封面引用检查失败 [{s}]: {e}"),
    }
  }
}

/// 封面字节落盘 → 同步 EPUB 副本嵌入封面 → 更新数据库（引用计数转移）
///
/// 降级链：封面缓存目录不可用 → 写入临时目录；EPUB 写入失败 → 仅更新数据库。
async fn store_cover(
  state: State<'_, Mutex<rusqlite::Connection>>,
  book_id: i64,
  bytes: Vec<u8>,
  ext: &str,
) -> Result<CoverUpdateDto, String> {
  // 读取书籍信息（短暂持锁，读完立即释放）
  let info = {
    let conn = state.lock().map_err(|e| e.to_string())?;
    read_cover_sync_info(&conn, book_id)?
  };

  let cfg = AppConfig::load();
  let dir = match cfg.covers_dir() {
    Ok(d) => d,
    Err(e) => {
      warn!("封面缓存目录不可用: {e}，降级写入临时目录");
      std::env::temp_dir()
    }
  };
  // 内容去重：相同图片复用同一文件（引用计数各自登记）
  let ext = if ext == "jpeg" { "jpg" } else { ext };
  let dest = ingestion::save_cover_dedup(&dir, &bytes, ext)
    .map_err(|e| format!("封面写入失败: {e}"))?;
  let new_cover = dest.to_string_lossy().into_owned();

  // 同步 EPUB 书库副本的嵌入封面（目标为副本，原文件不动；失败仅降级更新 DB）
  if let Some(file) = info.library_file.as_deref().filter(|f| !f.is_empty()) {
    if let Some(lib) = cfg.library_path.as_ref() {
      let epub_path = lib.join(file);
      if epub_path.exists() {
        let series = info
          .series_name
          .as_deref()
          .map(|n| (n, info.series_order.unwrap_or(1)));
        if let Err(e) = ingestion::write_epub_metadata(
          &epub_path,
          &info.title,
          &info.author,
          &utils::split_tags(&info.tags),
          info.description.as_deref(),
          series,
          Some(&dest),
        ) {
          warn!("EPUB 嵌入封面更新失败（仅更新数据库）: {e}");
        }
      }
    }
  }

  // 数据库更新（新封面登记引用，旧封面释放；归零时删除文件）
  let orphaned = {
    let conn = state.lock().map_err(|e| e.to_string())?;
    db::update_book_cover(&conn, book_id, &new_cover).map_err(|e| e.to_string())?
  };
  if let Some(old) = orphaned {
    let conn = state.lock().map_err(|e| e.to_string())?;
    release_cover_file(&conn, &old, &dir);
  }
  Ok(CoverUpdateDto { cover_path: new_cover })
}

/// 手动上传封面：读取图片字节 → 共用落盘/同步链（EPUB 副本嵌入 + 数据库更新）
#[tauri::command]
pub async fn upload_cover(
  state: State<'_, Mutex<rusqlite::Connection>>,
  book_id: i64,
  image_path: String,
) -> Result<CoverUpdateDto, String> {
  let src = PathBuf::from(&image_path);
  if !src.exists() {
    return Err("图片文件不存在".into());
  }
  let ext = src
    .extension()
    .and_then(|e| e.to_str())
    .map(|e| e.to_lowercase())
    .ok_or_else(|| "不支持的图片格式".to_string())?;
  if !COVER_EXTS.contains(&ext.as_str()) {
    return Err(format!("不支持的图片格式 .{ext}（仅支持 jpg/png/webp）"));
  }
  let bytes = std::fs::read(&src).map_err(|e| format!("封面读取失败: {e}"))?;
  let ext = if ext == "jpeg" { "jpg" } else { ext.as_str() };
  store_cover(state, book_id, bytes, ext).await
}

// ============================= //
//  来源管理（claspclub / 豆瓣有序项目）
// ============================= //

/// clasp 版本封面（预爬本地的 editions JSON 元素）
#[derive(Serialize, Deserialize)]
pub struct EditionCoverDto {
  pub label: String,
  pub url: String,
  pub path: Option<String>,
}

/// 来源项目（来源管理界面数据源）
#[derive(Serialize)]
pub struct SourceDto {
  pub kind: String,
  pub ref_key: String,
  pub position: i64,
  pub title: Option<String>,
  pub author: Option<String>,
  pub cover_url: Option<String>,
  pub cover_path: Option<String>,
  pub summary: Option<String>,
  /// 系列信息（仅 claspclub 来源有；旧数据可能为 None）
  pub series_name: Option<String>,
  pub series_order: Option<i64>,
  pub editions: Vec<EditionCoverDto>,
}

fn source_row_to_dto(row: db::SourceRow) -> SourceDto {
  let editions = row
    .editions
    .as_deref()
    .and_then(|s| serde_json::from_str::<Vec<EditionCoverDto>>(s).ok())
    .unwrap_or_default();
  SourceDto {
    kind: row.kind,
    ref_key: row.ref_key,
    position: row.position,
    title: row.title,
    author: row.author,
    cover_url: row.cover_url,
    cover_path: row.cover_path,
    summary: row.summary,
    series_name: row.series_name,
    series_order: row.series_order,
    editions,
  }
}

/// 列出书籍的全部来源项目（clasp 在前、豆瓣在后，各自按顺序）
#[tauri::command]
pub fn list_sources(
  state: State<'_, Mutex<rusqlite::Connection>>,
  book_id: i64,
) -> Result<Vec<SourceDto>, String> {
  let conn = state.lock().map_err(|e| e.to_string())?;
  Ok(
    db::list_sources(&conn, book_id)
      .map_err(|e| e.to_string())?
      .into_iter()
      .map(source_row_to_dto)
      .collect(),
  )
}

/// 追加来源项目（含短评入库）并回写 books 来源列
async fn append_pending_sources(
  state: &State<'_, Mutex<rusqlite::Connection>>,
  book_id: i64,
  kind: &str,
  items: Vec<ingestion::PendingSource>,
) -> Result<(), String> {
  let conn = state.lock().map_err(|e| e.to_string())?;
  let start = db::list_sources(&conn, book_id)
    .map_err(|e| e.to_string())?
    .iter()
    .filter(|s| s.kind == kind)
    .count() as i64;
  let source_name = if kind == "clasp" { "claspclub" } else { "豆瓣" };
  for (i, s) in items.iter().enumerate() {
    db::insert_source(
      &conn,
      book_id,
      kind,
      &s.ref_key,
      start + i as i64,
      s.title.as_deref(),
      s.author.as_deref(),
      s.cover_url.as_deref(),
      s.cover_path
        .as_ref()
        .map(|p| p.to_string_lossy().into_owned())
        .as_deref(),
      s.summary.as_deref(),
      s.tags.as_deref().and_then(|t| serde_json::to_string(t).ok()).as_deref(),
      s.series.as_ref().map(|(n, _)| n.as_str()),
      s.series.as_ref().and_then(|(_, o)| *o),
      s.editions_json.as_deref(),
    )
    .map_err(|e| e.to_string())?;
    for c in &s.comments {
      let _ = db::insert_comment(
        &conn,
        book_id,
        c.rating,
        &c.content,
        c.usefulness,
        source_name,
        Some(&s.ref_key),
      );
    }
  }
  db::sync_book_source_columns(&conn, book_id).map_err(|e| e.to_string())
}

/// 从 claspclub 页面链接或纯 ID 提取条目 ID
fn clasp_id_from_input(s: &str) -> Option<String> {
  let s = s.trim();
  if s.is_empty() {
    return None;
  }
  if let Some(pos) = s.find("claspclub.com/books/") {
    let rest = &s[pos + "claspclub.com/books/".len()..];
    let id: String = rest
      .chars()
      .take_while(|c| *c != '/' && *c != '?' && *c != '#')
      .collect();
    return (!id.is_empty()).then_some(id);
  }
  // 纯 ID（不含路径分隔符即可）
  let id = s.trim_start_matches('/');
  (!id.is_empty() && !id.contains('/')).then(|| id.to_string())
}

/// 添加 claspclub 来源（搜索多选 ID 或粘贴多行页面链接）
///
/// 每个项目爬取：详情（书名/作者/简介）、主封面、全部版本封面（预爬本地）、短评 top5。
/// 添加完成后从各项目推断豆瓣链接，新链接按顺序自动补充豆瓣来源并同样爬取。
#[tauri::command]
pub async fn add_clasp_sources(
  state: State<'_, Mutex<rusqlite::Connection>>,
  book_id: i64,
  items: Vec<String>,
) -> Result<Vec<SourceDto>, String> {
  let mut ids: Vec<String> = Vec::new();
  for it in &items {
    if let Some(id) = clasp_id_from_input(it) {
      if !ids.contains(&id) {
        ids.push(id);
      }
    }
  }
  if ids.is_empty() {
    return Err("未提供有效的 claspclub 条目".into());
  }

  let client = ingestion::gui_http_client();

  // 已有来源（避免重复添加）
  let existing: Vec<db::SourceRow> = {
    let conn = state.lock().map_err(|e| e.to_string())?;
    db::list_sources(&conn, book_id).map_err(|e| e.to_string())?
  };

  let mut new_clasp: Vec<ingestion::PendingSource> = Vec::new();
  let mut new_douban_urls: Vec<String> = Vec::new();
  for id in &ids {
    if existing.iter().any(|s| s.kind == "clasp" && s.ref_key == *id) {
      continue;
    }
    let ps = ingestion::crawl_clasp_source(&client, id).await;
    // 豆瓣项目自动按顺序补充（无对应豆瓣页时跳过）
    if let Some(u) = ps.douban_url.clone() {
      let already = existing.iter().any(|s| s.kind == "douban" && s.ref_key == u)
        || new_douban_urls.contains(&u);
      if !already {
        new_douban_urls.push(u);
      }
    }
    new_clasp.push(ps);
  }
  if new_clasp.is_empty() {
    return Err("所选条目均已存在".into());
  }

  // 动态更新标签：新增 clasp 来源的标签并入书籍标签（不影响既有标签）
  let new_tags: Vec<String> = new_clasp
    .iter()
    .filter_map(|s| s.tags.clone())
    .flatten()
    .collect();

  append_pending_sources(&state, book_id, "clasp", new_clasp).await?;
  if !new_tags.is_empty() {
    let conn = state.lock().map_err(|e| e.to_string())?;
    let mut cur: Vec<String> = db::get_book_detail(&conn, book_id)
      .map_err(|e| e.to_string())?
      .map(|d| utils::split_tags(&d.tags))
      .unwrap_or_default();
    for t in new_tags {
      if !cur.contains(&t) {
        cur.push(t);
      }
    }
    for t in AppConfig::load().default_tags() {
      if !cur.contains(&t) {
        cur.push(t);
      }
    }
    db::set_book_tags(&conn, book_id, &utils::join_tags(&cur)).map_err(|e| e.to_string())?;
  }

  // 动态更新系列：所有 clasp 来源系列一致时自动设为书的系列（与导入继承规则一致）
  {
    let conn = state.lock().map_err(|e| e.to_string())?;
    let rows = db::list_sources(&conn, book_id).map_err(|e| e.to_string())?;
    let pairs: Vec<(String, Option<i64>)> = rows
      .iter()
      .filter(|s| s.kind == "clasp")
      .filter_map(|s| s.series_name.clone().map(|n| (n, s.series_order)))
      .collect();
    if let Some((name, order)) = ingestion::inherit_series(&pairs) {
      db::set_book_series(&conn, book_id, Some(name.as_str()), order)
        .map_err(|e| e.to_string())?;
    }
  }

  // 新增豆瓣链接按 clasp 顺序爬取并追加
  let mut new_douban: Vec<ingestion::PendingSource> = Vec::new();
  for url in &new_douban_urls {
    let ps = ingestion::crawl_douban_source(&client, url).await;
    new_douban.push(ps);
  }
  if !new_douban.is_empty() {
    append_pending_sources(&state, book_id, "douban", new_douban).await?;
  }

  list_sources(state, book_id)
}

/// 添加豆瓣来源（粘贴多行书籍页链接；逐条爬取元数据/封面/简介/短评）
#[tauri::command]
pub async fn add_douban_sources(
  state: State<'_, Mutex<rusqlite::Connection>>,
  book_id: i64,
  items: Vec<String>,
) -> Result<Vec<SourceDto>, String> {
  let mut urls: Vec<String> = Vec::new();
  for it in &items {
    let u = it.trim();
    if u.is_empty() {
      continue;
    }
    if !u.contains("book.douban.com/subject/") {
      return Err(format!("不是有效的豆瓣书籍链接: {u}"));
    }
    if !urls.contains(&u.to_string()) {
      urls.push(u.to_string());
    }
  }
  if urls.is_empty() {
    return Err("未提供豆瓣链接".into());
  }

  let existing: Vec<db::SourceRow> = {
    let conn = state.lock().map_err(|e| e.to_string())?;
    db::list_sources(&conn, book_id).map_err(|e| e.to_string())?
  };
  let client = ingestion::gui_http_client();

  let mut new_items: Vec<ingestion::PendingSource> = Vec::new();
  for url in urls {
    if existing.iter().any(|s| s.kind == "douban" && s.ref_key == url) {
      continue;
    }
    new_items
      .push(ingestion::crawl_douban_source(&client, &url).await);
  }
  if new_items.is_empty() {
    return Err("链接均已存在".into());
  }
  append_pending_sources(&state, book_id, "douban", new_items).await?;
  list_sources(state, book_id)
}

/// 保存来源排序（拖拽后回传该类别的有序 ref 列表）
#[tauri::command]
pub fn save_source_order(
  state: State<'_, Mutex<rusqlite::Connection>>,
  book_id: i64,
  kind: String,
  refs: Vec<String>,
) -> Result<(), String> {
  if kind != "clasp" && kind != "douban" {
    return Err("无效的来源类别".into());
  }
  let conn = state.lock().map_err(|e| e.to_string())?;
  db::reorder_sources(&conn, book_id, &kind, &refs).map_err(|e| e.to_string())?;
  db::sync_book_source_columns(&conn, book_id).map_err(|e| e.to_string())
}

/// 删除单个来源项目（其抓取的短评一并删除；封面按引用计数清理）
#[tauri::command]
pub fn delete_source(
  state: State<'_, Mutex<rusqlite::Connection>>,
  book_id: i64,
  kind: String,
  ref_key: String,
) -> Result<Vec<SourceDto>, String> {
  {
    let conn = state.lock().map_err(|e| e.to_string())?;
    let (_, orphaned) =
      db::delete_source(&conn, book_id, &kind, &ref_key).map_err(|e| e.to_string())?;
    db::sync_book_source_columns(&conn, book_id).map_err(|e| e.to_string())?;
    if let Ok(dir) = AppConfig::load().covers_dir() {
      release_cover_files(&conn, orphaned, &dir);
    }
  }
  list_sources(state, book_id)
}

/// 清空某一类来源项目及其短评（封面按引用计数清理）
#[tauri::command]
pub fn clear_sources(
  state: State<'_, Mutex<rusqlite::Connection>>,
  book_id: i64,
  kind: String,
) -> Result<Vec<SourceDto>, String> {
  {
    let conn = state.lock().map_err(|e| e.to_string())?;
    let orphaned = db::clear_sources(&conn, book_id, &kind).map_err(|e| e.to_string())?;
    db::sync_book_source_columns(&conn, book_id).map_err(|e| e.to_string())?;
    if let Ok(dir) = AppConfig::load().covers_dir() {
      release_cover_files(&conn, orphaned, &dir);
    }
  }
  list_sources(state, book_id)
}

/// 从书库 EPUB 副本定位参数同步简介（封面保持不变；失败仅告警）
async fn sync_epub_description(
  state: &State<'_, Mutex<rusqlite::Connection>>,
  book_id: i64,
  description: Option<&str>,
) {
  let info = {
    let conn = match state.lock() {
      Ok(c) => c,
      Err(e) => {
        warn!("读取书籍信息失败（跳过 EPUB 简介）: {e}");
        return;
      }
    };
    match read_cover_sync_info(&conn, book_id) {
      Ok(i) => i,
      Err(e) => {
        warn!("读取书籍信息失败（跳过 EPUB 简介）: {e}");
        return;
      }
    }
  };
  if let Some(file) = info.library_file.as_deref().filter(|f| !f.is_empty()) {
    if let Some(lib) = AppConfig::load().library_path.as_ref() {
      let epub_path = lib.join(file);
      if epub_path.exists() {
        let series = info
          .series_name
          .as_deref()
          .map(|n| (n, info.series_order.unwrap_or(1)));
        if let Err(e) = ingestion::write_epub_metadata(
          &epub_path,
          &info.title,
          &info.author,
          &utils::split_tags(&info.tags),
          description,
          series,
          None,
        ) {
          warn!("EPUB 简介同步失败（数据库已更新）: {e}");
        }
      }
    }
  }
}

/// 重新爬取单个来源项目的短评（同时刷新其元数据/封面/简介/版本封面）
#[tauri::command]
pub async fn refresh_source_comments(
  state: State<'_, Mutex<rusqlite::Connection>>,
  book_id: i64,
  kind: String,
  ref_key: String,
) -> Result<usize, String> {
  let client = ingestion::gui_http_client();
  let ps = match kind.as_str() {
    "clasp" => ingestion::crawl_clasp_source(&client, &ref_key).await,
    "douban" => ingestion::crawl_douban_source(&client, &ref_key).await,
    _ => return Err("无效的来源类别".into()),
  };
  let count = ps.comments.len();
  {
    let conn = state.lock().map_err(|e| e.to_string())?;
    // 替换短评（个人/AI 书评不受影响）
    db::delete_comments_for_source(&conn, book_id, &ref_key).map_err(|e| e.to_string())?;
    let source_name = if kind == "clasp" { "claspclub" } else { "豆瓣" };
    for c in &ps.comments {
      let _ = db::insert_comment(
        &conn,
        book_id,
        c.rating,
        &c.content,
        c.usefulness,
        source_name,
        Some(&ref_key),
      );
    }
    // 刷新来源行元数据（书名/作者/封面/简介/标签/版本封面；变化时转移引用）
    let orphaned = db::update_source_meta(
      &conn,
      book_id,
      &kind,
      &ref_key,
      ps.title.as_deref(),
      ps.author.as_deref(),
      ps.cover_url.as_deref(),
      ps.cover_path
        .as_ref()
        .map(|p| p.to_string_lossy().into_owned())
        .as_deref(),
      ps.summary.as_deref(),
      ps.tags.as_deref().and_then(|t| serde_json::to_string(t).ok()).as_deref(),
      ps.series.as_ref().map(|(n, _)| n.as_str()),
      ps.series.as_ref().and_then(|(_, o)| *o),
      ps.editions_json.as_deref(),
    )
    .map_err(|e| e.to_string())?;
    if let Ok(dir) = AppConfig::load().covers_dir() {
      release_cover_files(&conn, orphaned, &dir);
    }
  }
  Ok(count)
}

/// 更新来源项目数据：重新爬取元数据（书名/作者/封面/简介/版本封面/标签；不动短评）
#[tauri::command]
pub async fn refresh_source_meta(
  state: State<'_, Mutex<rusqlite::Connection>>,
  book_id: i64,
  kind: String,
  ref_key: String,
) -> Result<(), String> {
  let client = ingestion::gui_http_client();
  let ps = match kind.as_str() {
    "clasp" => ingestion::crawl_clasp_source(&client, &ref_key).await,
    "douban" => ingestion::crawl_douban_source(&client, &ref_key).await,
    _ => return Err("无效的来源类别".into()),
  };
  {
    let conn = state.lock().map_err(|e| e.to_string())?;
    let orphaned = db::update_source_meta(
      &conn,
      book_id,
      &kind,
      &ref_key,
      ps.title.as_deref(),
      ps.author.as_deref(),
      ps.cover_url.as_deref(),
      ps.cover_path
        .as_ref()
        .map(|p| p.to_string_lossy().into_owned())
        .as_deref(),
      ps.summary.as_deref(),
      ps.tags.as_deref().and_then(|t| serde_json::to_string(t).ok()).as_deref(),
      ps.series.as_ref().map(|(n, _)| n.as_str()),
      ps.series.as_ref().and_then(|(_, o)| *o),
      ps.editions_json.as_deref(),
    )
    .map_err(|e| e.to_string())?;
    if let Ok(dir) = AppConfig::load().covers_dir() {
      release_cover_files(&conn, orphaned, &dir);
    }
  }
  Ok(())
}

/// 重设书籍标签：按当前 claspclub 来源的标签并集 + 书库默认标签重建
/// （豆瓣来源不含标签信息；旧数据缺标签时自动回拉详情补齐）
#[tauri::command]
pub async fn reset_book_tags(
  state: State<'_, Mutex<rusqlite::Connection>>,
  book_id: i64,
) -> Result<Vec<String>, String> {
  let client = ingestion::gui_http_client();
  // 1. 读取 clasp 来源（持锁收集引用，释放锁后再做网络补齐）
  let clasp_refs: Vec<(String, Option<Vec<String>>)> = {
    let conn = state.lock().map_err(|e| e.to_string())?;
    let rows = db::list_sources(&conn, book_id).map_err(|e| e.to_string())?;
    rows
      .iter()
      .filter(|s| s.kind == "clasp")
      .map(|s| {
        let list: Vec<String> = s
          .tags
          .as_deref()
          .and_then(|j| serde_json::from_str(j).ok())
          .unwrap_or_default();
        (s.ref_key.clone(), (!list.is_empty()).then_some(list))
      })
      .collect()
  };
  // 2. 汇总标签（缺失时回拉详情补齐，结果写回来源行）
  let mut tags: Vec<String> = Vec::new();
  for (ref_key, stored) in clasp_refs {
    let list = match stored {
      Some(l) => l,
      None => {
        // 旧数据缺标签：回拉详情补齐（标签与系列一并写回来源行）
        let detail = spider::fetch_book_detail(&client, &ref_key)
          .await
          .map_err(|e| format!("获取来源 {ref_key} 标签失败: {e}"))?;
        let list: Vec<String> = detail
          .tags
          .iter()
          .map(|t| t.name.trim().to_string())
          .filter(|t| !t.is_empty())
          .collect();
        let series = detail.series.and_then(|s| {
          s.name
            .map(|n| n.trim().to_string())
            .filter(|n| !n.is_empty())
            .map(|n| (n, s.order))
        });
        {
          let conn = state.lock().map_err(|e| e.to_string())?;
          let _ = db::update_source_meta(
            &conn,
            book_id,
            "clasp",
            &ref_key,
            None,
            None,
            None,
            None,
            None,
            Some(&serde_json::to_string(&list).unwrap_or_default()),
            series.as_ref().map(|(n, _)| n.as_str()),
            series.as_ref().and_then(|(_, o)| *o),
            None,
          );
        }
        list
      }
    };
    for t in list {
      if !tags.contains(&t) {
        tags.push(t);
      }
    }
  }
  // 2. 保证书库默认标签
  for t in AppConfig::load().default_tags() {
    if !tags.contains(&t) {
      tags.push(t);
    }
  }
  if tags.is_empty() {
    return Err("claspclub 来源不含任何标签信息".into());
  }

  // 3. 入库 + 同步 EPUB 副本
  {
    let conn = state.lock().map_err(|e| e.to_string())?;
    db::set_book_tags(&conn, book_id, &utils::join_tags(&tags)).map_err(|e| e.to_string())?;
  }
  let info = {
    let conn = state.lock().map_err(|e| e.to_string())?;
    read_cover_sync_info(&conn, book_id)
  };
  if let Ok(info) = info {
    if let Some(file) = info.library_file.as_deref().filter(|f| !f.is_empty()) {
      if let Some(lib) = AppConfig::load().library_path.as_ref() {
        let epub_path = lib.join(file);
        if epub_path.exists() {
          let series = info
            .series_name
            .as_deref()
            .map(|n| (n, info.series_order.unwrap_or(1)));
          if let Err(e) = ingestion::write_epub_metadata(
            &epub_path,
            &info.title,
            &info.author,
            &tags,
            info.description.as_deref(),
            series,
            None,
          ) {
            warn!("EPUB 标签同步失败（数据库已更新）: {e}");
          }
        }
      }
    }
  }
  Ok(tags)
}

/// 重设书籍系列：按 claspclub 来源的系列信息继承规则重建（与导入一致）
///
/// 旧数据缺系列时自动回拉详情补齐并写回来源行；返回（系列名, 卷号）。
#[tauri::command]
pub async fn reset_book_series(
  state: State<'_, Mutex<rusqlite::Connection>>,
  book_id: i64,
) -> Result<(String, Option<i64>), String> {
  // 1. 读取 clasp 来源系列（持锁收集，缺失项释放锁后回拉详情补齐）
  let client = ingestion::gui_http_client();
  let clasp_series: Vec<(String, Option<(String, Option<i64>)>)> = {
    let conn = state.lock().map_err(|e| e.to_string())?;
    db::list_sources(&conn, book_id)
      .map_err(|e| e.to_string())?
      .iter()
      .filter(|s| s.kind == "clasp")
      .map(|s| {
        (
          s.ref_key.clone(),
          s.series_name
            .clone()
            .filter(|n| !n.trim().is_empty())
            .map(|n| (n, s.series_order)),
        )
      })
      .collect()
  };
  let mut pairs: Vec<(String, Option<i64>)> = Vec::new();
  for (ref_key, stored) in clasp_series {
    let pair = match stored {
      Some(p) => Some(p),
      None => {
        let detail = spider::fetch_book_detail(&client, &ref_key)
          .await
          .map_err(|e| format!("获取来源 {ref_key} 系列失败: {e}"))?;
        let pair = detail.series.and_then(|s| {
          s.name
            .map(|n| n.trim().to_string())
            .filter(|n| !n.is_empty())
            .map(|n| (n, s.order))
        });
        if let Some((name, order)) = &pair {
          let conn = state.lock().map_err(|e| e.to_string())?;
          let _ = db::update_source_series(&conn, book_id, "clasp", &ref_key, Some(name), *order);
        }
        pair
      }
    };
    if let Some(p) = pair {
      pairs.push(p);
    }
  }

  // 2. 继承规则判定（与导入一致：系列信息一致才采用）
  let Some((name, order)) = ingestion::inherit_series(&pairs) else {
    return Err("claspclub 来源的系列信息不一致或缺失，无法自动重设".into());
  };

  // 3. 入库 + 同步 EPUB 副本（无卷号时写入 EPUB 默认卷号 1，与导入行为一致）
  {
    let conn = state.lock().map_err(|e| e.to_string())?;
    db::set_book_series(&conn, book_id, Some(name.as_str()), order).map_err(|e| e.to_string())?;
  }
  let info = {
    let conn = state.lock().map_err(|e| e.to_string())?;
    read_cover_sync_info(&conn, book_id)
  };
  if let Ok(info) = info {
    if let Some(file) = info.library_file.as_deref().filter(|f| !f.is_empty()) {
      if let Some(lib) = AppConfig::load().library_path.as_ref() {
        let epub_path = lib.join(file);
        if epub_path.exists() {
          let series = Some((name.as_str(), order.unwrap_or(1)));
          if let Err(e) = ingestion::write_epub_metadata(
            &epub_path,
            &info.title,
            &info.author,
            &utils::split_tags(&info.tags),
            info.description.as_deref(),
            series,
            None,
          ) {
            warn!("EPUB 系列同步失败（数据库已更新）: {e}");
          }
        }
      }
    }
  }
  Ok((name, order))
}

/// 从 URL 推断图片扩展名（无扩展名或不识别时默认 jpg）
fn cover_ext_from_url(url: &str) -> &'static str {
  let clean = url.split(['?', '#']).next().unwrap_or(url);
  let ext = clean.rsplit('.').next().unwrap_or("").to_lowercase();
  match ext.as_str() {
    "png" => "png",
    "webp" => "webp",
    _ => "jpg",
  }
}

/// 将来源项目封面设为本书封面
///
/// clasp 项目可传版本封面 URL；不传则用来源主封面。封面经代理下载
/// （伪装 Referer 过防盗链），内容去重落盘后作为本书封面。
#[tauri::command]
pub async fn set_source_as_cover(
  state: State<'_, Mutex<rusqlite::Connection>>,
  book_id: i64,
  kind: String,
  ref_key: String,
  edition_url: Option<String>,
) -> Result<CoverUpdateDto, String> {
  // 定位来源行（短暂持锁）
  let cover_url = {
    let conn = state.lock().map_err(|e| e.to_string())?;
    let rows = db::list_sources(&conn, book_id).map_err(|e| e.to_string())?;
    rows
      .iter()
      .find(|s| s.kind == kind && s.ref_key == ref_key)
      .and_then(|s| s.cover_url.clone())
      .ok_or_else(|| "来源项目不存在".to_string())?
  };
  let url = edition_url
    .filter(|u| !u.trim().is_empty())
    .unwrap_or(cover_url);
  if url.trim().is_empty() {
    return Err("该项目没有可用封面链接".into());
  }

  let bytes = ingestion::fetch_remote_image(&url)
    .await
    .map_err(|e| e.to_string())?;
  let ext = cover_ext_from_url(&url);
  store_cover(state, book_id, bytes, ext).await
}
/// 将来源项目简介设为本书简介（悬停可预览该项目简介）
#[tauri::command]
pub async fn set_source_as_description(
  state: State<'_, Mutex<rusqlite::Connection>>,
  book_id: i64,
  kind: String,
  ref_key: String,
) -> Result<String, String> {
  let summary = {
    let conn = state.lock().map_err(|e| e.to_string())?;
    let rows = db::list_sources(&conn, book_id).map_err(|e| e.to_string())?;
    rows
      .iter()
      .find(|s| s.kind == kind && s.ref_key == ref_key)
      .and_then(|s| s.summary.clone())
      .filter(|s| !s.trim().is_empty())
      .ok_or_else(|| "该项目没有简介".to_string())?
  };
  {
    let conn = state.lock().map_err(|e| e.to_string())?;
    db::set_book_description(&conn, book_id, Some(&summary)).map_err(|e| e.to_string())?;
  }
  sync_epub_description(&state, book_id, Some(&summary)).await;
  Ok(summary)
}

/// 简介操作结果
#[derive(Serialize)]
pub struct DescriptionDto {
  pub description: String,
  /// 所用模型（未走 LLM 时为 None）
  pub model: Option<String>,
  /// 降级原因
  pub degraded: Option<String>,
}

/// 合并全部来源项目的简介为一段简体中文简介（LLM）
#[tauri::command]
pub async fn merge_source_descriptions(
  state: State<'_, Mutex<rusqlite::Connection>>,
  book_id: i64,
) -> Result<DescriptionDto, String> {
  let summaries: Vec<String> = {
    let conn = state.lock().map_err(|e| e.to_string())?;
    db::list_sources(&conn, book_id)
      .map_err(|e| e.to_string())?
      .into_iter()
      .filter_map(|s| s.summary)
      .filter(|s| !s.trim().is_empty())
      .collect()
  };
  if summaries.len() < 2 {
    return Err("简介来源不足（至少两个项目有简介）".into());
  }
  if agent::resolve_config().is_none() {
    return Err("未配置 LLM（请在设置页配置后重试）".into());
  }
  let fusion = agent::combine_descriptions(&summaries)
    .await
    .map_err(|e| e.to_string())?;
  if let Some((model, usage)) = &fusion.llm {
    let conn = state.lock().map_err(|e| e.to_string())?;
    let _ = db::record_llm_usage(&conn, model, usage.prompt_tokens, usage.completion_tokens, usage.total_tokens);
  }
  {
    let conn = state.lock().map_err(|e| e.to_string())?;
    db::set_book_description(&conn, book_id, Some(&fusion.text)).map_err(|e| e.to_string())?;
  }
  sync_epub_description(&state, book_id, Some(&fusion.text)).await;
  Ok(DescriptionDto {
    description: fusion.text,
    model: fusion.llm.map(|(m, _)| m),
    degraded: fusion.degraded,
  })
}

/// 通用文本翻译为简体中文（LLM；确认弹窗 / 编辑元数据的各文本框翻译按钮）
#[tauri::command]
pub async fn translate_text(
  state: State<'_, Mutex<rusqlite::Connection>>,
  text: String,
) -> Result<String, String> {
  let text = text.trim().to_string();
  if text.is_empty() {
    return Err("内容为空".into());
  }
  if agent::resolve_config().is_none() {
    return Err("未配置 LLM（请在设置页配置后重试）".into());
  }
  let outcome = agent::translate_to_chinese(&text).await.map_err(|e| e.to_string())?;
  {
    let conn = state.lock().map_err(|e| e.to_string())?;
    let _ = db::record_llm_usage(
      &conn,
      &outcome.model,
      outcome.usage.prompt_tokens,
      outcome.usage.completion_tokens,
      outcome.usage.total_tokens,
    );
  }
  Ok(outcome.content)
}

// ============================= //
//  Chatbot（阅读助手）
// ============================= //

/// 截断文本（按字符数，过长补省略号）
fn truncate_for_prompt(s: &str, max: usize) -> String {
  let s = s.trim();
  if s.chars().count() <= max {
    s.to_string()
  } else {
    let t: String = s.chars().take(max).collect();
    format!("{t}…")
  }
}

/// 构建 Chatbot 系统提示：注入当前书库、书籍、简介与短评知识 + 工具使用指引
///
/// 注入量刻意克制（书 40 本、简介/短评/书评截断）：系统提示随书库规模线性膨胀，
/// 过长的 prompt 会显著拉长生成耗时甚至触发网关重置连接
/// （表现为「网络错误: error sending request for url (…/chat/completions)」）。
/// 更多细节由模型经 search_books / get_book_detail 等工具按需检索。
fn build_chatbot_system(conn: &rusqlite::Connection, library_id: Option<&str>) -> String {
  const CHATBOT_KNOWLEDGE_BOOKS: usize = 40;
  let cfg = AppConfig::load();
  let lib_title = cfg
    .current_library()
    .map(|l| l.title.clone())
    .unwrap_or_else(|| "未命名书库".into());
  let books =
    db::search_chatbot_books(
      conn,
      library_id,
      None,
      None,
      None,
      None,
      CHATBOT_KNOWLEDGE_BOOKS as i64,
    )
    .unwrap_or_default();

  let mut knowledge = String::new();
  for (i, b) in books.iter().enumerate() {
    if i >= CHATBOT_KNOWLEDGE_BOOKS {
      knowledge.push_str(&format!("…（其余 {} 本省略，可用工具检索）\n", books.len() - CHATBOT_KNOWLEDGE_BOOKS));
      break;
    }
    knowledge.push_str(&format!(
      "- 《{}》｜作者：{}｜状态：{}{}\n",
      b.title,
      if b.author.is_empty() { "佚名" } else { &b.author },
      b.status,
      b.finished_date
        .as_deref()
        .map(|d| format!("｜读完于 {d}"))
        .unwrap_or_default()
    ));
    if !b.tags.trim().is_empty() {
      knowledge.push_str(&format!("  标签：{}\n", b.tags));
    }
    if let Some(d) = b.description.as_deref().filter(|s| !s.trim().is_empty()) {
      knowledge.push_str(&format!("  简介：{}\n", truncate_for_prompt(d, 80)));
    }
    if let Ok(cs) = db::get_top_comments(conn, b.id, 1) {
      for c in cs {
        knowledge.push_str(&format!("  短评：{}\n", truncate_for_prompt(&c, 60)));
      }
    }
    if let Some(r) = b.my_review.as_deref().filter(|s| !s.trim().is_empty()) {
      knowledge.push_str(&format!("  我的书评：{}\n", truncate_for_prompt(r, 60)));
    }
  }

  format!(
    "你是本地推理小说书架应用的阅读助手「书虫」。用户正在使用「{lib_title}」书库（共 {total} 本书）。\
你可以回答关于这些书的问题、推荐阅读、比较作品、根据状态与短评总结用户的阅读品味。\
回答使用简体中文，语气自然，避免剧透关键结局。\n\n\
注意：书库知识中的「短评」来自网络公开短评（豆瓣 / claspclub 的网友撰写），\
它们代表网络上读者的看法，并不是用户本人的想法；「我的书评」才是用户本人的评论。\n\n\
你可以使用以下本地工具查询书库（需要某本书的完整简介、个人书评、系列信息，\
或按状态/作者/标签检索时，请主动调用工具，不要凭空猜测）：\n\
- search_books：按关键词搜索书籍（匹配书名/作者）\n\
- filter_books：按阅读状态/作者/标签筛选书籍\n\
- get_book_detail：查看某本书的完整详情（book_id 来自搜索/筛选结果）\n\
- get_book_comments：查看某本书的网络短评列表\n\n\
== 书库概览（含每本书的简短简介与短评节选）==\n{knowledge}",
    lib_title = lib_title,
    total = books.len(),
    knowledge = knowledge
  )
}

/// Chatbot 工具定义（OpenAI function calling 格式）
fn chatbot_tools() -> Vec<serde_json::Value> {
  let arr = serde_json::json!([

    {
      "type": "function",
      "function": {
        "name": "search_books",
        "description": "在当前书库中按关键词搜索书籍（匹配书名或作者），返回简要列表（id/书名/作者/状态/标签）",
        "parameters": {
          "type": "object",
          "properties": {
            "keyword": { "type": "string", "description": "书名或作者关键词" },
            "limit": { "type": "integer", "description": "最多返回条数，默认 10，最大 50" }
          },
          "required": ["keyword"]
        }
      }
    },
    {
      "type": "function",
      "function": {
        "name": "filter_books",
        "description": "按阅读状态（想读/在读/已读）、作者、标签筛选当前书库的书籍",
        "parameters": {
          "type": "object",
          "properties": {
            "status": { "type": "string", "description": "阅读状态：想读 / 在读 / 已读" },
            "author": { "type": "string", "description": "作者名（包含匹配）" },
            "tag": { "type": "string", "description": "标签（包含匹配）" },
            "limit": { "type": "integer", "description": "最多返回条数，默认 10，最大 50" }
          }
        }
      }
    },
    {
      "type": "function",
      "function": {
        "name": "get_book_detail",
        "description": "查看某本书的完整详情：简介全文、个人书评、系列、标签、阅读状态",
        "parameters": {
          "type": "object",
          "properties": {
            "book_id": { "type": "integer", "description": "书籍 ID（来自 search_books / filter_books 的结果）" }
          },
          "required": ["book_id"]
        }
      }
    },
    {
      "type": "function",
      "function": {
        "name": "get_book_comments",
        "description": "查看某本书的网络短评列表（豆瓣 / claspclub 网友短评）",
        "parameters": {
          "type": "object",
          "properties": {
            "book_id": { "type": "integer", "description": "书籍 ID" },
            "limit": { "type": "integer", "description": "最多返回条数，默认 5，最大 10" }
          },
          "required": ["book_id"]
        }
      }
    }
  ]);
  match arr {
    serde_json::Value::Array(items) => items,
    _ => Vec::new(),
  }
}

/// 执行 Chatbot 工具调用（本地数据库查询），返回 JSON 字符串给模型
fn execute_chatbot_tool(
  conn: &rusqlite::Connection,
  library_id: Option<&str>,
  name: &str,
  args: &serde_json::Value,
) -> String {
  let get_str = |k: &str| {
    args
      .get(k)
      .and_then(|v| v.as_str())
      .map(|s| s.trim().to_string())
      .filter(|s| !s.is_empty())
  };
  let get_i64 = |k: &str| args.get(k).and_then(|v| v.as_i64());

  let err = |msg: String| serde_json::json!({ "error": msg }).to_string();

  match name {
    "search_books" | "filter_books" => {
      let keyword = get_str("keyword");
      let status = get_str("status");
      let author = get_str("author");
      let tag = get_str("tag");
      let limit = get_i64("limit").unwrap_or(10).clamp(1, 50);
      if name == "search_books" && keyword.is_none() {
        return err("缺少 keyword 参数".into());
      }
      match db::search_chatbot_books(
        conn,
        library_id,
        keyword.as_deref(),
        status.as_deref(),
        author.as_deref(),
        tag.as_deref(),
        limit,
      ) {
        Ok(rows) => {
          let items: Vec<serde_json::Value> = rows
            .iter()
            .map(|b| {
              serde_json::json!({
                "book_id": b.id,
                "title": b.title,
                "author": b.author,
                "status": b.status,
                "tags": b.tags,
              })
            })
            .collect();
          serde_json::json!({ "count": items.len(), "books": items }).to_string()
        }
        Err(e) => err(e.to_string()),
      }
    }
    "get_book_detail" => {
      let Some(id) = get_i64("book_id") else {
        return err("缺少 book_id 参数".into());
      };
      match db::get_book_detail(conn, id) {
        Ok(Some(d)) => serde_json::json!({
          "book_id": d.id,
          "title": d.title,
          "author": d.author,
          "status": d.status,
          "tags": d.tags,
          // 系列展示：卷号已知时 "系列名 #N"，缺卷号只显示系列名
          "series": d.series_name.as_ref().map(|n| match d.series_order {
            Some(o) => format!("{n} #{o}"),
            None => n.clone(),
          }),
          "description": truncate_for_prompt(d.description.as_deref().unwrap_or(""), 800),
          "my_review": truncate_for_prompt(d.my_review.as_deref().unwrap_or(""), 400),
          "finished_date": d.finished_date,
        })
        .to_string(),
        Ok(None) => err("书籍不存在".into()),
        Err(e) => err(e.to_string()),
      }
    }
    "get_book_comments" => {
      let Some(id) = get_i64("book_id") else {
        return err("缺少 book_id 参数".into());
      };
      let limit = get_i64("limit").unwrap_or(5).clamp(1, 10);
      match db::get_comments_for_book(conn, id) {
        Ok(rows) => {
          let items: Vec<serde_json::Value> = rows
            .iter()
            .filter(|c| c.is_mine != 1)
            .take(limit as usize)
            .map(|c| {
              serde_json::json!({
                "source": c.source,
                "rating": c.rating,
                "content": truncate_for_prompt(&c.content, 150),
              })
            })
            .collect();
          serde_json::json!({ "count": items.len(), "comments": items }).to_string()
        }
        Err(e) => err(e.to_string()),
      }
    }
    _ => err(format!("未知工具: {name}")),
  }
}

/// Chatbot 对话：系统提示按数据库实时构建；支持工具调用 agent 循环
/// （模型可主动检索本地书库，最多 CHATBOT_MAX_ROUNDS 轮工具调用）
///
/// - 回复携带 steps：本次任务实际发生的工具调用轨迹（工具名 + 实参 + 结果），
///   前端以可折叠记录展示，避免黑盒
/// - 实时性：每执行完一个工具即推送 `chat-progress` 事件（携带当前 steps），
///   前端在生成期间同步展示工具调用记录，而非只放动画
/// - 可打断：task_id 注册取消令牌（TaskRegistry），前端「打断」按钮经
///   cancel_task 中断整个思考过程（含进行中的 LLM 请求）
#[tauri::command]
pub async fn chatbot_chat(
  state: State<'_, Mutex<rusqlite::Connection>>,
  registry: State<'_, TaskRegistry>,
  app: tauri::AppHandle,
  messages: Vec<ChatMessageDto>,
  task_id: Option<String>,
) -> Result<ChatReplyDto, String> {
  const CHATBOT_MAX_ROUNDS: usize = 5;
  /// 单次 LLM 请求超时：agent 循环的 prompt 含数十 KB 系统提示，30s 极易超时
  const CHATBOT_LLM_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);
  /// 历史消息上限（超出时保留最近的，控制每轮请求体积）
  const CHATBOT_HISTORY_MAX: usize = 16;

  if messages.is_empty() {
    return Err("消息不能为空".into());
  }
  let config = agent::resolve_config()
    .ok_or_else(|| "未配置 LLM（请在设置页配置后重试）".to_string())?;
  let lib_id = AppConfig::load().current_library_id().map(str::to_string);

  let cancel = task_id
    .as_deref()
    .map(|id| register_task(&registry, id))
    .unwrap_or_default();
  let finish = |registry: &TaskRegistry| finish_task(registry, task_id.as_deref());

  // 实时进度：携带当前完整工具轨迹推送给前端
  let emitter = app.clone();
  let progress = {
    let emitter = emitter.clone();
    move |steps: &[ChatStepDto]| {
      let _ = emitter.emit("chat-progress", serde_json::json!({ "steps": steps }));
    }
  };

  // 系统提示（短暂持锁读取 DB）
  let system = {
    let conn = state.lock().map_err(|e| e.to_string())?;
    build_chatbot_system(&conn, lib_id.as_deref())
  };
  // 消息序列：system → user/assistant 交替（忽略前端传入的 system）；
  // 历史超限时只保留最近的 CHATBOT_HISTORY_MAX 条，控制每轮请求体积
  let history: Vec<ChatMessageDto> = messages
    .into_iter()
    .filter(|m| m.role == "user" || m.role == "assistant")
    .collect();
  let history_start = history.len().saturating_sub(CHATBOT_HISTORY_MAX);
  let mut msgs: Vec<agent::Message> = vec![agent::Message::system(system)];
  for m in &history[history_start..] {
    msgs.push(agent::Message {
      role: m.role.clone(),
      content: m.content.clone(),
      tool_calls: None,
      tool_call_id: None,
    });
  }

  let tools = chatbot_tools();
  let mut tools_enabled = true;
  let mut rounds = 0usize;
  let mut steps: Vec<ChatStepDto> = Vec::new();

  loop {
    if cancel.is_cancelled() {
      finish(&registry);
      return Err(CANCELLED_MSG.into());
    }
    // LLM 调用与取消信号竞速：打断时中止进行中的请求
    let call = if tools_enabled {
      agent::chat_with_tools_timeout(&config, &msgs, Some(&tools), CHATBOT_LLM_TIMEOUT)
    } else {
      agent::chat_with_tools_timeout(&config, &msgs, None, CHATBOT_LLM_TIMEOUT)
    };
    let outcome = tokio::select! {
      r = call => r,
      _ = cancel.wait_cancelled() => Err(agent::LlmError::Parse("已取消".into())),
    };
    let outcome = match outcome {
      Ok(o) => o,
      Err(e) => {
        if cancel.is_cancelled() {
          finish(&registry);
          return Err(CANCELLED_MSG.into());
        }
        // 部分兼容 API 不支持 tools：检测到相关报错时降级为纯文本对话重试
        if tools_enabled {
          let lower = e.to_string().to_lowercase();
          let not_supported =
            lower.contains("tool") || lower.contains("function") || lower.contains("http 400");
          if let agent::LlmError::Api(_) = e {
            if not_supported {
              warn!("LLM 不支持工具调用，Chatbot 降级为纯文本对话");
              tools_enabled = false;
              continue;
            }
          }
        }
        finish(&registry);
        return Err(e.to_string());
      }
    };

    // 每轮 token 用量入库
    {
      let conn = state.lock().map_err(|e| e.to_string())?;
      let _ = db::record_llm_usage(
        &conn,
        &outcome.model,
        outcome.usage.prompt_tokens,
        outcome.usage.completion_tokens,
        outcome.usage.total_tokens,
      );
    }

    let Some(calls) = outcome
      .tool_calls
      .clone()
      .filter(|c| !c.is_empty())
      .filter(|_| tools_enabled)
    else {
      finish(&registry);
      return Ok(ChatReplyDto {
        content: outcome.content,
        model: outcome.model,
        steps,
      });
    };

    if rounds >= CHATBOT_MAX_ROUNDS {
      // 轮次上限：不带工具强制模型给出最终回答
      let final_call = agent::chat_with_tools_timeout(&config, &msgs, None, CHATBOT_LLM_TIMEOUT);
      let final_outcome = tokio::select! {
        r = final_call => r,
        _ = cancel.wait_cancelled() => Err(agent::LlmError::Parse("已取消".into())),
      };
      let final_outcome = match final_outcome {
        Ok(o) => o,
        Err(_) => {
          finish(&registry);
          return Err(CANCELLED_MSG.into());
        }
      };
      {
        let conn = state.lock().map_err(|e| e.to_string())?;
        let _ = db::record_llm_usage(
          &conn,
          &final_outcome.model,
          final_outcome.usage.prompt_tokens,
          final_outcome.usage.completion_tokens,
          final_outcome.usage.total_tokens,
        );
      }
      finish(&registry);
      return Ok(ChatReplyDto {
        content: final_outcome.content,
        model: final_outcome.model,
        steps,
      });
    }
    rounds += 1;

    // 回传 assistant 的工具调用请求
    msgs.push(agent::Message {
      role: "assistant".into(),
      content: outcome.content,
      tool_calls: Some(calls.clone()),
      tool_call_id: None,
    });

    // 逐个执行工具并回填结果（轨迹收集至 steps，供前端展示；实时推送）
    {
      let conn = state.lock().map_err(|e| e.to_string())?;
      for call in &calls {
        let call_id = call
          .get("id")
          .and_then(|v| v.as_str())
          .unwrap_or("")
          .to_string();
        let fn_obj = call.get("function");
        let name = fn_obj
          .and_then(|f| f.get("name"))
          .and_then(|v| v.as_str())
          .unwrap_or("")
          .to_string();
        let arg_str = fn_obj
          .and_then(|f| f.get("arguments"))
          .and_then(|v| v.as_str())
          .unwrap_or("{}")
          .to_string();
        let args: serde_json::Value = serde_json::from_str(&arg_str)
          .unwrap_or(serde_json::Value::Object(Default::default()));
        let result = execute_chatbot_tool(&conn, lib_id.as_deref(), &name, &args);
        steps.push(ChatStepDto {
          tool: name.clone(),
          args: arg_str,
          result: result.chars().take(1500).collect(),
        });
        msgs.push(agent::Message {
          role: "tool".into(),
          content: result,
          tool_calls: None,
          tool_call_id: Some(call_id),
        });
        // 实时推送当前工具轨迹（打断检查放在每轮 LLM 调用前）
        progress(&steps);
      }
    }
  }
}

/// GUI 多选合并结果
#[derive(Serialize)]
pub struct MergeResultDto {
  pub id: i64,
  pub title: String,
}

/// GUI 多选合并模式：按顺序合并多本书（EPUB + 来源 + 短评，删除原书）
#[tauri::command]
pub async fn merge_books_gui(
  state: State<'_, Mutex<rusqlite::Connection>>,
  ids: Vec<i64>,
  merge_description: Option<bool>,
) -> Result<MergeResultDto, String> {
  // 阶段一：合并计划（纯读取，短暂持锁）
  let plan = {
    let conn = state.lock().map_err(|e| e.to_string())?;
    merge::plan_merge(&conn, &ids).map_err(|e| e.to_string())?
  };

  // 阶段二：LLM 简介合并（不持锁；取消/未配置/失败 → 回退第一本）
  let mut description: Option<String> = None;
  if plan.descriptions.len() > 1 && merge_description.unwrap_or(false) {
    if agent::resolve_config().is_some() {
      match agent::combine_descriptions(&plan.descriptions).await {
        Ok(fusion) => {
          if let Some((model, usage)) = &fusion.llm {
            let conn = state.lock().map_err(|e| e.to_string())?;
            let _ = db::record_llm_usage(&conn, model, usage.prompt_tokens, usage.completion_tokens, usage.total_tokens);
          }
          description = Some(fusion.text);
        }
        Err(e) => warn!("LLM 简介合并失败，回退第一本: {e}"),
      }
    } else {
      warn!("未配置 LLM，合并书简介取第一本");
    }
  }

  // 阶段三：执行合并（持锁；本地 IO 为主）
  let conn = state.lock().map_err(|e| e.to_string())?;
  let (id, title) =
    merge::apply_merge(&conn, plan, description).map_err(|e| e.to_string())?;
  Ok(MergeResultDto { id, title })
}
// ============================= //
//  设置（LLM Endpoint / API Key / 模型 / 用量）
// ============================= //

/// LLM 可用性（加书弹窗合并简介预检）
#[derive(Serialize)]
pub struct LlmStatusDto {
  pub configured: bool,
  pub model: Option<String>,
}

/// 检测 LLM 是否已配置（设置页 → 环境变量兜底）
#[tauri::command]
pub fn llm_status() -> LlmStatusDto {
  match agent::resolve_config() {
    Some(c) => LlmStatusDto { configured: true, model: Some(c.model) },
    None => LlmStatusDto { configured: false, model: None },
  }
}

/// 设置页的单个 LLM 服务商
#[derive(Serialize, Deserialize)]
pub struct LlmProviderDto {
  #[serde(default)]
  pub name: String,
  #[serde(default)]
  pub base_url: String,
  #[serde(default)]
  pub api_key: String,
  #[serde(default)]
  pub models: Vec<String>,
}

/// 设置页的 LLM 配置（多 Provider）
#[derive(Serialize, Deserialize)]
pub struct LlmSettingsDto {
  #[serde(default)]
  pub providers: Vec<LlmProviderDto>,
  #[serde(default)]
  pub default_provider: Option<String>,
  #[serde(default)]
  pub default_model: Option<String>,
  /// LLM 调用失败重试次数（None = 默认 2）
  #[serde(default)]
  pub retry_count: Option<u32>,
}

/// 设置页完整数据（LLM 配置 + 各模型 token 用量）
#[derive(Serialize)]
pub struct SettingsDto {
  pub llm: LlmSettingsDto,
  pub usage: Vec<db::LlmUsageRow>,
}

/// 读取设置（config.toml llm 段 + DB 用量统计）
#[tauri::command]
pub async fn get_settings(
  state: State<'_, Mutex<rusqlite::Connection>>,
) -> Result<SettingsDto, String> {
  let cfg = AppConfig::load();
  let usage = {
    let conn = state.lock().map_err(|e| e.to_string())?;
    db::get_llm_usage(&conn).map_err(|e| e.to_string())?
  };
  Ok(SettingsDto {
    llm: LlmSettingsDto {
      providers: cfg
        .llm
        .providers
        .iter()
        .map(|p| LlmProviderDto {
          name: p.name.clone(),
          base_url: p.base_url.clone(),
          api_key: p.api_key.clone(),
          models: p.models.clone(),
        })
        .collect(),
      default_provider: cfg.llm.default_provider,
      default_model: cfg.llm.default_model,
      retry_count: cfg.llm.retry_count,
    },
    usage,
  })
}

/// 保存设置到 config.toml（多 Provider）
///
/// 校验：名称非空且不重复；默认服务商必须存在；默认模型归一为所选
/// 服务商下的有效模型（未指定或无效时取其第一个模型）。
#[tauri::command]
pub fn save_settings(settings: LlmSettingsDto) -> Result<(), String> {
  use mystery_novel_agent::config::{LlmProvider, LlmSettings};

  let mut providers: Vec<LlmProvider> = Vec::new();
  for p in settings.providers {
    let name = p.name.trim().to_string();
    if name.is_empty() {
      continue; // 忽略未命名的空 provider
    }
    if providers.iter().any(|x| x.name == name) {
      return Err(format!("服务商名称重复:「{name}」"));
    }
    let mut models: Vec<String> = Vec::new();
    for m in p.models {
      let m = m.trim().to_string();
      if !m.is_empty() && !models.contains(&m) {
        models.push(m);
      }
    }
    providers.push(LlmProvider {
      name,
      base_url: p.base_url.trim().to_string(),
      api_key: p.api_key.trim().to_string(),
      models,
    });
  }

  // 默认服务商：显式指定且存在 → 采用；否则取第一个
  let default_provider = settings
    .default_provider
    .as_deref()
    .map(str::trim)
    .filter(|s| !s.is_empty())
    .and_then(|n| providers.iter().find(|p| p.name == n))
    .map(|p| p.name.clone())
    .or_else(|| providers.first().map(|p| p.name.clone()));

  // 默认模型：归一为默认服务商下的有效模型
  let default_provider_models = default_provider
    .as_deref()
    .and_then(|n| providers.iter().find(|p| p.name == n))
    .map(|p| p.models.clone())
    .unwrap_or_default();
  let default_model = settings
    .default_model
    .as_deref()
    .map(str::trim)
    .filter(|s| !s.is_empty())
    .filter(|m| default_provider_models.iter().any(|x| x == m))
    .map(str::to_string)
    .or_else(|| default_provider_models.first().cloned());

  let mut cfg = AppConfig::load();
  cfg.llm = LlmSettings {
    providers,
    default_provider,
    default_model,
    // 重试次数：0~10，越界截断
    retry_count: settings.retry_count.map(|r| r.min(10)),
    base_url: None,
    api_key: None,
    models: Vec::new(),
  };
  cfg.save().map_err(|e| e.to_string())
}

// ============================= //
//  阅读状态 / AI 短评对话
// ============================= //

/// 切换书籍阅读状态（想读/在读/已读；已读自动记录完成时间）
#[tauri::command]
pub async fn set_book_status(
  state: State<'_, Mutex<rusqlite::Connection>>,
  id: i64,
  status: String,
) -> Result<(), String> {
  if !matches!(status.as_str(), "想读" | "在读" | "已读") {
    return Err(format!("无效的状态:「{status}」"));
  }
  let conn = state.lock().map_err(|e| e.to_string())?;
  db::set_book_status(&conn, id, &status).map_err(|e| e.to_string())
}

/// 通用 LLM 对话消息（GUI AI 书评会话）
#[derive(Deserialize, Serialize)]
pub struct ChatMessageDto {
  pub role: String,
  pub content: String,
}

/// LLM 回复（含所用模型与工具调用轨迹，便于前端展示）
#[derive(Serialize)]
pub struct ChatReplyDto {
  pub content: String,
  pub model: String,
  /// 本次任务的工具调用轨迹（工具名 / 实参 / 结果摘要）；未调用工具时为空
  #[serde(default)]
  pub steps: Vec<ChatStepDto>,
}

/// 一次工具调用记录（轨迹展示）
#[derive(Serialize)]
pub struct ChatStepDto {
  pub tool: String,
  /// 实参 JSON 文本
  pub args: String,
  /// 结果（截断）
  pub result: String,
}

/// 通用 LLM 对话（消息序列必须 system 开头）；每次调用的 token 用量入库
#[tauri::command]
pub async fn llm_chat(
  state: State<'_, Mutex<rusqlite::Connection>>,
  messages: Vec<ChatMessageDto>,
) -> Result<ChatReplyDto, String> {
  if messages.is_empty() {
    return Err("消息不能为空".into());
  }
  if messages[0].role != "system" {
    return Err("消息序列必须以 system 开头".into());
  }
  let config = agent::resolve_config().ok_or_else(|| "未配置 LLM（请在设置页配置后重试）".to_string())?;
  let msgs: Vec<agent::Message> = messages
    .into_iter()
    .map(|m| agent::Message { role: m.role, content: m.content, tool_calls: None, tool_call_id: None })
    .collect();
  let outcome = agent::chat(&config, &msgs).await.map_err(|e| e.to_string())?;
  {
    let conn = state.lock().map_err(|e| e.to_string())?;
    let _ = db::record_llm_usage(
      &conn,
      &outcome.model,
      outcome.usage.prompt_tokens,
      outcome.usage.completion_tokens,
      outcome.usage.total_tokens,
    );
  }
  Ok(ChatReplyDto {
    content: outcome.content,
    model: outcome.model,
    steps: Vec::new(),
  })
}

/// 保存书评（仅写入 books.my_review，不插入个人短评，避免豆瓣短评区重复展示）
#[tauri::command]
pub async fn save_book_review(
  state: State<'_, Mutex<rusqlite::Connection>>,
  id: i64,
  review: String,
) -> Result<(), String> {
  let review = review.trim().to_string();
  if review.is_empty() {
    return Err("书评不能为空".into());
  }
  let conn = state.lock().map_err(|e| e.to_string())?;
  db::save_review(&conn, id, &review).map_err(|e| e.to_string())?;
  Ok(())
}
