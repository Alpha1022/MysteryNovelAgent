//! WebDav 同步：本地书库 ⇄ 云端（按书库名远程子目录归档，手动推送/拉取）
//!
//! 远端布局：`{url}/{remote_dir}/{书库名}/`
//! - `*.epub`：书籍文件
//! - `covers/`：封面缓存（文件名为内容哈希，跨设备天然一致）
//! - `mystery_novel.db`：数据库快照（推送时以 VACUUM INTO 生成一致性快照上传）
//!
//! 同步语义（无自动同步，全部由用户在 GUI 手动触发）：
//! - **同步到云端**（[`push_library`]）：本地书库完整覆盖云端 —— 上传全部 EPUB/封面，
//!   删除云端多出的文件，数据库快照整份覆盖上传
//! - **从云端同步**（[`pull_library`]）：云端完整覆盖本地 —— 覆盖下载 EPUB/封面，
//!   删除本地多出的文件，数据库快照按目标书库整库覆盖（含短评/来源）

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, Context};
use reqwest::{Method, StatusCode};
use rusqlite::Connection;
use tracing::{info, warn};

use crate::config::WebDavConfig;

/// 远端数据库快照文件名（与本地 `AppConfig::database_file` 文件名一致）
const DB_FILE_NAME: &str = "mystery_novel.db";

/// 远端封面缓存子目录名
const COVERS_DIR: &str = "covers";

/// 同步取消令牌（GUI「打断」按钮置位；各文件间检查，进行中的单个请求完成后停止）
#[derive(Clone, Default)]
pub struct SyncCancel(Arc<AtomicBool>);

impl SyncCancel {
  pub fn new() -> Self {
    Self::default()
  }

  /// 置位取消信号
  pub fn cancel(&self) {
    self.0.store(true, Ordering::Relaxed);
  }

  pub fn is_cancelled(&self) -> bool {
    self.0.load(Ordering::Relaxed)
  }

  /// 已取消时返回错误（统一中断标记）
  fn check(&self) -> anyhow::Result<()> {
    if self.is_cancelled() {
      anyhow::bail!("已取消");
    }
    Ok(())
  }
}

/// 同步进度（GUI 通过 task-progress 事件实时渲染；字段与前端 TaskProgressEvent 对齐）
#[derive(Debug, Clone, serde::Serialize)]
pub struct SyncProgress {
  /// 事件类型前缀：webdav-push（同步到云端）/ webdav-pull（从云端同步），
  /// 具体动作与文件名见 message
  pub phase: String,
  pub current: i64,
  pub total: i64,
  pub message: String,
}

/// 同步结果（GUI 展示用）
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct SyncReport {
  /// 书籍 EPUB 总数（推送 = 本地数，拉取 = 云端数）
  pub total: usize,
  /// 本次上传数（书籍 + 封面 + 快照）
  pub uploaded: usize,
  /// 本次下载数（书籍 + 封面 + 快照）
  pub downloaded: usize,
  /// 本次删除的远端/本地多余文件数（完整覆盖语义）
  pub deleted: usize,
  /// 内容已一致而跳过数（仅统计 EPUB）
  pub skipped: usize,
  /// 失败数
  pub failed: usize,
  /// 数据库覆盖：远端有而本地无 → 并入
  pub books_imported: usize,
  /// 数据库覆盖：本地有 → 用云端数据覆盖
  pub books_updated: usize,
  /// 数据库覆盖：本地有而云端无 → 删除
  pub books_deleted: usize,
  /// 失败明细
  pub errors: Vec<String>,
}

/// 单书库同步上下文
pub struct LibrarySyncCtx<'a> {
  /// 书库 ID（云端快照书籍归属的本地书库）
  pub library_id: &'a str,
  /// 书库名（远程子目录名，[A-Za-z0-9_]）
  pub name: &'a str,
  /// 本地书库根目录（EPUB 存放处）
  pub library_dir: &'a Path,
  /// 封面缓存目录（移动端为应用私有目录，可与书库目录不同）
  pub covers_dir: PathBuf,
  /// 云端无数据库快照时的降级登记标签
  pub default_tags: &'a [String],
}

/// 校验书库可同步并构建上下文（推送/拉取共用入口校验）
///
/// `covers_dir` 由调用方经 `AppConfig::covers_dir()` 取得
/// （移动端为应用私有目录，桌面端为 `{library_path}/covers`）。
pub fn ensure_ctx(
  lib: &crate::config::LibraryConfig,
  covers_dir: PathBuf,
) -> anyhow::Result<LibrarySyncCtx<'_>> {
  if !lib.webdav.enabled {
    anyhow::bail!("该书库未启用 WebDav 同步，请先在 WebDav 页签启用并保存");
  }
  if !crate::config::is_valid_library_name(&lib.name) {
    anyhow::bail!("书库名非法（仅大小写字母/数字/下划线）：{}", lib.name);
  }
  Ok(LibrarySyncCtx {
    library_id: &lib.id,
    name: &lib.name,
    library_dir: &lib.path,
    covers_dir,
    default_tags: &lib.default_tags,
  })
}

/// WebDav 客户端（基础认证 + 通用方法）
struct DavClient {
  /// 远端书库目录 URL（`{url}/{remote_dir}/{书库名}`，无尾斜杠）
  base: String,
  http: reqwest::Client,
  username: String,
  password: String,
}

impl DavClient {
  fn new(cfg: &WebDavConfig, name: &str) -> anyhow::Result<Self> {
    let url = cfg.url.trim().trim_end_matches('/');
    if url.is_empty() {
      return Err(anyhow!("WebDav 服务器地址为空"));
    }
    if !url.starts_with("http://") && !url.starts_with("https://") {
      return Err(anyhow!("WebDav 地址需以 http(s):// 开头"));
    }
    let dir = cfg.remote_dir.trim().trim_matches('/');
    // 逐段编码（书库名仅 [A-Za-z0-9_]，编码为恒等）
    let mut segments: Vec<String> = dir
      .split('/')
      .filter(|s| !s.is_empty())
      .map(encode_seg)
      .collect();
    segments.push(encode_seg(name));
    let base = format!("{url}/{}", segments.join("/"));
    let http = reqwest::Client::builder()
      .timeout(std::time::Duration::from_secs(120))
      .connect_timeout(std::time::Duration::from_secs(10))
      .build()?;
    Ok(Self {
      base,
      http,
      username: cfg.username.trim().to_string(),
      password: cfg.password.clone(),
    })
  }

  /// 带基础认证的请求构造（reqwest 0.12 的 basic_auth 在 RequestBuilder 上）
  fn req(&self, method: Method, url: &str) -> reqwest::RequestBuilder {
    self
      .http
      .request(method, url)
      .basic_auth(self.username.clone(), Some(self.password.clone()))
  }

  /// 逐级 MKCOL 创建远程目录（405/301/409 = 已存在或中间目录已建）
  async fn ensure_dir(&self) -> anyhow::Result<()> {
    let (scheme_rest, rest) = self
      .base
      .split_once("://")
      .context("WebDav 地址缺少 scheme")?;
    let (host, dirpath) = rest.split_once('/').unwrap_or((rest, ""));
    let mut current = format!("{scheme_rest}://{host}");
    for seg in dirpath.split('/').filter(|s| !s.is_empty()) {
      current = format!("{current}/{seg}");
      let resp = self.req(dav_method("MKCOL"), &current).send().await?;
      let status = resp.status();
      if !(status.is_success() || is_dir_exists_status(status)) {
        return Err(anyhow!("创建远程目录失败: HTTP {status}（请检查地址与账号权限）"));
      }
    }
    Ok(())
  }

  /// 确保相对子目录存在（如 covers）
  async fn ensure_subdir(&self, sub: &str) -> anyhow::Result<()> {
    let url = format!("{}/{}", self.base, encode_seg(sub));
    let resp = self.req(dav_method("MKCOL"), &url).send().await?;
    let status = resp.status();
    if !(status.is_success() || is_dir_exists_status(status)) {
      return Err(anyhow!("创建远程子目录 {sub} 失败: HTTP {status}"));
    }
    Ok(())
  }

  /// PROPFIND Depth:1 列取目录：文件名 → 大小（404 视为空目录）
  async fn list_dir(&self, url: &str) -> anyhow::Result<HashMap<String, u64>> {
    let resp = self
      .req(dav_method("PROPFIND"), url)
      .header("Depth", "1")
      .header("Content-Type", "application/xml; charset=utf-8")
      .body(
        r#"<?xml version="1.0" encoding="utf-8"?>
<d:propfind xmlns:d="DAV:"><d:prop><d:getcontentlength/><d:resourcetype/></d:prop></d:propfind>"#,
      )
      .send()
      .await?;
    let status = resp.status();
    if status == StatusCode::NOT_FOUND {
      return Ok(HashMap::new());
    }
    if !status.is_success() {
      return Err(anyhow!("读取远程目录失败: HTTP {status}（401/403 请检查账号密码）"));
    }
    let text = resp.text().await?;
    Ok(parse_propfind(&text))
  }

  /// GET 下载远端文件
  async fn download(&self, url: &str) -> anyhow::Result<Vec<u8>> {
    let resp = self.req(Method::GET, url).send().await?;
    let status = resp.status();
    if !status.is_success() {
      return Err(anyhow!("HTTP {status}"));
    }
    Ok(resp.bytes().await?.to_vec())
  }

  /// PUT 上传一个文件（覆盖远端同名文件）
  async fn upload(&self, name: &str, bytes: Vec<u8>) -> anyhow::Result<()> {
    let url = format!("{}/{}", self.base, encode_seg(name));
    let resp = self.req(Method::PUT, &url).body(bytes).send().await?;
    let status = resp.status();
    if !status.is_success() {
      return Err(anyhow!("HTTP {status}"));
    }
    Ok(())
  }

  /// PUT 上传到指定子目录（如 covers）
  async fn upload_to_subdir(&self, sub: &str, name: &str, bytes: Vec<u8>) -> anyhow::Result<()> {
    let url = format!("{}/{}/{}", self.base, encode_seg(sub), encode_seg(name));
    let resp = self.req(Method::PUT, &url).body(bytes).send().await?;
    let status = resp.status();
    if !status.is_success() {
      return Err(anyhow!("HTTP {status}"));
    }
    Ok(())
  }

  /// DELETE 删除远端文件（完整覆盖语义：清除云端多出的文件）
  async fn delete_url(&self, url: &str) -> anyhow::Result<()> {
    let resp = self.req(Method::DELETE, url).send().await?;
    let status = resp.status();
    if !status.is_success() {
      return Err(anyhow!("HTTP {status}"));
    }
    Ok(())
  }

  /// 下载远端文件并写入本地路径（覆盖已存在文件）
  async fn download_to(&self, url: &str, dest: &Path) -> anyhow::Result<()> {
    let bytes = self.download(url).await?;
    std::fs::write(dest, bytes).map_err(|e| anyhow!("本地写入失败: {e}"))
  }
}

/// 目录已存在的常见响应（服务器实现不一）
fn is_dir_exists_status(status: StatusCode) -> bool {
  matches!(
    status,
    StatusCode::METHOD_NOT_ALLOWED
      | StatusCode::MOVED_PERMANENTLY
      | StatusCode::FOUND
      | StatusCode::CONFLICT
      | StatusCode::FORBIDDEN
  )
}

/// 构造 WebDav 扩展方法（MKCOL/PROPFIND）
fn dav_method(name: &str) -> Method {
  Method::from_bytes(name.as_bytes()).expect("合法 HTTP 方法名")
}

/// URL 路径段编码（保留 unreserved 字符 - _ . ~，书库文件名为 ASCII）
fn encode_seg(s: &str) -> String {
  const SEG: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~');
  percent_encoding::utf8_percent_encode(s, SEG).to_string()
}

/// 解析 PROPFIND multistatus XML：response 块内 href（末段）+ getcontentlength
///
/// 跳过目录（resourcetype 含 collection）条目，仅返回文件 —— Depth:1 列表
/// 会包含目录自身与其子目录，混入会导致"covers/covers"之类的 404 下载。
fn parse_propfind(xml: &str) -> HashMap<String, u64> {
  let mut map = HashMap::new();
  let re_resp =
    regex::Regex::new(r"(?is)<[a-z0-9]*:?response\b[^>]*>(.*?)</[a-z0-9]*:?response>").ok();
  let re_href = regex::Regex::new(r"(?is)<[a-z0-9]*:?href\b[^>]*>(.*?)</[a-z0-9]*:?href>").ok();
  let re_len =
    regex::Regex::new(r"(?is)<[a-z0-9]*:?getcontentlength\b[^>]*>(.*?)</[a-z0-9]*:?getcontentlength>")
      .ok();
  let re_collection =
    regex::Regex::new(r"(?is)<[a-z0-9]*:?resourcetype\b[^>]*>\s*<[a-z0-9]*:?collection\s*/?>").ok();
  let (Some(re_resp), Some(re_href), Some(re_len), Some(re_collection)) =
    (re_resp, re_href, re_len, re_collection)
  else {
    return map;
  };
  for caps in re_resp.captures_iter(xml) {
    let block = caps.get(1).map(|m| m.as_str()).unwrap_or("");
    if re_collection.is_match(block) {
      continue; // 目录条目不入文件表
    }
    let Some(href_caps) = re_href.captures(block) else {
      continue;
    };
    let raw = href_caps.get(1).map(|m| m.as_str()).unwrap_or("").trim();
    // href 可能是完整 URL 或路径，统一取最后一段
    let name = raw.rsplit('/').next().unwrap_or(raw);
    let name = percent_encoding::percent_decode_str(name)
      .decode_utf8_lossy()
      .into_owned();
    if name.is_empty() {
      continue;
    }
    let size = re_len
      .captures(block)
      .and_then(|c| c.get(1))
      .and_then(|m| m.as_str().trim().parse::<u64>().ok())
      .unwrap_or(0);
    map.insert(name, size);
  }
  map
}

/// 测试连接：创建远程目录（含书库名子目录）并列取内容
pub async fn test_connection(cfg: &WebDavConfig, name: &str) -> anyhow::Result<()> {
  let client = DavClient::new(cfg, name)?;
  client.ensure_dir().await?;
  client.ensure_subdir("covers").await?;
  let files = client.list_dir(&client.base).await?;
  info!("WebDav 连接成功，远端 {name}/ 现有 {} 个条目", files.len());
  Ok(())
}

/// 提取 EPUB 书名与作者（下载入库登记用；解析失败回退文件名）
fn epub_title_author(path: &Path) -> (String, String) {
  let fallback = path
    .file_stem()
    .map(|s| s.to_string_lossy().into_owned())
    .unwrap_or_default();
  match rbook::Epub::open(path.to_string_lossy().as_ref()) {
    Ok(epub) => {
      let title = epub
        .metadata()
        .title()
        .map(|t| t.value().trim().to_string())
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| fallback.clone());
      let author = epub
        .metadata()
        .creators()
        .next()
        .map(|c| c.value().trim().to_string())
        .filter(|v| !v.is_empty())
        .unwrap_or_default();
      (title, author)
    }
    Err(_) => (fallback, String::new()),
  }
}

/// 将下载的 EPUB 登记入数据库（云端无数据库快照时的兜底；幂等）
fn register_downloaded_epub(
  conn: &Connection,
  library_id: &str,
  library_file: &str,
  path: &Path,
  default_tags: &[String],
) -> anyhow::Result<()> {
  let exists = conn
    .query_row(
      "SELECT 1 FROM books WHERE library_file = ?1 LIMIT 1",
      rusqlite::params![library_file],
      |_| Ok(()),
    )
    .is_ok();
  if exists {
    return Ok(());
  }
  let (title, author) = epub_title_author(path);
  let title = crate::utils::to_simplified(&title);
  let author = crate::utils::to_simplified(&author);
  let mut tags: Vec<String> = default_tags
    .iter()
    .map(|t| t.trim().to_string())
    .filter(|t| !t.is_empty())
    .collect();
  if tags.is_empty() {
    tags.push("推理小说".to_string());
  }
  let book_id = crate::db::insert_book(conn, &title, &author, &crate::utils::join_tags(&tags), "", "[]")?;
  crate::db::set_book_library(conn, book_id, library_id)?;
  crate::db::update_book_enrichment(conn, book_id, None, None, None, None, Some(library_file), Some("[]"))?;
  info!("远端书籍已登记: {title} ({library_file})");
  Ok(())
}

/// 打开本地数据库执行一段纯同步操作（rusqlite Connection 不可跨 await，故分阶段短暂持有）
fn with_local_db<T>(f: impl FnOnce(&Connection) -> anyhow::Result<T>) -> anyhow::Result<T> {
  let db_path = crate::config::AppConfig::load().database_file();
  let conn =
    crate::db::open_db(&db_path.to_string_lossy()).map_err(|e| anyhow!("数据库打开失败: {e}"))?;
  f(&conn)
}

/// 校验目录可写：写入并删除一个 1 字节测试文件
///
/// Android 上书库目录位于公共存储但未授予「所有文件访问」时，
/// 读目录可能成功而写文件失败（EPUB/封面逐个下载失败但数据库正常——
/// 数据库走应用私有 cache 目录）。下载前预检可立即给出可操作的错误。
fn ensure_dir_writable(dir: &Path) -> anyhow::Result<()> {
  let test = dir.join(".mna-write-test");
  std::fs::write(&test, b"x").map_err(|e| {
    anyhow!(
      "书库目录不可写: {}（{e}）\n请在系统设置中授予「所有文件访问」权限，或将书库目录设为应用私有目录",
      dir.display()
    )
  })?;
  let _ = std::fs::remove_file(&test);
  Ok(())
}

/// 列取本地书库目录中的 EPUB 文件（名称升序）
fn collect_local_epubs(dir: &Path) -> anyhow::Result<Vec<(String, PathBuf, u64)>> {
  let mut out = Vec::new();
  for entry in std::fs::read_dir(dir)
    .with_context(|| format!("读取书库目录失败: {}", dir.display()))?
  {
    let entry = entry?;
    let p = entry.path();
    if !p.is_file() {
      continue;
    }
    let is_epub = p
      .extension()
      .and_then(|e| e.to_str())
      .map(|e| e.eq_ignore_ascii_case("epub"))
      .unwrap_or(false);
    if !is_epub {
      continue;
    }
    let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
    let name = p
      .file_name()
      .map(|s| s.to_string_lossy().into_owned())
      .unwrap_or_default();
    if !name.is_empty() {
      out.push((name, p, size));
    }
  }
  out.sort_by(|a, b| a.0.cmp(&b.0));
  Ok(out)
}

/// 列取本地封面缓存目录中的文件（跳过 .nomedia 等点文件）
fn collect_local_covers(dir: &Path) -> Vec<(String, PathBuf, u64)> {
  let mut out = Vec::new();
  let Ok(entries) = std::fs::read_dir(dir) else {
    return out;
  };
  for entry in entries.flatten() {
    let p = entry.path();
    if !p.is_file() {
      continue;
    }
    let Some(name) = p.file_name().map(|s| s.to_string_lossy().into_owned()) else {
      continue;
    };
    if name.starts_with('.') {
      continue;
    }
    let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
    out.push((name, p, size));
  }
  out.sort_by(|a, b| a.0.cmp(&b.0));
  out
}

/// 汇总推送用本地封面：书库 covers 目录 + 旧版独立 covers_path 配置（同名取其一）
///
/// 桌面端若配置了 covers_path，store_cover 写入的是该目录而非书库 covers 目录；
/// 只推送书库 covers 会让这部分封面永远无法到达云端，移动端重定位随之失败。
fn collect_push_covers(library_covers: &Path, alt_covers: Option<&Path>) -> Vec<(String, PathBuf, u64)> {
  let mut merged: HashMap<String, (PathBuf, u64)> = HashMap::new();
  for (name, path, size) in collect_local_covers(library_covers) {
    merged.entry(name).or_insert((path, size));
  }
  if let Some(dir) = alt_covers {
    for (name, path, size) in collect_local_covers(dir) {
      merged.entry(name).or_insert((path, size));
    }
  }
  let mut out: Vec<(String, PathBuf, u64)> = merged
    .into_iter()
    .map(|(name, (path, size))| (name, path, size))
    .collect();
  out.sort_by(|a, b| a.0.cmp(&b.0));
  out
}

/// 同步到云端：本地书库完整覆盖云端
///
/// ① EPUB：同名同大小跳过，其余覆盖上传；云端多出的 → 删除
/// ② covers：同上（忽略点文件）
/// ③ 数据库：VACUUM INTO 生成一致性快照 → 整份覆盖上传（失败回退直接读文件）
/// 单文件失败仅计入 report.errors，不中断整体同步；
/// 每个文件处理完检查取消令牌，进度经 progress 回调实时上报。
pub async fn push_library(
  cfg: &WebDavConfig,
  ctx: &LibrarySyncCtx<'_>,
  cancel: &SyncCancel,
  progress: impl Fn(SyncProgress) + Send + Sync,
) -> anyhow::Result<SyncReport> {
  let client = DavClient::new(cfg, ctx.name)?;
  client.ensure_dir().await?;
  client.ensure_subdir(COVERS_DIR).await?;
  let remote_root = client.list_dir(&client.base).await?;
  let covers_url = format!("{}/{}", client.base, COVERS_DIR);
  let remote_covers = client.list_dir(&covers_url).await?;

  let mut report = SyncReport::default();

  // ---------- ① EPUB：本地 → 云端 ----------
  let local_files = collect_local_epubs(ctx.library_dir)?;
  report.total = local_files.len();
  let local_names: HashSet<String> = local_files.iter().map(|(n, _, _)| n.clone()).collect();

  // 进度：EPUB + covers + 数据库快照
  let covers_dir = &ctx.covers_dir;
  let _ = std::fs::create_dir_all(covers_dir);
  crate::config::ensure_nomedia(covers_dir);
  // 旧版 covers_path 独立配置目录中的封面一并推送（与主封面目录求并集）
  let alt_covers = crate::config::AppConfig::load().covers_path;
  let local_covers = collect_push_covers(covers_dir, alt_covers.as_deref());
  let total = (local_files.len() + local_covers.len() + 1) as i64;
  let mut current: i64 = 0;
  let mut emit = |_stage: &str, message: String| {
    current += 1;
    progress(SyncProgress {
      phase: "webdav-push".into(),
      current,
      total,
      message,
    });
  };

  for (name, path, size) in &local_files {
    cancel.check()?;
    if *size > 0 && remote_root.get(name) == Some(size) {
      report.skipped += 1;
      emit("epub", format!("跳过（已一致）{name}"));
      continue;
    }
    match std::fs::read(path) {
      Ok(bytes) => match client.upload(name, bytes).await {
        Ok(()) => {
          report.uploaded += 1;
          emit("epub", format!("上传 {name}"));
        }
        Err(e) => {
          report.failed += 1;
          warn!("WebDav 推送失败: {name}: 上传失败 {e}");
          report.errors.push(format!("{name}: 上传失败 {e}"));
          emit("epub", format!("上传失败 {name}"));
        }
      },
      Err(e) => {
        report.failed += 1;
        warn!("WebDav 推送失败: {name}: 读取失败 {e}");
        report.errors.push(format!("{name}: 读取失败 {e}"));
        emit("epub", format!("读取失败 {name}"));
      }
    }
  }
  // 云端多出的 EPUB → 删除（本地已不存在的书不应继续留在云端）
  for name in remote_root.keys() {
    if !name.to_lowercase().ends_with(".epub") || local_names.contains(name) {
      continue;
    }
    cancel.check()?;
    let url = format!("{}/{}", client.base, encode_seg(name));
    match client.delete_url(&url).await {
      Ok(()) => {
        report.deleted += 1;
        emit("epub", format!("删除云端多余 {name}"));
      }
      Err(e) => {
        report.failed += 1;
        warn!("WebDav 推送删除失败: {name}: {e}");
        report.errors.push(format!("{name}: 远端删除失败 {e}"));
        emit("epub", format!("删除失败 {name}"));
      }
    }
  }

  // ---------- ② covers：本地 → 云端 ----------
  let local_cover_names: HashSet<String> =
    local_covers.iter().map(|(n, _, _)| n.clone()).collect();
  for (name, path, size) in &local_covers {
    cancel.check()?;
    if *size > 0 && remote_covers.get(name) == Some(size) {
      emit("covers", format!("跳过（已一致）封面 {name}"));
      continue;
    }
    match std::fs::read(path) {
      Ok(bytes) => match client.upload_to_subdir(COVERS_DIR, name, bytes).await {
        Ok(()) => {
          report.uploaded += 1;
          emit("covers", format!("上传封面 {name}"));
        }
        Err(e) => {
          report.failed += 1;
          warn!("WebDav 推送失败: {COVERS_DIR}/{name}: 上传失败 {e}");
          report.errors.push(format!("{COVERS_DIR}/{name}: 上传失败 {e}"));
          emit("covers", format!("上传失败封面 {name}"));
        }
      },
      Err(e) => {
        report.failed += 1;
        warn!("WebDav 推送失败: {COVERS_DIR}/{name}: 读取失败 {e}");
        report.errors.push(format!("{COVERS_DIR}/{name}: 读取失败 {e}"));
        emit("covers", format!("读取失败封面 {name}"));
      }
    }
  }
  for name in remote_covers.keys() {
    if name.starts_with('.') || local_cover_names.contains(name) {
      continue;
    }
    cancel.check()?;
    let url = format!("{covers_url}/{}", encode_seg(name));
    match client.delete_url(&url).await {
      Ok(()) => {
        report.deleted += 1;
        emit("covers", format!("删除云端多余封面 {name}"));
      }
      Err(e) => {
        report.failed += 1;
        warn!("WebDav 推送删除失败: {COVERS_DIR}/{name}: {e}");
        report.errors.push(format!("{COVERS_DIR}/{name}: 远端删除失败 {e}"));
        emit("covers", format!("删除失败封面 {name}"));
      }
    }
  }

  // ---------- ③ 数据库快照：整份覆盖上传 ----------
  cancel.check()?;
  emit("db", "上传数据库快照…".into());
  match push_db_snapshot(&client).await {
    Ok(()) => report.uploaded += 1,
    Err(e) => {
      report.failed += 1;
      warn!("WebDav 推送失败: {DB_FILE_NAME}: 上传失败 {e}");
      report.errors.push(format!("{DB_FILE_NAME}: 上传失败 {e}"));
    }
  }

  info!(
    "WebDav 推送完成 [{}]: 共 {} 本，上传 {} 删除 {} 跳过 {} 失败 {}",
    ctx.name, report.total, report.uploaded, report.deleted, report.skipped, report.failed
  );
  Ok(report)
}

/// 生成并上传本地数据库的一致性快照
///
/// VACUUM INTO 在读取事务中产出干净快照（不受其他连接并发写影响、自动压缩）；
/// 失败时回退为直接读取数据库文件（存在极小概率读到写事务中间态）。
async fn push_db_snapshot(client: &DavClient) -> anyhow::Result<()> {
  let tmp = crate::config::temp_dir().join(format!(
    "mna-webdav-push-{}-{}.db",
    std::process::id(),
    unix_secs()
  ));
  let _ = std::fs::remove_file(&tmp);
  let snapshot = with_local_db(|conn| {
    conn
      .execute(
        "VACUUM INTO ?1",
        rusqlite::params![tmp.to_string_lossy().as_ref()],
      )
      .map(|_| ())
      .map_err(|e| anyhow!("{e}"))
  });
  let bytes = match snapshot {
    Ok(()) => std::fs::read(&tmp).map_err(|e| anyhow!("快照文件读取失败: {e}")),
    Err(e) => {
      warn!("VACUUM INTO 快照失败，回退直接读取数据库文件: {e}");
      let db_path = crate::config::AppConfig::load().database_file();
      std::fs::read(&db_path).map_err(|e| anyhow!("数据库文件读取失败: {e}"))
    }
  };
  let _ = std::fs::remove_file(&tmp);
  client.upload(DB_FILE_NAME, bytes?).await
}

/// 从云端同步：云端完整覆盖本地
///
/// ① covers：云端 → 本地（缺失或大小不一致则覆盖下载）
/// ② EPUB：云端 → 本地（缺失或大小不一致则覆盖下载）；本地多出的 → 删除
/// ③ 数据库：云端快照按目标书库整库覆盖（含短评/来源）；云端无快照时
///    降级为按 EPUB 文件登记骨架行（幂等，不覆盖已有数据）
/// ④ 重算封面引用计数并清扫无主封面文件
/// 每个文件处理完检查取消令牌，进度经 progress 回调实时上报。
pub async fn pull_library(
  cfg: &WebDavConfig,
  ctx: &LibrarySyncCtx<'_>,
  cancel: &SyncCancel,
  progress: impl Fn(SyncProgress) + Send + Sync,
) -> anyhow::Result<SyncReport> {
  let client = DavClient::new(cfg, ctx.name)?;
  // 拉取不创建远端目录：云端书库尚未推送时直接明确报错
  let remote_root = client.list_dir(&client.base).await?;
  if remote_root.is_empty() {
    return Err(anyhow!(
      "云端书库目录为空（请先在其他设备执行「同步到云端」，或检查书库名/远程目录是否一致）"
    ));
  }
  let covers_url = format!("{}/{}", client.base, COVERS_DIR);
  let remote_covers = client.list_dir(&covers_url).await?;
  let remote_files: HashSet<String> = remote_root
    .keys()
    .filter(|n| n.to_lowercase().ends_with(".epub"))
    .cloned()
    .collect();

  let mut report = SyncReport::default();
  report.total = remote_files.len();
  let covers_dir = &ctx.covers_dir;
  let _ = std::fs::create_dir_all(covers_dir);
  crate::config::ensure_nomedia(covers_dir);

  // 下载前预检目标目录可写（EPUB 写书库目录、封面写封面目录；
  // Android 公共存储未授权时逐文件写入必然失败）
  ensure_dir_writable(ctx.library_dir)?;
  ensure_dir_writable(covers_dir)?;

  // 进度：云端 EPUB + covers + 数据库覆盖
  let total = (remote_files.len() + remote_covers.len() + 1) as i64;
  let mut current: i64 = 0;
  let mut emit = |_stage: &str, message: String| {
    current += 1;
    progress(SyncProgress {
      phase: "webdav-pull".into(),
      current,
      total,
      message,
    });
  };

  // ---------- ① covers：云端 → 本地 ----------
  for (name, size) in &remote_covers {
    if name.starts_with('.') {
      continue;
    }
    cancel.check()?;
    let local = covers_dir.join(name);
    if local.is_file() && *size > 0 && local.metadata().map(|m| m.len()).unwrap_or(0) == *size {
      emit("covers", format!("跳过（已一致）封面 {name}"));
      continue;
    }
    let url = format!("{covers_url}/{}", encode_seg(name));
    match client.download_to(&url, &local).await {
      Ok(()) => {
        report.downloaded += 1;
        emit("covers", format!("下载封面 {name}"));
      }
      Err(e) => {
        report.failed += 1;
        let msg = format!("{COVERS_DIR}/{name}: 下载失败 {e}");
        warn!("WebDav 拉取失败: {msg}");
        report.errors.push(msg);
        emit("covers", format!("下载失败封面 {name}"));
      }
    }
  }

  // ---------- ② EPUB：云端 → 本地（缺失/不一致覆盖下载，本地多余删除） ----------
  let local_files = collect_local_epubs(ctx.library_dir)?;
  for name in &remote_files {
    cancel.check()?;
    let identical = local_files
      .iter()
      .any(|(n, _, s)| n == name && *s > 0 && remote_root.get(name) == Some(s));
    if identical {
      report.skipped += 1;
      emit("epub", format!("跳过（已一致）{name}"));
      continue;
    }
    let url = format!("{}/{}", client.base, encode_seg(name));
    let dest = ctx.library_dir.join(name);
    match client.download_to(&url, &dest).await {
      Ok(()) => {
        report.downloaded += 1;
        emit("epub", format!("下载 {name}"));
      }
      Err(e) => {
        report.failed += 1;
        let msg = format!("{name}: 下载失败 {e}");
        warn!("WebDav 拉取失败: {msg}");
        report.errors.push(msg);
        emit("epub", format!("下载失败 {name}"));
      }
    }
  }
  for (name, path, _) in &local_files {
    if remote_files.contains(name) {
      continue;
    }
    cancel.check()?;
    match std::fs::remove_file(path) {
      Ok(()) => {
        report.deleted += 1;
        emit("epub", format!("删除本地多余 {name}"));
      }
      Err(e) => {
        report.failed += 1;
        warn!("WebDav 拉取删除失败: {name}: {e}");
        report.errors.push(format!(
          "{name}: 本地删除失败 {e}（文件可能正被阅读器占用）"
        ));
        emit("epub", format!("删除失败 {name}"));
      }
    }
  }

  // ---------- ③ 数据库：云端快照整库覆盖（作用域限定目标书库） ----------
  cancel.check()?;
  emit("db", "应用云端数据库…".into());
  let mut db_covered = false;
  if remote_root.contains_key(DB_FILE_NAME) {
    match client
      .download(&format!("{}/{}", client.base, DB_FILE_NAME))
      .await
    {
      Ok(bytes) => {
        let tmp = crate::config::temp_dir().join(format!(
          "mna-webdav-pull-{}-{}.db",
          std::process::id(),
          unix_secs()
        ));
        if std::fs::write(&tmp, &bytes).is_ok() {
          let result = Connection::open(tmp.to_string_lossy().as_ref())
            .map_err(|e| anyhow!("云端数据库打开失败: {e}"))
            .and_then(|remote_conn| {
              with_local_db(|conn| {
                crate::db::replace_library_from_remote(
                  conn,
                  &remote_conn,
                  ctx.library_id,
                  &covers_dir,
                  &remote_files,
                )
                .map_err(|e| anyhow!("{e}"))
              })
            });
          match result {
            Ok(stats) => {
              report.books_imported = stats.inserted;
              report.books_updated = stats.updated;
              report.books_deleted = stats.deleted;
              db_covered = true;
            }
            Err(e) => {
              let msg = format!("{DB_FILE_NAME}: 云端数据覆盖失败: {e}");
              warn!("{msg}");
              report.errors.push(msg);
            }
          }
          let _ = std::fs::remove_file(&tmp);
        } else {
          report.errors.push("云端数据库缓存写入失败".into());
        }
      }
      Err(e) => {
        warn!("WebDav 拉取失败: {DB_FILE_NAME}: 下载失败 {e}");
        report.errors.push(format!("{DB_FILE_NAME}: 下载失败 {e}"))
      }
    }
  }
  // 云端无快照或覆盖失败：降级为骨架登记（幂等，不覆盖已有书籍数据）
  if !db_covered {
    let register = with_local_db(|conn| {
      for name in &remote_files {
        let path = ctx.library_dir.join(name);
        if let Err(e) =
          register_downloaded_epub(conn, ctx.library_id, name, &path, ctx.default_tags)
        {
          let msg = format!("{name}: 登记失败 {e}");
          warn!("{msg}");
        }
      }
      Ok(())
    });
    if let Err(e) = register {
      report.errors.push(format!("本地数据库打开失败: {e}"));
    }
  }

  // ---------- ④ 重算封面引用 + 清扫无主封面文件 ----------
  match with_local_db(|conn| {
    crate::db::seed_cover_refs(conn);
    Ok(crate::db::cleanup_unreferenced_covers(conn, &covers_dir))
  }) {
    Ok(n) if n > 0 => info!("已清扫无主封面文件 {n} 个"),
    Ok(_) => {}
    Err(e) => warn!("封面清扫失败: {e}"),
  }

  info!(
    "WebDav 拉取完成 [{}]: 共 {} 本，下载 {} 删除 {} 跳过 {}，入库 {} 覆盖 {} 移除 {}，失败 {}",
    ctx.name,
    report.total,
    report.downloaded,
    report.deleted,
    report.skipped,
    report.books_imported,
    report.books_updated,
    report.books_deleted,
    report.failed
  );
  Ok(report)
}

fn unix_secs() -> u64 {
  std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map(|d| d.as_secs())
    .unwrap_or(0)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_parse_propfind() {
    // 典型 Apache/Nginx DAV 响应（D: 前缀 + 无前缀混合）
    let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<D:multistatus xmlns:D="DAV:">
  <D:response>
    <D:href>/dav/mystery-novel-agent/</D:href>
    <D:propstat><D:prop><D:resourcetype><D:collection/></D:resourcetype></D:prop></D:propstat>
  </D:response>
  <D:response>
    <D:href>/dav/mystery-novel-agent/test%20book.epub</D:href>
    <D:propstat><D:prop><D:getcontentlength>12345</D:getcontentlength></D:prop></D:propstat>
  </D:response>
  <response>
    <href>/dav/mystery-novel-agent/a.epub</href>
    <propstat><prop><getcontentlength>99</getcontentlength></prop></propstat>
  </response>
</D:multistatus>"#;
    let map = parse_propfind(xml);
    assert_eq!(map.get("test book.epub"), Some(&12345));
    assert_eq!(map.get("a.epub"), Some(&99));
    assert_eq!(map.len(), 2, "目录条目不进入文件表");
  }

  #[test]
  fn test_encode_seg() {
    assert_eq!(
      encode_seg("[a-b] 作者-c.epub"),
      "%5Ba-b%5D%20%E4%BD%9C%E8%80%85-c.epub"
    );
  }

  #[test]
  fn test_dav_base_with_library_name() {
    let cfg = WebDavConfig {
      enabled: true,
      url: "https://dav.example.com/dav/".into(),
      username: "u".into(),
      password: "p".into(),
      remote_dir: " reading ".into(),
    };
    let client = DavClient::new(&cfg, "my_lib").unwrap();
    assert_eq!(client.base, "https://dav.example.com/dav/reading/my_lib");
  }

  #[test]
  fn test_ensure_ctx_validates() {
    let mut lib = crate::config::LibraryConfig {
      id: "default".into(),
      name: "my-lib".into(),
      title: "默认书库".into(),
      path: std::env::temp_dir(),
      theme: Default::default(),
      default_tags: vec![],
      webdav: WebDavConfig {
        enabled: true,
        url: "https://dav.example.com".into(),
        username: String::new(),
        password: String::new(),
        remote_dir: String::new(),
      },
    };
    // 书库名含连字符 → 非法（仅大小写字母/数字/下划线）
    assert!(ensure_ctx(&lib, std::env::temp_dir()).is_err());
    lib.name = "default".into();
    assert!(ensure_ctx(&lib, std::env::temp_dir()).is_ok());
    // 未启用 → 拒绝
    lib.webdav.enabled = false;
    assert!(ensure_ctx(&lib, std::env::temp_dir()).is_err());
  }
}
