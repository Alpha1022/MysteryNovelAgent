use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use dialoguer::{Confirm, Input, MultiSelect, Select};
use thiserror::Error;
use tracing::{info, warn};

use crate::agent;
use crate::db;
use crate::spider::{self, ClaspBookMeta, ClaspBookSuggestion};
use crate::utils;

/// 自定义错误枚举
#[derive(Error, Debug)]
pub enum IngestionError {
  #[error("IO 错误: {0}")]
  IoError(#[from] std::io::Error),

  #[error("网络错误: {0}")]
  NetworkError(String),

  #[error("HTML 解析错误: {0}")]
  ParseError(String),

  #[error("EPUB 处理错误: {0}")]
  EpubError(String),

  #[error("数据库错误: {0}")]
  DbError(#[from] rusqlite::Error),

  #[error("用户取消了操作")]
  UserAbort,

  #[error("任务已取消")]
  Cancelled,

  #[error("{0}")]
  Other(String),
}

// ------------------------------------------------------------------ //
//  长任务取消 / 进度（GUI 实时渲染与打断）
// ------------------------------------------------------------------ //

/// 长任务取消令牌：GUI 侧持有并跨线程置位，
/// prepare_import 在各阶段检查并可用 select! 打断进行中的网络调用
#[derive(Clone, Default)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
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

  /// 校验：已取消时返回错误
  pub fn check(&self) -> Result<(), IngestionError> {
    if self.is_cancelled() {
      Err(IngestionError::Cancelled)
    } else {
      Ok(())
    }
  }

  /// 等待取消信号（供 tokio::select! 打断长网络调用）
  pub async fn wait_cancelled(&self) {
    while !self.is_cancelled() {
      tokio::time::sleep(std::time::Duration::from_millis(120)).await;
    }
  }
}

/// 长任务进度（GUI 通过 task-progress 事件实时渲染）
#[derive(Debug, Clone, serde::Serialize)]
pub struct TaskProgress {
  /// 阶段：analyze / detail / fusion / cover / comments / copy
  pub phase: String,
  pub current: i64,
  pub total: i64,
  pub message: String,
}

impl From<reqwest::Error> for IngestionError {
  fn from(e: reqwest::Error) -> Self {
    IngestionError::NetworkError(e.to_string())
  }
}

// ------------------------------------------------------------------ //
//  导入选项与搜索选择结果
// ------------------------------------------------------------------ //

/// 导入选项
#[derive(Debug, Clone, Copy, Default)]
pub struct IngestOptions {
  /// 合并本模式：直达多选
  pub merged: bool,
  /// 批量模式：唯一结果自动静默处理（跳过选择与编辑确认）
  pub batch: bool,
}

/// 搜索选择结果
struct Selected {
  suggestions: Vec<ClaspBookSuggestion>,
  /// 唯一结果自动选中（批量模式下静默处理）
  auto: bool,
}

// ------------------------------------------------------------------ //
//  公共入口
// ------------------------------------------------------------------ //

/// 检测 OPF manifest 是否需要修复（重复 href 或引用不存在文件的 href）
fn opf_needs_fix(content: &str, valid_paths: &std::collections::HashSet<String>, opf_dir: &str) -> bool {
  let item_re = regex::Regex::new(r"<item\b[^>]*>").unwrap();
  let href_re = regex::Regex::new(r#"href\s*=\s*"([^"]*)""#).unwrap();
  let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
  for caps in item_re.captures_iter(content) {
    let tag = caps.get(0).map(|m| m.as_str()).unwrap_or("");
    if let Some(h) = href_re.captures(tag).and_then(|c| c.get(1)) {
      let href = h.as_str().trim().to_lowercase();
      if !seen.insert(href.clone()) {
        return true; // 重复 href
      }
      let resolved = resolve_href(opf_dir, href.trim());
      if !valid_paths.contains(&resolved) {
        return true; // 引用不存在文件
      }
    }
  }
  false
}

/// 修复 OPF：去除 manifest 中重复 href 的 item、移除引用不存在文件的 item
/// 及对应的 spine itemref（部分制作工具会产生这些错误）
fn fix_opf(content: &str, valid_paths: &std::collections::HashSet<String>, opf_dir: &str) -> String {
  let item_re = regex::Regex::new(r"<item\b[^>]*/?>").unwrap();
  let href_re = regex::Regex::new(r#"href\s*=\s*"([^"]*)""#).unwrap();
  let id_re = regex::Regex::new(r#"id\s*=\s*"([^"]*)""#).unwrap();

  let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
  let mut broken_ids: std::collections::HashSet<String> = std::collections::HashSet::new();

  // 第一步：处理 manifest items（去重 + 移除引用不存在文件的 item）
  let mut content = item_re
    .replace_all(content, |caps: &regex::Captures| {
      let tag = caps.get(0).map(|m| m.as_str()).unwrap_or("");
      let Some(href_m) = href_re.captures(tag) else {
        return tag.to_string();
      };
      let href = href_m.get(1).map(|m| m.as_str().trim()).unwrap_or("");
      let href_lower = href.to_lowercase();
      // 去重
      if !seen.insert(href_lower.clone()) {
        return String::new();
      }
      // 解析 href → 归一化路径，检查是否在有效文件集中
      let resolved = resolve_href(opf_dir, href);
      if !valid_paths.contains(&resolved) {
        if let Some(id_m) = id_re.captures(tag) {
          broken_ids.insert(id_m.get(1).map(|m| m.as_str().to_string()).unwrap_or_default());
        }
        return String::new();
      }
      tag.to_string()
    })
    .into_owned();

  // 第二步：移除 spine 中引用已删除 item 的 itemref
  if !broken_ids.is_empty() {
    let itemref_re = regex::Regex::new(r#"<itemref\b[^>]*/?>"#).unwrap();
    let idref_re = regex::Regex::new(r#"idref\s*=\s*"([^"]*)""#).unwrap();
    content = itemref_re
      .replace_all(&content, |caps: &regex::Captures| {
        let tag = caps.get(0).map(|m| m.as_str()).unwrap_or("");
        if let Some(id) = idref_re.captures(tag).and_then(|c| c.get(1)) {
          if broken_ids.contains(id.as_str()) {
            return String::new();
          }
        }
        tag.to_string()
      })
      .into_owned();
  }
  content
}

/// 解析 OPF 内相对 href 到 zip 内路径
fn resolve_href(opf_dir: &str, href: &str) -> String {
  let combined = if href.starts_with('/') {
    href.trim_start_matches('/').to_string()
  } else {
    format!("{opf_dir}{href}")
  };
  let mut parts: Vec<&str> = Vec::new();
  for seg in combined.split('/') {
    match seg {
      ".." => {
        parts.pop();
      }
      "." | "" => {}
      s => parts.push(s),
    }
  }
  parts.join("/")
}

/// 检查 EPUB 是否需要修复（zip 重复条目 / OPF manifest 重复 href），需要则重建
///
/// 优先走 `ZipArchive`（对 data-descriptor 条目支持完善）；
/// 仅当 `ZipArchive` 因重复条目无法打开时，回退到手动解析中央目录重建。
pub fn sanitize_epub_if_needed(path: &Path) -> Result<Option<PathBuf>, IngestionError> {
  use std::io::Read as _;
  let file = std::fs::File::open(path).map_err(|e| {
    IngestionError::IoError(std::io::Error::new(
      e.kind(),
      format!("打开 EPUB 失败: {e}"),
    ))
  })?;
  match zip::ZipArchive::new(std::io::BufReader::new(file)) {
    Ok(mut arch) => {
      // 正常打开：检查 OPF manifest（重复 href + 引用不存在文件的 href）
      let mut valid_paths: std::collections::HashSet<String> = std::collections::HashSet::new();
      let mut opf_contents: Vec<(String, String)> = Vec::new();
      for i in 0..arch.len() {
        let mut fe = arch
          .by_index(i)
          .map_err(|e| IngestionError::EpubError(format!("EPUB 解析失败: {e}")))?;
        if !fe.is_dir() {
          valid_paths.insert(fe.name().to_string());
        }
        if fe.name().to_lowercase().ends_with(".opf") {
          let mut content = String::new();
          fe.read_to_string(&mut content)
            .map_err(|e| IngestionError::EpubError(format!("EPUB 解析失败: {e}")))?;
          opf_contents.push((fe.name().to_string(), content));
        }
      }
      let opf_dir_for = |name: &str| match name.rfind('/') {
        Some(pos) => name[..=pos].to_string(),
        None => String::new(),
      };
      let needs_fix = opf_contents
        .iter()
        .any(|(name, content)| opf_needs_fix(content, &valid_paths, &opf_dir_for(name)));
      if !needs_fix {
        return Ok(None);
      }
      let tmp = temp_sanitize_path();
      rebuild_from_archive(&mut arch, &tmp)?;
      info!("EPUB OPF 存在问题，已重建: {}", tmp.display());
      Ok(Some(tmp))
    }
    Err(e) => {
      let msg = e.to_string();
      if msg.contains("Duplicate filename") {
        // zip 严格模式拒绝重复条目：手动解析中央目录重建（跳过重复项）
        let tmp = rebuild_via_central_directory(path)?;
        info!("EPUB 含重复条目，已通过中央目录重建: {}", tmp.display());
        Ok(Some(tmp))
      } else {
        Err(IngestionError::EpubError(format!(
          "EPUB 解析失败（zip 结构损坏，无法自动修复）: {e}"
        )))
      }
    }
  }
}

fn temp_sanitize_path() -> PathBuf {
  crate::config::temp_dir().join(format!(
    "mna-sanitize-{}-{}.epub",
    std::process::id(),
    unix_secs()
  ))
}

/// 从已打开的 ZipArchive 重建 EPUB：跳过目录条目、修复 OPF（去重 + 移除无效引用）、验证结果
fn rebuild_from_archive(
  arch: &mut zip::ZipArchive<std::io::BufReader<std::fs::File>>,
  tmp: &Path,
) -> Result<(), IngestionError> {
  use std::io::{Read as _, Write as _};

  // 收集有效文件路径（供 OPF href 解析校验）
  let mut valid_paths: std::collections::HashSet<String> = std::collections::HashSet::new();
  for i in 0..arch.len() {
    let entry = arch.by_index(i).map_err(|e| IngestionError::EpubError(format!("EPUB 解析失败: {e}")))?;
    if !entry.is_dir() {
      valid_paths.insert(entry.name().to_string());
    }
  }

  let out_file = std::fs::File::create(tmp).map_err(|e| {
    IngestionError::IoError(std::io::Error::new(
      e.kind(),
      format!("创建修复文件失败: {e}"),
    ))
  })?;
  let mut out = zip::ZipWriter::new(out_file);
  for i in 0..arch.len() {
    let mut entry = arch.by_index(i).map_err(|e| IngestionError::EpubError(format!("EPUB 解析失败: {e}")))?;
    let name = entry.name().to_string();
    if name.is_empty() || entry.is_dir() {
      continue;
    }
    let mut bytes = Vec::new();
    entry
      .read_to_end(&mut bytes)
      .map_err(|e| IngestionError::EpubError(format!("读取条目 {name} 失败: {e}")))?;
    if name.to_lowercase().ends_with(".opf") {
      let content = String::from_utf8_lossy(&bytes).into_owned();
      let opf_dir = match name.rfind('/') {
        Some(pos) => &name[..=pos],
        None => "",
      };
      bytes = fix_opf(&content, &valid_paths, opf_dir).into_bytes();
    }
    let options = zip::write::SimpleFileOptions::default()
      .compression_method(entry.compression());
    out
      .start_file(name, options)
      .map_err(|e| IngestionError::EpubError(format!("重建 zip 失败: {e}")))?;
    out
      .write_all(&bytes)
      .map_err(|e| IngestionError::EpubError(format!("写入条目失败: {e}")))?;
  }
  out
    .finish()
    .map_err(|e| IngestionError::EpubError(format!("重建 zip 失败: {e}")))?;
  rbook::Epub::open(tmp.to_string_lossy().as_ref())
    .map_err(|e| IngestionError::EpubError(format!("EPUB 修复后仍无法解析: {e}")))?;
  Ok(())
}

/// 手动解析 zip 中央目录并重建（zip 严格模式因重复条目拒绝打开时的兜底）
///
/// 解压仅支持 Stored / Deflated（EPUB 实际使用的全部方式）；跳过重复与目录条目。
fn rebuild_via_central_directory(path: &Path) -> Result<PathBuf, IngestionError> {
  use std::io::{Read as _, Write as _};

  let mut raw = Vec::new();
  std::fs::File::open(path)
    .and_then(|mut f| f.read_to_end(&mut raw))
    .map_err(|e| IngestionError::IoError(e))?;
  let len = raw.len();

  // 定位 EOCD（从尾部向前扫描）
  let mut eocd = None;
  let scan_from = len.saturating_sub(66_000);
  let mut p = len.saturating_sub(22);
  while p >= scan_from {
    if u32_at(&raw, p) == 0x0605_4b50 {
      eocd = Some(p);
      break;
    }
    p -= 1;
  }
  let Some(eocd) = eocd else {
    return Err(IngestionError::EpubError("EPUB 解析失败: 未找到 zip 中央目录".into()));
  };
  let count = u16_at(&raw, eocd + 10) as usize;
  let cd_ofs = u32_at(&raw, eocd + 16) as usize;

  // 解析中央目录条目（首个同名条目胜出）
  struct CdEntry {
    name: String,
    method: u16,
    csize: u64,
    lho: u64,
  }
  let mut entries: Vec<CdEntry> = Vec::new();
  let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
  let mut p = cd_ofs;
  for _ in 0..count {
    if p + 46 > len || u32_at(&raw, p) != 0x0201_4b50 {
      return Err(IngestionError::EpubError("EPUB 解析失败: 中央目录损坏".into()));
    }
    let method = u16_at(&raw, p + 10);
    let csize = u32_at(&raw, p + 20) as u64;
    let nlen = u16_at(&raw, p + 28) as usize;
    let elen = u16_at(&raw, p + 30) as usize;
    let clen = u16_at(&raw, p + 32) as usize;
    let lho = u32_at(&raw, p + 42) as u64;
    if p + 46 + nlen > len {
      return Err(IngestionError::EpubError("EPUB 解析失败: 中央目录越界".into()));
    }
    let name = String::from_utf8_lossy(&raw[p + 46..p + 46 + nlen]).into_owned();
    p += 46 + nlen + elen + clen;
    if name.is_empty() || name.ends_with('/') || !seen.insert(name.clone()) {
      continue; // 目录条目 / 重复条目跳过
    }
    entries.push(CdEntry { name, method, csize, lho });
  }
  if entries.is_empty() {
    return Err(IngestionError::EpubError("EPUB 解析失败: 无有效条目".into()));
  }

  // 有效文件路径集（供 OPF href 解析校验）
  let valid_paths: std::collections::HashSet<String> =
    entries.iter().map(|e| e.name.clone()).collect();

  // 重建
  let tmp = temp_sanitize_path();
  let out_file = std::fs::File::create(&tmp).map_err(|e| {
    IngestionError::IoError(std::io::Error::new(
      e.kind(),
      format!("创建修复文件失败: {e}"),
    ))
  })?;
  let mut out = zip::ZipWriter::new(out_file);
  for e in &entries {
    if e.method != 0 && e.method != 8 {
      return Err(IngestionError::EpubError(format!(
        "EPUB 修复失败: 条目 {} 使用了不支持的压缩方式 ({})",
        e.name, e.method
      )));
    }
    let lho = e.lho as usize;
    if lho + 30 > len || u32_at(&raw, lho) != 0x0403_4b50 {
      return Err(IngestionError::EpubError("EPUB 解析失败: 本地文件头损坏".into()));
    }
    let nlen = u16_at(&raw, lho + 26) as usize;
    let elen = u16_at(&raw, lho + 28) as usize;
    let data_start = lho + 30 + nlen + elen;
    if data_start + e.csize as usize > len {
      return Err(IngestionError::EpubError("EPUB 解析失败: 条目数据越界".into()));
    }
    let comp = &raw[data_start..data_start + e.csize as usize];
    let mut bytes: Vec<u8> = match e.method {
      0 => comp.to_vec(),
      8 => {
        let mut decoder = flate2::read::DeflateDecoder::new(comp);
        let mut v = Vec::new();
        decoder
          .read_to_end(&mut v)
          .map_err(|err| IngestionError::EpubError(format!("条目 {} 解压失败: {err}", e.name)))?;
        v
      }
      _ => unreachable!(),
    };

    if name_is_opf(&e.name) {
      let content = String::from_utf8_lossy(&bytes).into_owned();
      let opf_dir = match e.name.rfind('/') {
        Some(pos) => &e.name[..=pos],
        None => "",
      };
      bytes = fix_opf(&content, &valid_paths, opf_dir).into_bytes();
    }

    let method = if e.method == 0 {
      zip::CompressionMethod::Stored
    } else {
      zip::CompressionMethod::Deflated
    };
    out
      .start_file(e.name.clone(), zip::write::SimpleFileOptions::default().compression_method(method))
      .map_err(|err| IngestionError::EpubError(format!("重建 zip 失败: {err}")))?;
    out
      .write_all(&bytes)
      .map_err(|err| IngestionError::EpubError(format!("写入条目失败: {err}")))?;
  }
  out
    .finish()
    .map_err(|err| IngestionError::EpubError(format!("重建 zip 失败: {err}")))?;

  rbook::Epub::open(tmp.to_string_lossy().as_ref())
    .map_err(|err| IngestionError::EpubError(format!("EPUB 修复后仍无法解析: {err}")))?;
  Ok(tmp)
}

fn u32_at(raw: &[u8], pos: usize) -> u32 {
  u32::from_le_bytes([raw[pos], raw[pos + 1], raw[pos + 2], raw[pos + 3]])
}

fn u16_at(raw: &[u8], pos: usize) -> u16 {
  u16::from_le_bytes([raw[pos], raw[pos + 1]])
}

fn name_is_opf(name: &str) -> bool {
  name.to_lowercase().ends_with(".opf")
}

/// 主入口：接收 EPUB 路径，完成元数据增强 + 简介融合 + 书库入库/// 主入口：接收 EPUB 路径，完成元数据增强 + 简介融合 + 书库入库
///
/// - `opts.merged = true` 时直达合并本多选模式
/// - `opts.batch = true` 时唯一结果自动静默处理（多结果仍走交互）
/// - 原始 EPUB 保持不动：先复制入书库，元数据（标题/作者/标签/简介/系列/封面）只写入书库副本
pub async fn ingest_book(
  epub_path: PathBuf,
  conn: &rusqlite::Connection,
  opts: IngestOptions,
) -> Result<(), IngestionError> {
  // 部分来源 EPUB 的 zip 含重复条目，自动重建为干净副本后处理（原始文件不动）
  let effective = match sanitize_epub_if_needed(&epub_path)? {
    Some(fixed) => fixed,
    None => epub_path.clone(),
  };

  // 阶段一：提取搜索关键词候选（EPUB 标题优先，文件名兜底）
  let mut candidates = Vec::new();
  if let Some(title) = extract_epub_title(&effective) {
    info!("从 EPUB 元数据提取标题: {title}");
    candidates.push(title);
  }
  let filename = utils::filename_stem(&epub_path);
  if !candidates.contains(&filename) {
    candidates.push(filename);
  }
  info!("搜索关键词候选: {:?}", candidates);

  let client = reqwest::Client::builder()
    .timeout(std::time::Duration::from_secs(15))
    .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36")
    .build()?;

  // 阶段二：搜索 + 交互选择（单本 / 合并本）
  let selected = search_and_select(&client, &candidates, opts).await?;
  let mut meta = ClaspBookMeta::from_suggestions(&selected.suggestions);

  let covers_dir = crate::config::AppConfig::load()
    .covers_dir()
    .unwrap_or_else(|_| crate::config::temp_dir());
  let mut clasp_summaries: Vec<String> = Vec::new();
  let mut cover_urls: Vec<String> = Vec::new();
  let mut series_pairs: Vec<(String, Option<i64>)> = Vec::new();
  let mut clasp_sources: Vec<PendingSource> = Vec::new();

  // 阶段三：逐 clasp 条目预爬（详情 + 主封面 + 版本封面 + 评论 + 系列 + 豆瓣链接）
  for clasp_id in &meta.clasp_ids {
    let ps = crawl_clasp_source(&client, clasp_id).await;
    if let Some(s) = ps.summary.clone() {
      clasp_summaries.push(s);
    }
    if let Some(c) = ps.cover_url.as_deref().filter(|c| !c.trim().is_empty()) {
      if !cover_urls.contains(&c.to_string()) {
        cover_urls.push(c.to_string());
      }
    }
    if let Some(s) = ps.series.clone() {
      series_pairs.push(s);
    }
    if let Some(u) = ps.douban_url.clone() {
      if !meta.douban_urls.contains(&u) {
        meta.douban_urls.push(u);
      }
    }
    clasp_sources.push(ps);
  }

  // 系列继承规则：所有条目系列信息一致才沿用（上下册同属一部作品的情况）
  if let Some((name, order)) = inherit_series(&series_pairs) {
    meta.series_name = Some(name);
    meta.series_order = order;
  }

  // 书名规范化：繁体转简体；非中文标题由用户确认（批量模式也不例外）
  meta.title = normalize_title(&meta.title)?;

  // 确保包含当前书库的固定标签（默认"推理小说"）
  let default_tags = crate::config::AppConfig::load().default_tags();
  for t in &default_tags {
    if !meta.tags.contains(t) {
      meta.tags.push(t.clone());
    }
  }

  // 阶段四：手动填写豆瓣链接（豆瓣无搜索接口；留空跳过）
  println!("\n可手动补充豆瓣书籍页链接（短评/简介来源，留空跳过）：");
  let douban_input: String = Input::new()
    .with_prompt("豆瓣链接（空格/逗号分隔多条）")
    .allow_empty(true)
    .default(String::new())
    .interact_text()
    .map_err(io_abort)?;
  let mut douban_list: Vec<String> = Vec::new();
  for u in douban_input.split(|c: char| c == ' ' || c == ',' || c == '，' || c == '\t') {
    let u = u.trim();
    if u.is_empty() {
      continue;
    }
    if !u.contains("book.douban.com/subject/") {
      println!("  ⚠ 忽略无效豆瓣链接: {u}");
      continue;
    }
    if !douban_list.contains(&u.to_string()) {
      douban_list.push(u.to_string());
    }
  }
  for u in &meta.douban_urls {
    if !douban_list.contains(u) {
      douban_list.push(u.clone());
    }
  }
  meta.douban_urls = douban_list;

  // 阶段五：逐豆瓣链接预爬（页面元数据 + 简介 + 封面 + 短评）
  let mut douban_summaries: Vec<String> = Vec::new();
  let mut douban_sources: Vec<PendingSource> = Vec::new();
  for (i, url) in meta.douban_urls.iter().enumerate() {
    println!("  爬取豆瓣来源 {}/{}: {url}", i + 1, meta.douban_urls.len());
    let ps = crawl_douban_source(&client, url).await;
    if let Some(s) = ps.summary.clone() {
      douban_summaries.push(s);
    }
    douban_sources.push(ps);
  }

  // 阶段六：简介定稿（clasp 简介 → 豆瓣简介兜底；多条确认合并 / 取第一本）
  let desc_source = if clasp_summaries.is_empty() { &douban_summaries } else { &clasp_summaries };
  let description = if desc_source.is_empty() {
    String::new()
  } else if desc_source.len() == 1 {
    desc_source[0].clone()
  } else {
    let merge = if agent::resolve_config().is_some() {
      Confirm::new()
        .with_prompt("检测到多条简介，是否调用 LLM 合并？（否 = 取第一本）")
        .default(true)
        .interact()
        .map_err(io_abort)?
    } else {
      println!("  ⚠ 未配置 LLM，简介取第一本");
      false
    };
    let (text, llm_outcome, degraded) =
      pick_import_description(desc_source, merge, &CancelToken::new()).await
      .unwrap_or((String::new(), None, None));
    if let Some((model, usage)) = &llm_outcome {
      let _ = db::record_llm_usage(conn, model, usage.prompt_tokens, usage.completion_tokens, usage.total_tokens);
    }
    if let Some(reason) = &degraded {
      println!("  ⚠ {reason}");
    }
    text
  };

  // 阶段七：封面下载（OSS 伪装 → 豆瓣 og:image → EPUB 内嵌封面兜底）
  let cover_path: Option<PathBuf> = if !cover_urls.is_empty() || !meta.douban_urls.is_empty() {
    let key = meta
      .clasp_ids
      .first()
      .cloned()
      .unwrap_or_else(|| format!("cover-{}", unix_secs()));
    match download_cover_robust(&client, &cover_urls, &meta.douban_urls, &covers_dir, &key).await {
      Ok(p) => Some(p),
      Err(e) => {
        warn!("封面下载失败: {e}，尝试 EPUB 内嵌封面");
        save_epub_cover(&effective, &covers_dir, &key)
      }
    }
  } else {
    // 未匹配到书籍：默认采用 EPUB 内嵌封面
    save_epub_cover(&effective, &covers_dir, &format!("epub-{}", unix_secs()))
  };

  // 阶段六：用户编辑确认（书名/作者/标签/简介）
  // 批量模式下唯一结果自动选中 → 静默跳过编辑环节
  let (title, author, tags, description) = if selected.auto && opts.batch {
    println!("  批量模式：使用抓取的元数据直接入库");
    (
      meta.title.clone(),
      meta.author.clone(),
      meta.tags.clone(),
      description.clone(),
    )
  } else {
    interact_edit(&meta, &description)?
  };
  meta.title = title;
  meta.author = author;
  meta.tags = tags;

  // 阶段七：复制原始文件入书库（原文件保持不动，元数据只写入书库副本）
  let series_tuple = meta
    .series_name
    .as_ref()
    .map(|n| (n.as_str(), meta.series_order.unwrap_or(1)));
  let cfg = crate::config::AppConfig::load();
  let lib = cfg.require_library_path().map_err(|e| {
    IngestionError::Other(format!(
      "{e}（写入元数据需要书库，请先运行 library config set <PATH>）"
    ))
  })?;
  let library_file = crate::library::copy_into_library(
    &lib,
    &effective,
    &meta.author,
    &meta.title,
    series_tuple,
  )
  .map_err(|e| IngestionError::Other(format!("复制入书库失败: {e}")))?;
  let library_abs = lib.join(&library_file);

  // 阶段八：将元数据写入书库副本（不修改原文件，无需备份）
  write_epub(&library_abs, &meta, &description, cover_path.as_deref())?;

  // 阶段九：入库（books + 增强元数据）
  let clasp_ids_json = serde_json::to_string(&meta.clasp_ids).unwrap_or_default();
  // 溯源记录原始路径（若经过 sanitize 重建，处理用的是临时副本）
  let file_path = epub_path.to_string_lossy().into_owned();
  let book_id = db::insert_book(
    conn,
    &meta.title,
    &meta.author,
    &utils::join_tags(&meta.tags),
    &file_path,
    &clasp_ids_json,
  )?;

  let cover_path_str = cover_path.as_ref().map(|p| p.to_string_lossy().into_owned());
  // 归属当前书库（多书库）
  if let Some(lib_id) = crate::config::AppConfig::load().current_library_id() {
    db::set_book_library(conn, book_id, lib_id)?;
  }
  db::update_book_enrichment(
    conn,
    book_id,
    Some(description.as_str()).filter(|s| !s.trim().is_empty()),
    cover_path_str.as_deref(),
    meta.series_name.as_deref(),
    meta.series_order,
    Some(library_file.as_str()),
    Some(clasp_ids_json.as_str()),
  )?;
  // 豆瓣链接入库（重抓短评 / 更换封面豆瓣兜底的数据源）
  let douban_json = serde_json::to_string(&meta.douban_urls).unwrap_or_default();
  db::set_book_match_urls(conn, book_id, None, &douban_json)?;
  // 书籍封面登记引用（计数归零才允许删除文件）
  if let Some(c) = &cover_path {
    db::cover_ref_add(conn, c.to_string_lossy().as_ref())?;
  }

  // 阶段十：来源项目与短评入库（短评带来源定位；来源封面在 insert_source 内登记引用）
  persist_sources(conn, book_id, &clasp_sources, &douban_sources)?;

  print_summary(&meta, book_id, &description, Some(library_file.as_str()));
  Ok(())
}

// ------------------------------------------------------------------ //
//  阶段一
// ------------------------------------------------------------------ //

/// 提取 EPUB 内嵌的书名与作者（字段缺失返回 None，解析失败返回 None）
fn extract_epub_meta(path: &Path) -> Option<(Option<String>, Option<String>)> {
  let epub = rbook::Epub::open(path.to_string_lossy().as_ref()).ok()?;
  let title = epub.metadata().title().and_then(|t| {
    let v = t.value().trim().to_string();
    (!v.is_empty()).then_some(v)
  });
  let author = epub
    .metadata()
    .creators()
    .next()
    .map(|c| c.value().trim().to_string())
    .filter(|v| !v.is_empty());
  Some((title, author))
}

/// 从 EPUB 元数据 `<dc:title>` 提取书名，失败则返回 None
fn extract_epub_title(path: &Path) -> Option<String> {
  extract_epub_meta(path).and_then(|(t, _)| t)
}

// ------------------------------------------------------------------ //
//  阶段二：搜索 + 交互选择
// ------------------------------------------------------------------ //

/// 搜索 claspclub API，返回书籍建议列表
async fn search_claspclub_api(
  client: &reqwest::Client,
  keyword: &str,
) -> Result<Vec<ClaspBookSuggestion>, IngestionError> {
  let encoded = percent_encoding::utf8_percent_encode(keyword, percent_encoding::NON_ALPHANUMERIC);
  let url = format!("https://claspclub.com/api/v1/search/suggestions?keyword={encoded}");
  info!("请求 API: {url}");

  let resp = client.get(&url).send().await?;
  if !resp.status().is_success() {
    return Err(IngestionError::NetworkError(format!("HTTP {}", resp.status())));
  }
  let body = resp.json::<spider::ClaspSuggestionResp>().await?;
  Ok(body.books)
}

/// 搜索 + 交互选择循环
///
/// - `opts.merged`：搜索有结果后直达多选模式（`--merged`）
/// - `opts.batch`：唯一结果自动选中，跳过确认（多结果仍走交互）
async fn search_and_select(
  client: &reqwest::Client,
  candidates: &[String],
  opts: IngestOptions,
) -> Result<Selected, IngestionError> {
  let mut merged_mode = opts.merged;

  // 依次尝试候选关键词，直到有结果或用完
  let mut items: Vec<ClaspBookSuggestion> = Vec::new();
  for kw in candidates {
    info!("尝试搜索: {kw}");
    items = search_claspclub_api(client, kw)
      .await
      .unwrap_or_else(|e| {
        warn!("claspclub API 搜索失败: {e}");
        Vec::new()
      });
    if !items.is_empty() {
      break;
    }
    warn!("用「{kw}」搜不到结果");
  }

  loop {
    if items.is_empty() {
      println!("\n未找到搜索结果。");
      let input = prompt_book_name()?;
      if input.trim().is_empty() {
        return Ok(Selected { suggestions: vec![manual_entry()?], auto: false });
      }
      items = search_claspclub_api(client, input.trim())
        .await
        .unwrap_or_default();
      continue;
    }

    // 合并模式：直达多选
    if merged_mode {
      match merged_multiselect(&items)? {
        Some(sel) => return Ok(sel),
        None => {
          merged_mode = false; // 未勾选任何条目，回退普通模式
          continue;
        }
      }
    }

    if items.len() == 1 {
      let i = &items[0];

      // 批量模式：唯一结果自动选中，省略所有确认
      if opts.batch {
        println!("  自动匹配唯一结果: 《{}》- {}", i.title, i.author_name);
        return Ok(Selected { suggestions: vec![i.clone()], auto: true });
      }

      println!("\n找到唯一结果：");
      println!("  书名: {}", i.title);
      println!("  作者: {}", i.author_name);
      println!("  标签: {}", utils::join_tags(&i.tags));
      let ok = Confirm::new()
        .with_prompt("确认选择此书？")
        .default(true)
        .interact()
        .map_err(io_abort)?;
      if ok {
        return Ok(Selected { suggestions: vec![i.clone()], auto: false });
      }
      let input = prompt_book_name()?;
      if input.trim().is_empty() {
        return Ok(Selected { suggestions: vec![manual_entry()?], auto: false });
      }
      items = search_claspclub_api(client, input.trim())
        .await
        .unwrap_or_default();
      continue;
    }

    // 多条结果
    let mut choices: Vec<String> = items
      .iter()
      .map(|i| format!("《{}》- {}", i.title, i.author_name))
      .collect();
    choices.push("手动输入书名".into());
    choices.push("这是合并本，多选匹配条目".into());
    choices.push("取消".into());

    let sel = Select::new()
      .with_prompt("请选择正确的书籍")
      .items(&choices)
      .default(0)
      .interact()
      .map_err(io_abort)?;

    if sel < items.len() {
      return Ok(Selected { suggestions: vec![items[sel].clone()], auto: false });
    }

    match sel - items.len() {
      0 => {
        // 手动输入书名
        let input = prompt_book_name()?;
        if input.trim().is_empty() {
          return Ok(Selected { suggestions: vec![manual_entry()?], auto: false });
        }
        items = search_claspclub_api(client, input.trim())
          .await
          .unwrap_or_default();
      }
      1 => {
        // 合并本多选
        match merged_multiselect(&items)? {
          Some(sel) => return Ok(sel),
          None => continue, // 空选择回到菜单
        }
      }
      _ => return Err(IngestionError::UserAbort), // 取消
    }
  }
}

/// 合并本多选：勾选若干条目
/// 返回 None 表示未勾选任何条目
fn merged_multiselect(
  items: &[ClaspBookSuggestion],
) -> Result<Option<Selected>, IngestionError> {
  let prompts: Vec<String> = items
    .iter()
    .map(|i| format!("《{}》- {}", i.title, i.author_name))
    .collect();
  println!("\n勾选包含在此合并本中的条目（空格勾选，回车确认）：");
  let picks = MultiSelect::new()
    .items(&prompts)
    .interact()
    .map_err(io_abort)?;

  if picks.is_empty() {
    return Ok(None);
  }
  Ok(Some(Selected {
    suggestions: picks.into_iter().map(|i| items[i].clone()).collect(),
    auto: false,
  }))
}

/// 提示用户输入书名以重新搜索（留空则进入手动录入）
fn prompt_book_name() -> Result<String, IngestionError> {
  Input::new()
    .with_prompt("输入书名以重新搜索（留空则手动录入）")
    .allow_empty(true)
    .default(String::new())
    .interact_text()
    .map_err(io_abort)
}

/// 手动录入书名和作者（无 clasp 元数据）
fn manual_entry() -> Result<ClaspBookSuggestion, IngestionError> {
  println!("\n进入手动录入模式：");
  let title: String = Input::new()
    .with_prompt("书名")
    .allow_empty(false)
    .interact_text()
    .map_err(io_abort)?;
  let author: String = Input::new()
    .with_prompt("作者（可留空）")
    .allow_empty(true)
    .default(String::new())
    .interact_text()
    .map_err(io_abort)?;
  Ok(ClaspBookSuggestion {
    title,
    author_name: author,
    id: String::new(),
    tags: vec![],
    cover_url: None,
    summary: None,
    douban_url: None,
  })
}

// ------------------------------------------------------------------ //
//  阶段五：封面下载
// ------------------------------------------------------------------ //

/// 当前 Unix 时间戳（秒；时钟异常时回退 0）
fn unix_secs() -> u64 {
  std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map(|d| d.as_secs())
    .unwrap_or(0)
}

/// 提取 EPUB 内嵌封面字节与扩展名（无内嵌封面或解析失败返回 None）
///
/// 对应 EPUB manifest 中的 cover-image 条目（rbook 已解析好），MIME 子类型转扩展名。
pub fn extract_epub_cover_bytes(path: &Path) -> Option<(Vec<u8>, String)> {
  let epub = rbook::Epub::open(path.to_string_lossy().as_ref()).ok()?;
  let entry = epub.manifest().cover_image()?;
  let bytes = entry.read_bytes().ok()?;
  if bytes.is_empty() {
    return None;
  }
  let kind = entry.kind();
  let ext = match kind.subtype() {
    "jpeg" => "jpg",
    "" => "jpg",
    other => other,
  };
  Some((bytes, ext.to_string()))
}

/// 提取 EPUB 内嵌封面并保存到封面缓存目录（无匹配书籍 / 下载失败时的默认封面来源）
///
/// 内容去重；保存失败返回 None（降级为无封面，不阻断导入）。
pub fn save_epub_cover(path: &Path, dir: &Path, _key: &str) -> Option<PathBuf> {
  let (bytes, ext) = extract_epub_cover_bytes(path)?;
  match save_cover_dedup(dir, &bytes, &ext) {
    Ok(dest) => {
      info!("采用 EPUB 内嵌封面: {}", dest.display());
      Some(dest)
    }
    Err(e) => {
      warn!("EPUB 内嵌封面保存失败: {e}");
      None
    }
  }
}

/// 生成豆瓣请求用的随机 bid cookie（规避部分反爬）
fn douban_bid() -> String {
  const CHARS: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
  let seed = std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map(|d| d.subsec_nanos() as usize)
    .unwrap_or(12345)
    | 1;
  let mut x = seed;
  (0..11)
    .map(|_| {
      x = x.wrapping_mul(1103515245).wrapping_add(12345);
      CHARS[(x >> 16) % CHARS.len()] as char
    })
    .collect()
}

/// 封面下载：先尝试 claspclub 阿里云 OSS（带 Referer 伪装），失败则回退豆瓣页面解析 og:image
async fn download_cover_robust(
  client: &reqwest::Client,
  oss_urls: &[String],
  douban_urls: &[String],
  dir: &Path,
  _key: &str,
) -> anyhow::Result<PathBuf> {
  // 1. OSS 直链：伪装为 claspclub 站内请求（阿里云 OSS 常按 Referer 防盗链）
  for url in oss_urls {
    let resp = client
      .get(url)
      .header("Referer", "https://claspclub.com/")
      .header("Accept", "image/avif,image/webp,image/apng,image/*,*/*;q=0.8")
      .send()
      .await;
    match resp {
      Ok(r) if r.status().is_success() => match r.bytes().await {
        Ok(bytes) if bytes.len() > 1024 => {
          let ext = if url.to_lowercase().contains(".png") { "png" } else { "jpg" };
          let path = save_cover_dedup(dir, &bytes, ext)?;
          info!("封面已保存（OSS，内容去重）: {}", path.display());
          return Ok(path);
        }
        _ => warn!("OSS 封面响应异常: {url}"),
      },
      Ok(r) => warn!("OSS 封面 HTTP {}: {url}", r.status()),
      Err(e) => warn!("OSS 封面请求失败: {e}"),
    }
  }

  // 2. 豆瓣兜底：从书籍页解析 og:image 后下载
  for page in douban_urls {
    let img = match fetch_douban_cover(client, page).await {
      Ok(Some(u)) => u,
      _ => continue,
    };
    let resp = client
      .get(&img)
      .header("Referer", "https://book.douban.com/")
      .header("Cookie", format!("bid={}", douban_bid()))
      .send()
      .await;
    if let Ok(r) = resp {
      if r.status().is_success() {
        if let Ok(bytes) = r.bytes().await {
          if bytes.len() > 1024 {
            let ext = if img.contains(".png") { "png" } else { "jpg" };
            let path = save_cover_dedup(dir, &bytes, ext)?;
            info!("封面已保存（豆瓣，内容去重）: {}", path.display());
            return Ok(path);
          }
        }
      }
    }
    warn!("豆瓣封面下载失败: {img}");
  }

  anyhow::bail!("所有封面来源均失败（OSS + 豆瓣）")
}

/// 从豆瓣书籍页 HTML 中解析 og:image 封面地址
async fn fetch_douban_cover(
  client: &reqwest::Client,
  page_url: &str,
) -> anyhow::Result<Option<String>> {
  let resp = client
    .get(page_url)
    .header(
      "User-Agent",
      "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36",
    )
    .header("Accept", "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8")
    .header("Cookie", format!("bid={}", douban_bid()))
    .send()
    .await?;
  if !resp.status().is_success() {
    anyhow::bail!("豆瓣页面 HTTP {}", resp.status());
  }
  let html = resp.text().await?;

  // 对应豆瓣书籍页: <meta property="og:image" content="https://img...doubanio.com/...jpg" />
  // 兜底: <div id="mainpic"> 内的 img src
  if let Ok(re) = regex::Regex::new(r#"<meta[^>]+property="og:image"[^>]+content="([^"]+)""#) {
    if let Some(c) = re.captures(&html) {
      return Ok(Some(c[1].to_string()));
    }
  }
  if let Ok(re) = regex::Regex::new(r#"<meta[^>]+content="([^"]+)"[^>]+property="og:image""#) {
    if let Some(c) = re.captures(&html) {
      return Ok(Some(c[1].to_string()));
    }
  }
  if let Ok(re) = regex::Regex::new(r#"<div id="mainpic"[^>]*>.*?<img[^>]*src="([^"]+)""#) {
    if let Some(c) = re.captures(&html) {
      return Ok(Some(c[1].to_string()));
    }
  }
  Ok(None)
}

// ------------------------------------------------------------------ //
//  书名规范化
// ------------------------------------------------------------------ //

/// 书名规范化：繁体自动转简体；非中文标题由用户确认（批量模式也不例外）
fn normalize_title(title: &str) -> Result<String, IngestionError> {
  use character_converter::traditional_to_simplified;

  if utils::count_han(title) > 0 {
    let simplified = traditional_to_simplified(title).into_owned();
    if simplified != title {
      info!("书名已转为简体: {simplified}");
    }
    Ok(simplified)
  } else {
    let t: String = Input::new()
      .with_prompt("书名不是中文，请确认或修改")
      .allow_empty(true)
      .default(title.to_string())
      .interact_text()
      .map_err(io_abort)?;
    Ok(if t.trim().is_empty() { title.to_string() } else { t })
  }
}

// ------------------------------------------------------------------ //
//  阶段六
// ------------------------------------------------------------------ //

/// 打印抓取到的元数据，逐项询问用户修改（书名/作者/标签/简介）
fn interact_edit(
  meta: &ClaspBookMeta,
  description: &str,
) -> Result<(String, String, Vec<String>, String), IngestionError> {
  println!("\n抓取到以下元数据：");
  println!("  书名: {}", meta.title);
  println!("  作者: {}", meta.author);
  println!("  标签: {}", utils::join_tags(&meta.tags));
  if let Some(name) = &meta.series_name {
    match meta.series_order {
      Some(o) => println!("  系列: {name} #{o}"),
      None => println!("  系列: {name}"),
    }
  }
  if !description.is_empty() {
    let preview: String = description.chars().take(80).collect();
    println!("  简介: {preview}...");
  }
  println!("\n请确认或修改（直接回车保留原值）：");

  let title: String = Input::new()
    .with_prompt("书名")
    .allow_empty(true)
    .default(meta.title.clone())
    .interact_text()
    .map_err(io_abort)?;

  let author: String = Input::new()
    .with_prompt("作者")
    .allow_empty(true)
    .default(meta.author.clone())
    .interact_text()
    .map_err(io_abort)?;

  let tags_str: String = Input::new()
    .with_prompt("标签（英文逗号分隔）")
    .allow_empty(true)
    .default(utils::join_tags(&meta.tags))
    .interact_text()
    .map_err(io_abort)?;

  let tags = if tags_str.trim().is_empty() {
    Vec::new()
  } else {
    utils::split_tags(&tags_str)
  };

  let desc_input: String = Input::new()
    .with_prompt("简介（回车保留；输入 - 清空；或输入新内容）")
    .allow_empty(true)
    .default(String::new())
    .interact_text()
    .map_err(io_abort)?;

  let description = if desc_input.trim() == "-" {
    String::new()
  } else if desc_input.trim().is_empty() {
    description.to_string()
  } else {
    desc_input
  };

  Ok((title, author, tags, description))
}

// ------------------------------------------------------------------ //
//  阶段七：EPUB 写入
// ------------------------------------------------------------------ //

/// 将元数据写入 EPUB 文件（无交互确认，供 write_epub / merge 调用）
/// 写入 dc:title / dc:creator / dc:subject / dc:description / calibre:series(_index) / 封面
pub fn write_epub_metadata(
  path: &Path,
  title: &str,
  author: &str,
  tags: &[String],
  description: Option<&str>,
  series: Option<(&str, i64)>,
  cover: Option<&Path>,
) -> Result<(), String> {
  let mut epub =
    rbook::Epub::open(path.to_string_lossy().as_ref()).map_err(|e| e.to_string())?;

  let mut editor = epub
    .edit()
    .clear_meta("dc:title")
    .title(title)
    .clear_meta("dc:creator")
    .author(author)
    .clear_meta("dc:subject")
    .clear_meta("dc:description")
    .clear_meta("calibre:series")
    .clear_meta("calibre:series_index");

  if let Some(d) = description {
    if !d.trim().is_empty() {
      editor = editor.description(d);
    }
  }

  if let Some((name, order)) = series {
    editor = editor
      .meta(("calibre:series", name))
      .meta(("calibre:series_index", order.to_string()));
  }

  if let Some(c) = cover {
    let ext = c.extension().and_then(|e| e.to_str()).unwrap_or("jpg");
    // 使用独特的 href，避免与 EPUB 内已有封面文件名冲突
    editor = editor.cover_image((format!("mystery-agent-cover.{ext}"), c.to_path_buf()));
  }

  for tag in tags {
    editor = editor.tag(tag);
  }

  let editor = editor.modified_now();

  let bytes = editor
    .write()
    .compression(9)
    .to_vec()
    .map_err(|e| e.to_string())?;

  std::fs::write(path, &bytes).map_err(|e| e.to_string())?;
  Ok(())
}

/// 将元数据写入书库副本（目标为副本，原文件不受影响，无需确认与备份）
fn write_epub(
  path: &Path,
  meta: &ClaspBookMeta,
  description: &str,
  cover: Option<&Path>,
) -> Result<(), IngestionError> {
  if !path.exists() {
    return Err(IngestionError::IoError(std::io::Error::new(
      std::io::ErrorKind::NotFound,
      format!("EPUB 文件不存在: {}", path.display()),
    )));
  }

  let series = meta
    .series_name
    .as_ref()
    .map(|n| (n.as_str(), meta.series_order.unwrap_or(1)));

  write_epub_metadata(
    path,
    &meta.title,
    &meta.author,
    &meta.tags,
    Some(description).filter(|s| !s.trim().is_empty()),
    series,
    cover,
  )
  .map_err(IngestionError::EpubError)?;
  info!("EPUB 元数据写入成功");
  Ok(())
}

// ------------------------------------------------------------------ //
//  阶段十：豆瓣短评
// ------------------------------------------------------------------ //

async fn fetch_douban(
  client: &reqwest::Client,
  url: &str,
) -> Result<Vec<spider::Comment>, IngestionError> {
  info!("抓取豆瓣短评: {url}");
  let resp = client.get(url).send().await?;
  if !resp.status().is_success() {
    return Err(IngestionError::NetworkError(format!("HTTP {}", resp.status())));
  }
  let html = resp.text().await?;
  let raw = spider::parse_douban_comments(&html);
  Ok(spider::filter_comments(raw))
}

// ------------------------------------------------------------------ //
//  输出
// ------------------------------------------------------------------ //

fn print_summary(meta: &ClaspBookMeta, book_id: i64, description: &str, library_file: Option<&str>) {
  println!("\n========== 入库完成 ==========");
  println!("  书名: {}", meta.title);
  println!("  作者: {}", meta.author);
  println!("  标签: {}", utils::join_tags(&meta.tags));
  if let Some(name) = &meta.series_name {
    match meta.series_order {
      Some(o) => println!("  系列: {name} #{o}"),
      None => println!("  系列: {name}"),
    }
  }
  if !description.is_empty() {
    let preview: String = description.chars().take(60).collect();
    println!("  简介: {preview}...");
  }
  for url in &meta.douban_urls {
    println!("  豆瓣: {url}");
  }
  for id in &meta.clasp_ids {
    println!("  Clasp: {id}");
  }
  if let Some(f) = library_file {
    println!("  书库: {f}");
  }
  println!("  数据库 ID: {book_id}");
  println!("==============================\n");
}

// ------------------------------------------------------------------ //
//  GUI 非交互式导入（Tauri 前端复用，不依赖 dialoguer）
// ------------------------------------------------------------------ //

/// GUI 导入参数：claspclub 搜索匹配条目（搜索页单选/多选回传，多选 = 合并本）
///
/// 分页搜索接口直接携带简介与豆瓣链接 → 导入时省去详情 API 调用；
/// `summary` 为空（旧建议接口路径）时回退详情 API 逐条增强。
#[derive(Debug, Clone, Default)]
pub struct SuggestionMatch {
  pub id: String,
  /// 条目书名（来源展示与合并元数据用）
  pub title: String,
  /// 条目作者（多选合并时取并集去重）
  pub author: String,
  pub tags: Vec<String>,
  pub cover_url: Option<String>,
  /// 无剧透简介（分页搜索接口直接返回）
  pub summary: Option<String>,
  /// 豆瓣书籍页链接（搜索接口从封面推断）
  pub douban_url: Option<String>,
}

/// analyze_epub 的分析结果（供前端确认弹窗展示与编辑）
#[derive(Debug, Clone)]
pub struct EpubAnalysis {
  /// 书名：静默匹配命中时优先采用爬取到的（繁转简），否则 EPUB 内嵌标题 → 文件名兜底
  pub title: String,
  /// 作者：静默匹配命中时优先采用爬取到的，否则 EPUB 内嵌作者（可能为空）
  pub author: String,
  /// 书名是否含中文（非中文标题必须经前端确认后才可导入）
  pub is_chinese_title: bool,
  /// 是否有内嵌封面（无匹配时前端将默认采用 EPUB 封面，供确认弹窗预览）
  pub has_epub_cover: bool,
  /// zip 重建副本路径（原 EPUB 含重复条目时存在；导入应使用该路径处理）
  pub effective_path: Option<PathBuf>,
  /// 静默匹配到的 clasp 条目（歧义/无结果时为 None，降级为本地元数据）
  pub suggestion: Option<ClaspBookSuggestion>,
  /// 搜索结果总数（分页接口 total，供前端提示匹配质量）
  pub search_results: usize,
}

/// GUI 搜索分页大小（分页搜索接口）
pub const GUI_PAGE_SIZE: i64 = 5;

/// 一页分页搜索结果（条目已统一为建议结构）
pub struct ClaspSearchPage {
  pub items: Vec<ClaspBookSuggestion>,
  /// 总页数（前端翻页按钮用；接口异常时为 1）
  pub total_pages: i64,
  /// 结果总数
  pub total: i64,
  /// true = claspclub 无精确匹配，正在返回相近结果（视为未搜到）
  pub fuzzy: bool,
}

/// 分析 EPUB：提取内嵌元数据 + claspclub 搜索静默匹配
///
/// 与交互式流程的区别：不弹出任何菜单；唯一结果或与书名精确一致的
/// 结果自动采用，歧义时降级为 EPUB 内嵌元数据（数据质量优于数量）。
/// 搜索使用分页接口（按豆瓣评分排序，pageSize = 5），条目自带简介。
pub async fn analyze_epub(path: &Path) -> Result<EpubAnalysis, IngestionError> {
  use character_converter::traditional_to_simplified;

  if !path.exists() {
    return Err(IngestionError::IoError(std::io::Error::new(
      std::io::ErrorKind::NotFound,
      format!("文件不存在: {}", path.display()),
    )));
  }
  // 部分来源 EPUB 的 zip 含重复条目，无法直接解析；自动重建为干净副本后继续
  let effective = match sanitize_epub_if_needed(path)? {
    Some(fixed) => fixed,
    None => path.to_path_buf(),
  };
  let (raw_title, raw_author) = extract_epub_meta(&effective).ok_or_else(|| {
    IngestionError::EpubError(format!("EPUB 解析失败（文件可能已损坏）: {}", path.display()))
  })?;

  let fallback = utils::filename_stem(path);
  let mut title = raw_title.filter(|s| !s.trim().is_empty()).unwrap_or_else(|| fallback.clone());
  if title.trim().is_empty() {
    title = fallback.clone();
  }
  let is_chinese = utils::count_han(&title) > 0;
  if is_chinese {
    title = traditional_to_simplified(&title).into_owned();
  }

  // 依次尝试候选关键词搜索（标题优先，文件名兜底）
  let mut candidates: Vec<String> = vec![title.clone()];
  if !fallback.is_empty() && !candidates.contains(&fallback) {
    candidates.push(fallback);
  }

  // 并发执行：本地封面探测（阻塞任务）与网络搜索同时进行，互不等待
  let cover_path = effective.clone();
  let client = gui_http_client();
  let cover_fut = tokio::task::spawn_blocking(move || {
    extract_epub_cover_bytes(&cover_path).is_some()
  });
  let search_fut = search_candidates(&client, &candidates);
  let (cover_res, search_res) = tokio::join!(cover_fut, search_fut);
  let has_epub_cover = cover_res.unwrap_or(false);
  let (items, total) = search_res;

  let suggestion = pick_silent_match(&items, &title);

  // 书名/作者优先采用爬取到的（claspclub 静默匹配结果），EPUB 原有值仅兜底；
  // 爬取到的标题同样做繁转简，非中文标题仍交由前端确认
  let title = match suggestion.as_ref() {
    Some(s) if !s.title.trim().is_empty() => {
      let t = s.title.trim().to_string();
      if utils::count_han(&t) > 0 {
        traditional_to_simplified(&t).into_owned()
      } else {
        t
      }
    }
    _ => title,
  };
  let author = match suggestion.as_ref() {
    Some(s) if !s.author_name.trim().is_empty() => s.author_name.trim().to_string(),
    _ => raw_author.clone().unwrap_or_default(),
  };
  let is_chinese = utils::count_han(&title) > 0;

  Ok(EpubAnalysis {
    title,
    author,
    is_chinese_title: is_chinese,
    // zip 重建副本路径（无需修复时为 None；导入应使用该路径处理）
    effective_path: (effective != path).then_some(effective.clone()),
    has_epub_cover,
    suggestion,
    search_results: if total > 0 { total as usize } else { items.len() },
  })
}

/// 候选关键词搜索：并发发出全部候选（≤2 个），按候选顺序取首个非空结果
///
/// 相比顺序尝试：最坏情形（首选无结果）耗时 ≈ 单次请求而非两次串行请求
async fn search_candidates(
  client: &reqwest::Client,
  candidates: &[String],
) -> (Vec<ClaspBookSuggestion>, i64) {
  match candidates {
    [] => (Vec::new(), 0),
    [first, second] => {
      let (r1, r2) = tokio::join!(search_one(client, first), search_one(client, second));
      // 首选命中则忽略备用候选的结果
      if !r1.0.is_empty() { r1 } else { r2 }
    }
    many => {
      for kw in many {
        let r = search_one(client, kw).await;
        if !r.0.is_empty() {
          return r;
        }
      }
      (Vec::new(), 0)
    }
  }
}

/// 单个关键词的静默搜索：fuzzy 视为未搜到（宁缺毋滥），失败仅告警
async fn search_one(
  client: &reqwest::Client,
  kw: &str,
) -> (Vec<ClaspBookSuggestion>, i64) {
  match search_clasp_page_with(client, kw, 1).await {
    Ok(page) => {
      if page.fuzzy {
        (Vec::new(), page.total)
      } else {
        (page.items, page.total)
      }
    }
    Err(e) => {
      warn!("claspclub API 搜索失败: {e}");
      (Vec::new(), 0)
    }
  }
}

/// 静默匹配规则：唯一结果直接采用；多条时采用与书名精确一致（繁简/大小写归一后）
/// 的条目；仍歧义则返回 None（降级为本地元数据，宁缺毋滥）
fn pick_silent_match(
  items: &[ClaspBookSuggestion],
  title: &str,
) -> Option<ClaspBookSuggestion> {
  let norm = |s: &str| {
    character_converter::traditional_to_simplified(s.trim())
      .into_owned()
      .to_lowercase()
  };
  if items.len() == 1 {
    return Some(items[0].clone());
  }
  let target = norm(title);
  items
    .iter()
    .find(|i| norm(&i.title) == target)
    .cloned()
}

/// GUI 专用 HTTP 客户端（详情/搜索 15s 超时 + 浏览器 UA）
pub fn gui_http_client() -> reqwest::Client {
  reqwest::Client::builder()
    .timeout(std::time::Duration::from_secs(15))
    .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36")
    .build()
    .expect("构建 HTTP 客户端失败") // 常量配置，构建失败只可能是 TLS 后端缺失
}

/// prepare_import 的产出：完成全部网络/文件操作，待写入数据库
#[derive(Debug, Clone)]
pub struct PreparedImport {
  /// 处理用 EPUB 路径（zip 含重复条目时为重建的临时副本；finalize_import 的复制源）
  pub file_path: String,
  /// 原始导入路径（溯源记录，写入 books.file_path）
  pub original_path: String,
  pub title: String,
  pub author: String,
  pub tags: Vec<String>,
  pub description: String,
  pub series_name: Option<String>,
  pub series_order: Option<i64>,
  pub clasp_ids: Vec<String>,
  pub douban_urls: Vec<String>,
  /// 书库内规范文件名（相对路径；finalize_import 后填入）
  pub library_file: String,
  pub cover_path: Option<PathBuf>,
  /// 确认弹窗中手动上传的封面（finalize_import 时复制入缓存并采用）
  pub cover_override: Option<PathBuf>,
  /// 预爬完成的 claspclub 来源项目（有序）
  pub sources_clasp: Vec<PendingSource>,
  /// 预爬完成的豆瓣来源项目（有序）
  pub sources_douban: Vec<PendingSource>,
  /// 简介合并的 LLM 用量（模型, 用量），persist_import 时入库统计
  pub llm_usage: Vec<(String, agent::TokenUsage)>,
  /// 简介融合所用模型（未融合/降级时为 None）
  pub fusion_model: Option<String>,
  /// 简介融合降级/失败原因（GUI 提示用；None 表示融合正常）
  pub fusion_error: Option<String>,
}

/// GUI 搜索页：claspclub 分页搜索（按豆瓣评分排序，pageSize = 5）
///
/// 返回条目已统一为建议结构（含无剧透简介、推断的豆瓣链接）
pub async fn search_clasp_page(keyword: &str, page: i64) -> Result<ClaspSearchPage, IngestionError> {
  let client = gui_http_client();
  search_clasp_page_with(&client, keyword, page).await
}

/// 同上，但复用调用方的 HTTP 客户端（避免每次请求重复 TLS 握手）
async fn search_clasp_page_with(
  client: &reqwest::Client,
  keyword: &str,
  page: i64,
) -> Result<ClaspSearchPage, IngestionError> {
  let resp = spider::search_books_paged(client, keyword.trim(), page, GUI_PAGE_SIZE)
    .await
    .map_err(|e| IngestionError::Other(e.to_string()))?;
  let pagination = resp.pagination.clone().unwrap_or_default();
  Ok(ClaspSearchPage {
    items: resp.items.iter().map(ClaspBookSuggestion::from).collect(),
    total_pages: pagination.total_pages.max(1),
    total: pagination.total,
    fuzzy: resp.search_mode.as_deref() == Some("fuzzy"),
  })
}

/// 下载远程图片字节（GUI 搜索结果封面展示用）
///
/// OSS 图床按 Referer 防盗链：claspclub 封面伪装站内请求，
/// 豆瓣图床伪装书籍页请求（带随机 bid cookie）。
pub async fn fetch_remote_image(url: &str) -> Result<Vec<u8>, IngestionError> {
  let client = gui_http_client();
  let mut req = client
    .get(url)
    .header("Accept", "image/avif,image/webp,image/apng,image/*,*/*;q=0.8");
  if url.contains("doubanio.com") {
    req = req
      .header("Referer", "https://book.douban.com/")
      .header("Cookie", format!("bid={}", douban_bid()));
  } else {
    req = req.header("Referer", "https://claspclub.com/");
  }
  let resp = req.send().await?;
  if !resp.status().is_success() {
    return Err(IngestionError::NetworkError(format!("HTTP {}", resp.status())));
  }
  Ok(resp.bytes().await?.to_vec())
}

// ------------------------------------------------------------------ //
//  来源项目爬取（导入预爬 / 来源管理添加共用）
// ------------------------------------------------------------------ //

/// 来源项目的版本封面元数据（持久化为 sources.editions JSON）
#[derive(Debug, Clone, serde::Serialize)]
pub struct EditionCoverMeta {
  pub label: String,
  pub url: String,
  /// 本地缓存路径（预爬下载成功时存在）
  pub path: Option<String>,
}

/// 待入库的来源项目（爬取产物，导入 / 来源管理共用）
#[derive(Debug, Clone, Default)]
pub struct PendingSource {
  /// clasp 条目 ID 或豆瓣书籍页 URL
  pub ref_key: String,
  pub title: Option<String>,
  pub author: Option<String>,
  pub cover_url: Option<String>,
  /// 本地缓存封面路径
  pub cover_path: Option<PathBuf>,
  pub summary: Option<String>,
  /// 标签（仅 claspclub 来源有）
  pub tags: Option<Vec<String>>,
  /// clasp 版本封面 JSON（豆瓣项目为 None）
  pub editions_json: Option<String>,
  /// 推断的豆瓣书籍页链接（clasp 项目用于自动同步豆瓣来源）
  pub douban_url: Option<String>,
  /// 系列信息（仅 clasp 详情返回；导入时用于继承规则；卷号缺失时为 None）
  pub series: Option<(String, Option<i64>)>,
  /// 过滤后的短评（≥15 汉字，按有用数 top5）
  pub comments: Vec<spider::Comment>,
}

impl PendingSource {
  fn new(ref_key: &str) -> Self {
    PendingSource { ref_key: ref_key.to_string(), ..Default::default() }
  }
}

/// 封面文件内容键（去重）：64 位内容哈希 + 字节数（碰撞概率可忽略）
fn cover_content_key(bytes: &[u8]) -> String {
  use std::hash::{Hash, Hasher};
  let mut h = std::collections::hash_map::DefaultHasher::new();
  bytes.hash(&mut h);
  format!("{:016x}-{}", h.finish(), bytes.len())
}

/// 将封面字节写入缓存目录（内容去重：同内容图片复用已有文件，引用计数各自累加）
pub fn save_cover_dedup(dir: &Path, bytes: &[u8], ext: &str) -> std::io::Result<PathBuf> {
  let key = cover_content_key(bytes);
  let dest = dir.join(format!("{key}.{ext}"));
  if !dest.exists() {
    std::fs::write(&dest, bytes)?;
  }
  Ok(dest)
}

/// 爬取一个 claspclub 来源项目：详情（标题/作者/简介）+ 主封面 +
/// 全部版本封面（预爬本地）+ 短评 top5（尽力而为，失败仅告警）
pub async fn crawl_clasp_source(client: &reqwest::Client, id: &str) -> PendingSource {
  let mut ps = PendingSource::new(id);
  let detail = match spider::fetch_book_detail(client, id).await {
    Ok(d) => d,
    Err(e) => {
      warn!("claspclub 详情获取失败 [{id}]: {e}");
      return ps;
    }
  };

  ps.title = detail.title.clone().filter(|s| !s.trim().is_empty());
  ps.author = detail
    .author
    .as_ref()
    .map(|a| a.name.trim().to_string())
    .filter(|s| !s.is_empty());
  ps.summary = detail
    .summary_no_spoiler
    .clone()
    .filter(|s| !s.trim().is_empty());
  ps.cover_url = detail.cover_url.clone();

  // 版本封面：仅记录链接（不落盘，展示/应用时经代理伪装 Referer 访问）
  let mut editions: Vec<EditionCoverMeta> = Vec::new();
  for e in detail.editions.iter() {
    let Some(url) = e.cover_url.as_deref().map(str::trim).filter(|u| !u.is_empty()) else {
      continue;
    };
    if editions.iter().any(|x| x.url == url) {
      continue;
    }
    editions.push(EditionCoverMeta {
      label: edition_label(e),
      url: url.to_string(),
      path: None,
    });
  }
  if !editions.is_empty() {
    ps.editions_json = serde_json::to_string(&editions).ok();
  }

  // 豆瓣链接：版本表精准链接优先，封面 URL 推断兜底
  ps.douban_url = detail
    .editions
    .iter()
    .find(|e| e.is_primary)
    .and_then(|e| e.douban_url.clone())
    .or_else(|| detail.editions.iter().find_map(|e| e.douban_url.clone()))
    .or_else(|| {
      detail
        .cover_url
        .as_deref()
        .and_then(spider::extract_douban_id_from_cover)
        .map(|id| spider::douban_book_url(&id))
    });

  // 系列信息（导入时用于继承规则；卷号缺失时仅保留系列名，不丢弃）
  if let Some(series) = detail.series {
    if let Some(name) = series
      .name
      .map(|n| n.trim().to_string())
      .filter(|n| !n.is_empty())
    {
      ps.series = Some((name, series.order));
    }
  }

  // 短评（接口未公开文档化，尽力而为）
  match spider::fetch_clasp_reviews(client, id).await {
    Ok(raw) => ps.comments = spider::filter_comments(raw),
    Err(e) => warn!("claspclub 评论抓取失败 [{id}]: {e}"),
  }
  ps
}

/// 爬取一个豆瓣来源项目：页面（标题/作者/封面链接/简介）+ 短评 top5
pub async fn crawl_douban_source(client: &reqwest::Client, url: &str) -> PendingSource {
  let mut ps = PendingSource::new(url);

  // 页面 HTML 一次获取（meta + 简介 + 封面共用）
  let html = match fetch_douban_page(client, url).await {
    Ok(h) => h,
    Err(e) => {
      warn!("豆瓣页面获取失败 [{url}]: {e}");
      return ps;
    }
  };
  let meta = spider::parse_douban_book_meta(&html);
  ps.title = meta.title;
  ps.author = meta.author;
  ps.cover_url = meta.cover_url.clone();
  ps.summary = meta.summary;

  // 短评（短评页单独请求）
  let comments_url = spider::douban_comments_url(url);
  match fetch_douban(client, &comments_url).await {
    Ok(cs) => ps.comments = cs,
    Err(e) => warn!("豆瓣短评抓取失败 [{url}]: {e}"),
  }
  ps
}

/// 获取豆瓣页面 HTML（伪装浏览器 UA + bid cookie）
async fn fetch_douban_page(client: &reqwest::Client, page_url: &str) -> anyhow::Result<String> {
  let resp = client
    .get(page_url)
    .header(
      "User-Agent",
      "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36",
    )
    .header("Accept", "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8")
    .header("Cookie", format!("bid={}", douban_bid()))
    .send()
    .await?;
  if !resp.status().is_success() {
    anyhow::bail!("豆瓣页面 HTTP {}", resp.status());
  }
  Ok(resp.text().await?)
}

/// 抓取一组豆瓣链接的短评（已过滤；单条失败仅告警跳过）
pub async fn fetch_comments_for_urls(urls: &[String]) -> Vec<spider::Comment> {
  let client = gui_http_client();
  let mut out = Vec::new();
  for url in urls {
    let comments_url = spider::douban_comments_url(url);
    match fetch_douban(&client, &comments_url).await {
      Ok(cs) => out.extend(cs),
      Err(e) => warn!("豆瓣短评抓取失败 [{url}]: {e}"),
    }
  }
  out
}

/// 来源项目写入 sources 表并入库其短评（带来源定位），回写 books 来源列
pub fn persist_sources(
  conn: &rusqlite::Connection,
  book_id: i64,
  clasp: &[PendingSource],
  douban: &[PendingSource],
) -> rusqlite::Result<()> {
  for (i, s) in clasp.iter().enumerate() {
    db::insert_source(
      conn,
      book_id,
      "clasp",
      &s.ref_key,
      i as i64,
      s.title.as_deref(),
      s.author.as_deref(),
      s.cover_url.as_deref(),
      s.cover_path.as_ref().map(|p| p.to_string_lossy().into_owned()).as_deref(),
      s.summary.as_deref(),
      s.tags.as_deref().and_then(|t| serde_json::to_string(t).ok()).as_deref(),
      s.series.as_ref().map(|(n, _)| n.as_str()),
      s.series.as_ref().and_then(|(_, o)| *o),
      s.editions_json.as_deref(),
    )?;
    for c in &s.comments {
      let _ = db::insert_comment(conn, book_id, c.rating, &c.content, c.usefulness, "claspclub", Some(&s.ref_key));
    }
  }
  for (i, s) in douban.iter().enumerate() {
    db::insert_source(
      conn,
      book_id,
      "douban",
      &s.ref_key,
      i as i64,
      s.title.as_deref(),
      s.author.as_deref(),
      s.cover_url.as_deref(),
      s.cover_path.as_ref().map(|p| p.to_string_lossy().into_owned()).as_deref(),
      s.summary.as_deref(),
      None,
      None,
      None,
      None,
    )?;
    for c in &s.comments {
      let _ = db::insert_comment(conn, book_id, c.rating, &c.content, c.usefulness, "豆瓣", Some(&s.ref_key));
    }
  }
  db::sync_book_source_columns(conn, book_id)?;
  Ok(())
}

/// 导入简介定稿（多条规则）：确认合并且 LLM 可用 → LLM 融合（可打断）；
/// 取消 / 未配置 LLM / 融合失败 → 取第一本
/// 返回（简介文本, LLM 用量, 降级原因）
pub async fn pick_import_description(
  summaries: &[String],
  merge: bool,
  cancel: &CancelToken,
) -> Result<(String, Option<(String, agent::TokenUsage)>, Option<String>), IngestionError> {
  let descs: Vec<&str> = summaries
    .iter()
    .map(|s| s.trim())
    .filter(|s| !s.is_empty())
    .collect();
  if descs.is_empty() {
    return Ok((String::new(), None, None));
  }
  if descs.len() == 1 {
    return Ok((descs[0].to_string(), None, None));
  }
  if merge {
    if agent::resolve_config().is_none() {
      return Ok((descs[0].to_string(), None, Some("未配置 LLM，已取第一本简介".into())));
    }
    let owned: Vec<String> = descs.iter().map(|s| s.to_string()).collect();
    let fusion_res = tokio::select! {
      r = agent::combine_descriptions(&owned) => r,
      _ = cancel.wait_cancelled() => return Err(IngestionError::Cancelled),
    };
    match fusion_res {
      Ok(fusion) => return Ok((fusion.text, fusion.llm, fusion.degraded)),
      Err(e) => {
        return Ok((
          descs[0].to_string(),
          None,
          Some(format!("LLM 合并失败，已取第一本简介: {}", brief_error(&e.to_string()))),
        ));
      }
    }
  }
  Ok((descs[0].to_string(), None, None))
}

/// 非交互式导入第一阶段：完成网络增强（来源预爬 / 简介定稿 / 封面）与
/// 书库复制、EPUB 副本元数据写入。不接触数据库，便于 GUI 层在
/// 异步上下文中短暂持锁。每一步失败都按降级链继续（详见 AGENTS.md §5）。
///
/// - `tags`：用户在弹窗中编辑后的标签（空则默认"推理小说"）
/// - `matches`：claspclub 搜索页选中的条目（多条 = 合并本：封面取第一本、
///   作者/标签取并集去重、系列需全部条目同系列同卷号）
/// - `douban_links`：手动填写的豆瓣书籍页链接（可多条，clasp 无简介时兜底）
/// - `merge_summaries`：多条简介时是否调用 LLM 合并（取消/未配置 → 取第一本）
/// - `cancel`：取消令牌，各阶段间检查；`progress`：进度回调（GUI 实时渲染）
pub async fn prepare_import(
  epub_path: PathBuf,
  original_path: Option<&str>,
  title: &str,
  author: &str,
  tags: &[String],
  matches: &[SuggestionMatch],
  douban_links: &[String],
  merge_summaries: bool,
  cancel: &CancelToken,
  progress: &(dyn Fn(TaskProgress) + Send + Sync),
) -> Result<PreparedImport, IngestionError> {
  cancel.check()?;
  if !epub_path.exists() {
    return Err(IngestionError::IoError(std::io::Error::new(
      std::io::ErrorKind::NotFound,
      format!("文件不存在: {}", epub_path.display()),
    )));
  }

  let client = gui_http_client();

  // 标签：以用户编辑值为准；确保当前书库的固定标签（默认"推理小说"）
  let default_tags = crate::config::AppConfig::load().default_tags();
  let mut tag_list: Vec<String> = tags.iter().map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect();
  for t in &default_tags {
    if !tag_list.contains(t) {
      tag_list.push(t.clone());
    }
  }
  if tag_list.is_empty() {
    tag_list.push("推理小说".to_string());
  }

  let mut meta = ClaspBookMeta {
    title: title.to_string(),
    author: author.to_string(),
    tags: tag_list,
    ..Default::default()
  };
  let mut clasp_summaries: Vec<String> = Vec::new();
  let mut cover_urls: Vec<String> = Vec::new();
  let mut series_pairs: Vec<(String, Option<i64>)> = Vec::new();
  let mut clasp_sources: Vec<PendingSource> = Vec::new();
  let push_douban = |urls: &mut Vec<String>, u: String| {
    if !u.trim().is_empty() && !urls.contains(&u) {
      urls.push(u);
    }
  };

  // 阶段一：逐 clasp 匹配条目预爬（详情 + 主封面 + 全部版本封面 + 短评；可打断）
  let match_list: Vec<&SuggestionMatch> = matches.iter().filter(|m| !m.id.trim().is_empty()).collect();
  let match_total = match_list.len() as i64;
  for (ci, m) in match_list.iter().enumerate() {
    cancel.check()?;
    progress(TaskProgress {
      phase: "source".into(),
      current: ci as i64 + 1,
      total: match_total,
      message: format!("爬取 claspclub 来源 {}/{}：{}", ci + 1, match_total, m.title),
    });

    meta.clasp_ids.push(m.id.clone());
    if let Some(c) = m.cover_url.as_deref().filter(|c| !c.trim().is_empty()) {
      if !cover_urls.contains(&c.to_string()) {
        cover_urls.push(c.to_string());
      }
    }
    // 搜索接口快路径：简介/豆瓣链接（爬取失败时的兜底数据）
    let fast_summary = m.summary.as_deref().filter(|s| !s.trim().is_empty());
    if let Some(s) = fast_summary {
      clasp_summaries.push(s.to_string());
    }

    // 来源预爬（含详情 API；select! 可打断）
    let mut ps = tokio::select! {
      r = crawl_clasp_source(&client, &m.id) => r,
      _ = cancel.wait_cancelled() => return Err(IngestionError::Cancelled),
    };
    // 快路径简介缺失时用爬取结果补齐
    if fast_summary.is_none() {
      if let Some(s) = ps.summary.take() {
        clasp_summaries.push(s);
      }
    }
    // 豆瓣链接：版本表精准链接优先，搜索接口推断兜底
    let derived = ps.douban_url.take().or_else(|| {
      m.douban_url
        .as_deref()
        .filter(|u| !u.trim().is_empty())
        .map(str::to_string)
    });
    if let Some(u) = derived {
      push_douban(&mut meta.douban_urls, u);
    }
    if let Some(c) = ps.cover_url.as_deref().filter(|c| !c.trim().is_empty()) {
      if !cover_urls.contains(&c.to_string()) {
        cover_urls.push(c.to_string());
      }
    }
    if let Some(s) = ps.series.take() {
      series_pairs.push(s);
    }
    clasp_sources.push(ps);
  }

  // 系列继承规则：所有 clasp 来源条目系列信息一致时自动沿用（与 CLI 导入一致）
  if let Some((name, order)) = inherit_series(&series_pairs) {
    meta.series_name = Some(name);
    meta.series_order = order;
  }

  // 作者/书名的爬取优先回填统一在豆瓣来源爬取完成后进行（见下方"元数据回填"）

  // 阶段二：豆瓣链接集合 = 手动填写（优先）+ clasp 推断，去重
  let mut douban_list: Vec<String> = Vec::new();
  for u in douban_links {
    let u = u.trim();
    if u.is_empty() {
      continue;
    }
    if !u.contains("book.douban.com/subject/") {
      warn!("忽略无效豆瓣链接: {u}");
      continue;
    }
    push_douban(&mut douban_list, u.to_string());
  }
  for u in &meta.douban_urls {
    push_douban(&mut douban_list, u.clone());
  }
  meta.douban_urls = douban_list;

  // 阶段三：逐豆瓣链接预爬（页面元数据 + 简介 + 封面 + 短评；可打断）
  let mut douban_summaries: Vec<String> = Vec::new();
  let mut douban_sources: Vec<PendingSource> = Vec::new();
  let douban_count = meta.douban_urls.len() as i64;
  for (di, url) in meta.douban_urls.iter().enumerate() {
    cancel.check()?;
    progress(TaskProgress {
      phase: "source".into(),
      current: di as i64 + 1,
      total: douban_count,
      message: format!("爬取豆瓣来源 {}/{}：{}", di + 1, douban_count, url),
    });
    let ps = tokio::select! {
      r = crawl_douban_source(&client, url) => r,
      _ = cancel.wait_cancelled() => return Err(IngestionError::Cancelled),
    };
    if let Some(s) = ps.summary.clone() {
      douban_summaries.push(s);
    }
    douban_sources.push(ps);
  }

  // 元数据回填：书名/作者优先采用爬取到的，EPUB 原有值仅作兜底。
  // 此处入参来自确认弹窗前的预填（GUI 在 prepare 之后才允许编辑），
  // 用户编辑值由 commit_import 在最终落盘时覆盖，不受影响。
  let (title, author) = crawled_title_author(
    &meta.title,
    &meta.author,
    &clasp_sources,
    &match_list,
    &douban_sources,
  );
  meta.title = title;
  meta.author = author;

  // 阶段四：简介定稿（clasp 简介 → 豆瓣简介兜底；多条按"确认合并 / 取第一本"）
  let desc_source = if clasp_summaries.is_empty() { &douban_summaries } else { &clasp_summaries };
  let mut llm_usage: Vec<(String, agent::TokenUsage)> = Vec::new();
  if desc_source.len() > 1 && merge_summaries {
    cancel.check()?;
    progress(TaskProgress {
      phase: "fusion".into(),
      current: 0,
      total: 0,
      message: "LLM 合并简介中…".into(),
    });
  }
  let (description, llm_outcome, degraded) =
    pick_import_description(desc_source, merge_summaries, cancel).await?;
  if let Some((model, usage)) = &llm_outcome {
    llm_usage.push((model.clone(), *usage));
  }
  let fusion_error = degraded;

  // 阶段五：封面下载（OSS 伪装 → 豆瓣 og:image → EPUB 内嵌封面兜底；多选合并本取第一本封面）
  let covers_dir = crate::config::AppConfig::load()
    .covers_dir()
    .unwrap_or_else(|_| crate::config::temp_dir());
  let cover_path: Option<PathBuf> = if !cover_urls.is_empty() || !meta.douban_urls.is_empty() {
    cancel.check()?;
    progress(TaskProgress {
      phase: "cover".into(),
      current: 0,
      total: 0,
      message: "下载封面中…".into(),
    });
    let key = meta
      .clasp_ids
      .first()
      .cloned()
      .unwrap_or_else(|| format!("cover-{}", unix_secs()));
    let downloaded = tokio::select! {
      r = download_cover_robust(&client, &cover_urls, &meta.douban_urls, &covers_dir, &key) => r,
      _ = cancel.wait_cancelled() => return Err(IngestionError::Cancelled),
    };
    match downloaded {
      Ok(p) => Some(p),
      Err(e) => {
        warn!("封面下载失败: {e}，尝试 EPUB 内嵌封面");
        save_epub_cover(&epub_path, &covers_dir, &key)
      }
    }
  } else {
    // 未匹配到书籍：默认采用 EPUB 内嵌封面
    save_epub_cover(&epub_path, &covers_dir, &format!("epub-{}", unix_secs()))
  };

  // 预处理到此结束（网络 + 封面文件就绪）；书库复制与 EPUB 写入
  // 延后到用户在确认弹窗中编辑完元数据后，由 finalize_import 执行
  let fusion_model = llm_usage.first().map(|(m, _)| m.clone());
  Ok(PreparedImport {
    file_path: epub_path.to_string_lossy().into_owned(),
    original_path: original_path
      .map(str::to_string)
      .unwrap_or_else(|| epub_path.to_string_lossy().into_owned()),
    title: meta.title,
    author: meta.author,
    tags: meta.tags,
    description,
    series_name: meta.series_name,
    series_order: meta.series_order,
    clasp_ids: meta.clasp_ids,
    douban_urls: meta.douban_urls,
    library_file: String::new(),
    cover_path,
    sources_clasp: clasp_sources,
    sources_douban: douban_sources,
    cover_override: None,
    llm_usage,
    fusion_model,
    fusion_error,
  })
}

/// 导入确认后的落盘阶段：按（可能被用户编辑过的）元数据复制入书库并写入 EPUB 副本
///
/// 就地更新 `library_file` 与 `cover_path`（确认弹窗上传的封面复制入封面缓存后
/// 作为书籍封面），随后可调用 `persist_import` 入库。
pub fn finalize_import(
  p: &mut PreparedImport,
  cover_override: Option<&Path>,
) -> Result<(), IngestionError> {
  let cfg = crate::config::AppConfig::load();
  let lib = cfg.require_library_path().map_err(|e| {
    IngestionError::Other(format!(
      "{e}（写入元数据需要书库，请先运行 library config set <PATH>）"
    ))
  })?;
  let series_tuple = p
    .series_name
    .as_ref()
    .map(|n| (n.as_str(), p.series_order.unwrap_or(1)));
  let epub_path = PathBuf::from(&p.file_path);
  let library_file = crate::library::copy_into_library(
    &lib,
    &epub_path,
    &p.author,
    &p.title,
    series_tuple,
  )
  .map_err(|e| IngestionError::Other(format!("复制入书库失败: {e}")))?;
  let library_abs = lib.join(&library_file);

  // 封面：确认弹窗上传优先（内容去重落盘），其次预爬封面
  let cover = match cover_override {
    Some(src) => {
      let ext = src
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .unwrap_or_else(|| "jpg".into());
      let ext = if ext == "jpeg" { "jpg" } else { ext.as_str() };
      let dir = cfg.covers_dir().unwrap_or_else(|_| crate::config::temp_dir());
      let bytes = std::fs::read(src)
        .map_err(|e| IngestionError::Other(format!("封面读取失败: {e}")))?;
      let dest = save_cover_dedup(&dir, &bytes, ext)
        .map_err(|e| IngestionError::Other(format!("封面复制失败: {e}")))?;
      p.cover_path = Some(dest);
      p.cover_path.as_deref()
    }
    None => p.cover_path.as_deref(),
  };

  write_epub_metadata(
    &library_abs,
    &p.title,
    &p.author,
    &p.tags,
    Some(p.description.as_str()).filter(|s| !s.trim().is_empty()),
    series_tuple,
    cover,
  )
  .map_err(IngestionError::EpubError)?;
  info!("EPUB 元数据写入成功");
  p.library_file = library_file;
  Ok(())
}

/// 导入元数据回填（GUI prepare_import 用）：书名/作者优先采用爬取到的，
/// EPUB 原有值（调用方传入的 current_*）仅作兜底。
///
/// - 书名：clasp 详情 → 搜索条目首条 → 豆瓣首来源；含汉字时繁转简（数据不变量）
/// - 作者：clasp 来源并集（合并本多作者顿号拼接）→ 搜索条目并集 → 豆瓣首来源
/// - 全部来源都爬取失败时保留原值（降级不阻断）
pub fn crawled_title_author(
  current_title: &str,
  current_author: &str,
  clasp_sources: &[PendingSource],
  match_list: &[&SuggestionMatch],
  douban_sources: &[PendingSource],
) -> (String, String) {
  use character_converter::traditional_to_simplified;

  let mut title = current_title.to_string();
  let crawled_title = clasp_sources
    .iter()
    .find_map(|s| s.title.clone().filter(|t| !t.trim().is_empty()))
    .or_else(|| {
      match_list
        .first()
        .map(|m| m.title.trim().to_string())
        .filter(|t| !t.is_empty())
    })
    .or_else(|| {
      douban_sources
        .iter()
        .find_map(|s| s.title.clone().filter(|t| !t.trim().is_empty()))
    });
  if let Some(t) = crawled_title {
    title = if utils::count_han(&t) > 0 {
      traditional_to_simplified(&t).into_owned()
    } else {
      t
    };
  }

  let mut authors: Vec<String> = Vec::new();
  let push_author = |a: &str, out: &mut Vec<String>| {
    let a = a.trim();
    if !a.is_empty() && !out.iter().any(|x| x == a) {
      out.push(a.to_string());
    }
  };
  for s in clasp_sources {
    if let Some(a) = s.author.as_deref() {
      push_author(a, &mut authors);
    }
  }
  if authors.is_empty() {
    for m in match_list {
      push_author(&m.author, &mut authors);
    }
  }
  if authors.is_empty() {
    if let Some(a) = douban_sources
      .iter()
      .find_map(|s| s.author.clone().filter(|a| !a.trim().is_empty()))
    {
      push_author(&a, &mut authors);
    }
  }
  let author = if authors.is_empty() {
    current_author.to_string()
  } else {
    authors.join("、")
  };
  (title, author)
}

/// 系列继承规则（导入时）：若所有 clasp 来源条目的系列名一致，则自动取为书籍系列。
///
/// - 系列名必须全部一致（任一条目缺系列则视为不一致）
/// - 卷号：全部一致（含全部缺失）时沿用该卷号；卷号冲突时取首条目卷号
///   （多来源合并本常为同系列上下册，以第一册卷号为准）
pub fn inherit_series(pairs: &[(String, Option<i64>)]) -> Option<(String, Option<i64>)> {
  let (name, first_order) = pairs.first()?;
  if !pairs.iter().all(|(n, _)| n == name) {
    return None;
  }
  let mut known: Vec<i64> = pairs.iter().filter_map(|(_, o)| *o).collect();
  known.sort_unstable();
  known.dedup();
  let order = match known.as_slice() {
    // 全部缺失 → 无卷号；唯一已知卷号 → 沿用；冲突 → 取首条目卷号
    [] => None,
    [_] => known.first().copied(),
    _ => *first_order,
  };
  Some((name.clone(), order))
}

/// 截断过长的错误信息（LLM API 错误体可能包含整段响应），用于向用户展示
fn brief_error(msg: &str) -> String {
  let s: String = msg.chars().take(300).collect();
  if s.len() < msg.len() {
    format!("{s}…")
  } else {
    s
  }
}

/// 版本标签：出版社 · 丛书 · 年份 · 装帧 · 语种 · 译者（忽略空值与重复）
fn edition_label(e: &spider::ClaspEdition) -> String {
  let mut parts: Vec<String> = Vec::new();
  let mut push = |s: Option<&str>| {
    if let Some(v) = s.map(str::trim).filter(|v| !v.is_empty()) {
      if !parts.iter().any(|p| p == v) {
        parts.push(v.to_string());
      }
    }
  };
  push(e.publisher.as_deref());
  if let Some(i) = &e.imprint {
    // 丛书名与出版社相同时不重复展示
    if i.name.as_deref().map(str::trim) != e.publisher.as_deref().map(str::trim) {
      push(i.name.as_deref());
    }
  }
  if let Some(d) = &e.published_at {
    let year: String = d.chars().take_while(|c| c.is_ascii_digit()).collect();
    if year.len() == 4 {
      push(Some(&year));
    }
  }
  push(e.binding.as_deref());
  push(e.language.as_deref());
  push(e.translator.as_deref());
  parts.join(" · ")
}

/// 非交互式导入第二阶段：同步写入数据库（books + 增强 + 短评 + LLM 用量），返回书籍 ID
pub fn persist_import(conn: &rusqlite::Connection, p: &PreparedImport) -> rusqlite::Result<i64> {
  let clasp_ids_json = serde_json::to_string(&p.clasp_ids).unwrap_or_default();
  let book_id = db::insert_book(
    conn,
    &p.title,
    &p.author,
    &utils::join_tags(&p.tags),
    &p.original_path,
    &clasp_ids_json,
  )?;

  let cover_str = p
    .cover_path
    .as_ref()
    .map(|c| c.to_string_lossy().into_owned());
  db::update_book_enrichment(
    conn,
    book_id,
    Some(p.description.as_str()).filter(|s| !s.trim().is_empty()),
    cover_str.as_deref(),
    p.series_name.as_deref(),
    p.series_order,
    Some(p.library_file.as_str()),
    Some(clasp_ids_json.as_str()),
  )?;
  // 豆瓣链接入库（重抓短评 / 更换封面豆瓣兜底的数据源）
  let douban_json = serde_json::to_string(&p.douban_urls).unwrap_or_default();
  db::set_book_match_urls(conn, book_id, None, &douban_json)?;
  // 书籍封面登记引用（计数归零才允许删除文件）
  if let Some(c) = &p.cover_path {
    db::cover_ref_add(conn, c.to_string_lossy().as_ref())?;
  }

  // 归属当前书库（多书库）
  if let Some(lib_id) = crate::config::AppConfig::load().current_library_id() {
    db::set_book_library(conn, book_id, lib_id)?;
  }
  // 来源项目与短评入库（短评带来源定位；来源封面在 insert_source 内登记引用）
  persist_sources(conn, book_id, &p.sources_clasp, &p.sources_douban)?;

  for (model, usage) in &p.llm_usage {
    db::record_llm_usage(conn, model, usage.prompt_tokens, usage.completion_tokens, usage.total_tokens)?;
  }
  Ok(book_id)
}

// ------------------------------------------------------------------ //
//  辅助
// ------------------------------------------------------------------ //

fn io_abort(e: dialoguer::Error) -> IngestionError {
  IngestionError::Other(e.to_string())
}

#[cfg(test)]
mod tests {
  use super::*;

  /// 端到端验证：write_epub_metadata 将封面/标题/作者/简介/系列写入 EPUB
  #[test]
  fn test_write_epub_metadata_with_cover() {
    let dir = std::env::temp_dir().join(format!("mna-test-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let epub_path = dir.join("test.epub");
    let cover_path = dir.join("downloaded.jpg");
    std::fs::write(&cover_path, b"\xFF\xD8\xFF\xE0 fake jpg bytes").unwrap();

    // 用 rbook builder 构造一本最小 EPUB
    rbook::Epub::builder()
      .identifier("urn:test")
      .title("舊標題")
      .author("舊作者")
      .language("zh")
      .chapter(rbook::epub::EpubChapter::new("ch1").xhtml_body("<p>hi</p>"))
      .write()
      .save(&epub_path)
      .unwrap();

    write_epub_metadata(
      &epub_path,
      "钟表馆事件",
      "绫辻行人",
      &["本格推理".to_string(), "推理小说".to_string()],
      Some("无剧透简介"),
      Some(("馆系列", 3)),
      Some(&cover_path),
    )
    .unwrap();

    // 重新打开验证
    let epub = rbook::Epub::open(epub_path.to_string_lossy().as_ref()).unwrap();
    assert_eq!(epub.metadata().title().unwrap().value(), "钟表馆事件");
    assert_eq!(
      epub.metadata().creators().next().unwrap().value(),
      "绫辻行人"
    );
    let desc = epub.metadata().description().unwrap();
    assert_eq!(desc.value(), "无剧透简介");
    let cover = epub.manifest().cover_image();
    assert!(cover.is_some(), "封面应已嵌入 EPUB");

    std::fs::remove_dir_all(&dir).ok();
  }

  /// 静默匹配：唯一结果直接采用
  #[test]
  fn test_pick_silent_match_unique() {
    let items = vec![ClaspBookSuggestion {
      title: "另一个标题".into(),
      author_name: "某人".into(),
      id: "id1".into(),
      tags: vec![],
      cover_url: None,
      summary: None,
      douban_url: None,
    }];
    let picked = pick_silent_match(&items, "钟表馆事件");
    assert_eq!(picked.as_ref().map(|s| s.id.as_str()), Some("id1"));
  }

  /// 静默匹配：多条结果时只采用与书名精确一致（繁简/大小写归一）的条目
  #[test]
  fn test_pick_silent_match_exact_title() {
    let items = vec![
      ClaspBookSuggestion {
        title: "殺人推理筆記".into(),
        author_name: "甲".into(),
        id: "id-a".into(),
        tags: vec![],
        cover_url: None,
        summary: None,
        douban_url: None,
      },
      ClaspBookSuggestion {
        title: "杀人推理笔记（新版）".into(),
        author_name: "乙".into(),
        id: "id-b".into(),
        tags: vec![],
        cover_url: None,
        summary: None,
        douban_url: None,
      },
    ];
    // 繁体候选标题与第一条归一后一致
    let picked = pick_silent_match(&items, "杀人推理笔记");
    assert_eq!(picked.as_ref().map(|s| s.id.as_str()), Some("id-a"));

    // 完全歧义 → 降级为 None
    assert!(pick_silent_match(&items, "完全无关的书").is_none());
  }

  /// OPF broken href 修复：移除引用不存在文件的 item 及 spine itemref
  #[test]
  fn test_fix_opf_broken_href() {
    let opf = r#"<package>
<manifest>
<item id="nav" href="../nav." media-type="application/xhtml+xml" properties="nav"/>
<item id="ch1" href="chapter1.html" media-type="application/xhtml+xml"/>
<item id="css" href="css/main.css" media-type="text/css"/>
</manifest>
<spine>
<itemref idref="nav"/>
<itemref idref="ch1"/>
</spine>
</package>"#;
    // 有效路径不含 nav.（它不存在于 zip 中）
    let valid: std::collections::HashSet<String> = [
      "OEBPS/chapter1.html".to_string(),
      "OEBPS/css/main.css".to_string(),
    ]
    .into_iter()
    .collect();
    let out = fix_opf(opf, &valid, "OEBPS/");
    assert!(!out.contains("nav."), "broken item 应被移除");
    assert!(!out.contains("nav."), "broken itemref 应被移除");
    assert!(out.contains("chapter1.html"), "正常 item 保留");
    assert!(out.contains("css/main.css"), "正常 css item 保留");
  }

  /// OPF manifest 重复 href 去重（sndjj.epub 场景：css/main.css 被声明两次）'
  #[test]
  fn test_dedupe_opf_manifest() {
    let opf = r#"<manifest>
<item id="main-css" href="css/main.css" media-type="text/css"/>
<item id="coverpage" href="coverpage.html" media-type="application/xhtml+xml"/>
<item id="css" href="css/main.css" media-type="text/css"/>
</manifest>"#;
    let out = fix_opf(opf, &["css/main.css".into(), "coverpage.html".into()].into_iter().collect(), "");
    assert!(out.contains(r#"<item id="main-css""#));
    assert!(out.contains("coverpage.html"));
    assert!(!out.contains(r#"id="css""#));
    assert!(!opf_needs_fix(&out, &["css/main.css".into(), "coverpage.html".into()].into_iter().collect(), ""));
  }

  /// 系列继承规则：全部条目系列信息一致才自动沿用
  #[test]
  fn test_inherit_series() {
    use super::inherit_series;

    // 无来源 → 不继承
    assert_eq!(inherit_series(&[]), None);
    // 单条目：名称 + 已知卷号
    assert_eq!(
      inherit_series(&[("馆系列".into(), Some(5))]),
      Some(("馆系列".into(), Some(5)))
    );
    // 多条目完全一致（合并本上下册同一卷号）
    assert_eq!(
      inherit_series(&[("馆系列".into(), Some(5)), ("馆系列".into(), Some(5))]),
      Some(("馆系列".into(), Some(5)))
    );
    // 名称一致、部分条目缺卷号 → 取唯一已知卷号
    assert_eq!(
      inherit_series(&[("馆系列".into(), None), ("馆系列".into(), Some(5))]),
      Some(("馆系列".into(), Some(5)))
    );
    // 名称一致、卷号全部缺失 → 仅继承系列名
    assert_eq!(
      inherit_series(&[("馆系列".into(), None), ("馆系列".into(), None)]),
      Some(("馆系列".into(), None))
    );
    // 卷号冲突（同系列上下册合并本）→ 系列名沿用，卷号取首条目（第一册）
    assert_eq!(
      inherit_series(&[("馆系列".into(), Some(1)), ("馆系列".into(), Some(5))]),
      Some(("馆系列".into(), Some(1)))
    );
    // 系列名不一致 → 不继承
    assert_eq!(
      inherit_series(&[("馆系列".into(), Some(5)), ("伽利略系列".into(), Some(5))]),
      None
    );
  }

  /// 爬取优先回填：书名/作者优先采用爬取到的，EPUB 原值仅兜底
  #[test]
  fn test_crawled_title_author() {
    let clasp = |title: Option<&str>, author: Option<&str>| PendingSource {
      ref_key: String::new(),
      title: title.map(str::to_string),
      author: author.map(str::to_string),
      ..Default::default()
    };
    let m = |title: &str, author: &str| SuggestionMatch {
      id: String::new(),
      title: title.to_string(),
      author: author.to_string(),
      tags: vec![],
      cover_url: None,
      summary: None,
      douban_url: None,
    };

    // 单条 clasp 来源：覆盖 EPUB 原值；繁体书名自动转简体（作者不转换，与 CLI 一致）
    let (t, a) = crawled_title_author(
      "EPUB旧标题",
      "EPUB旧作者",
      &[clasp(Some("鐘錶館事件"), Some("綾辻行人"))],
      &[],
      &[],
    );
    assert_eq!(t, "钟表馆事件");
    assert_eq!(a, "綾辻行人");

    // 合并本：作者取 clasp 来源并集（顿号拼接、去重）
    let (t, a) = crawled_title_author(
      "",
      "",
      &[clasp(Some("馆系列合集"), Some("绫辻行人")), clasp(None, Some("绫辻行人"))],
      &[],
      &[],
    );
    assert_eq!(t, "馆系列合集");
    assert_eq!(a, "绫辻行人");

    // clasp 详情全失败 → 搜索条目兜底（标题 + 作者并集）
    let (t, a) = crawled_title_author(
      "EPUB标题",
      "EPUB作者",
      &[clasp(None, None)],
      &[&m("搜索标题", "作者甲"), &m("搜索标题2", "作者乙")],
      &[],
    );
    assert_eq!(t, "搜索标题");
    assert_eq!(a, "作者甲、作者乙");

    // 无 clasp 匹配（跳过搜索走豆瓣）：豆瓣首来源覆盖
    let (t, a) = crawled_title_author(
      "EPUB标题",
      "",
      &[],
      &[],
      &[clasp(Some("豆瓣标题"), Some("豆瓣作者"))],
    );
    assert_eq!(t, "豆瓣标题");
    assert_eq!(a, "豆瓣作者");

    // 全部来源为空 → 保留 EPUB 原值（降级不阻断）
    let (t, a) = crawled_title_author("EPUB标题", "EPUB作者", &[], &[], &[]);
    assert_eq!(t, "EPUB标题");
    assert_eq!(a, "EPUB作者");

    // 非中文爬取书名原样保留（由前端确认）
    let (t, _) = crawled_title_author("EPUB标题", "", &[clasp(Some("Murder on the Links"), None)], &[], &[]);
    assert_eq!(t, "Murder on the Links");
  }

  /// persist_import：books + 增强元数据 + 来源项目 + 短评 + LLM 用量一次性写入内存库
  #[test]
  fn test_persist_import() {
    let conn = crate::db::open_db(":memory:").unwrap();

    let prepared = PreparedImport {
      file_path: "E:\\books\\原始.epub".into(),
      original_path: "E:\\books\\原始.epub".into(),
      title: "钟表馆事件".into(),
      author: "绫辻行人".into(),
      tags: vec!["本格推理".into(), "推理小说".into()],
      description: "无剧透简介".into(),
      series_name: Some("馆系列".into()),
      series_order: Some(3),
      clasp_ids: vec!["clasp-1".into()],
      douban_urls: vec![],
      library_file: "[guan-xi-lie-3] ling-shi-xing-ren-zhong-biao-guan-shi-jian.epub".into(),
      cover_path: None,
      cover_override: None,
      sources_clasp: vec![PendingSource {
        ref_key: "clasp-1".into(),
        title: Some("钟表馆事件".into()),
        author: Some("绫辻行人".into()),
        summary: Some("无剧透简介".into()),
        comments: vec![spider::Comment {
          rating: Some(4),
          content: "这是一条足够长的短评，用于通过质量过滤的检验标准。".into(),
          usefulness: 12,
        }],
        ..Default::default()
      }],
      sources_douban: vec![],
      llm_usage: vec![(
        "gpt-test".into(),
        agent::TokenUsage { prompt_tokens: 100, completion_tokens: 50, total_tokens: 150 },
      )],
      fusion_model: Some("gpt-test".into()),
      fusion_error: None,
    };

    let id = persist_import(&conn, &prepared).unwrap();
    let detail = crate::db::get_book_detail(&conn, id).unwrap().unwrap();
    assert_eq!(detail.title, "钟表馆事件");
    assert_eq!(detail.author, "绫辻行人");
    assert_eq!(detail.tags, "本格推理, 推理小说");
    assert_eq!(detail.description.as_deref(), Some("无剧透简介"));
    assert_eq!(detail.series_name.as_deref(), Some("馆系列"));
    assert_eq!(detail.series_order, Some(3));
    assert_eq!(detail.clasp_ids.as_deref(), Some("[\"clasp-1\"]"));

    // 短评来自 clasp 来源并带来源定位
    let comments = crate::db::get_comments_for_book(&conn, id).unwrap();
    assert_eq!(comments.len(), 1);
    assert_eq!(comments[0].source, "claspclub");
    assert_eq!(comments[0].source_ref.as_deref(), Some("clasp-1"));

    // 来源项目已入库（clasp 在前，position 有序）
    let sources = crate::db::list_sources(&conn, id).unwrap();
    assert_eq!(sources.len(), 1);
    assert_eq!(sources[0].kind, "clasp");
    assert_eq!(sources[0].position, 0);
    assert_eq!(sources[0].summary.as_deref(), Some("无剧透简介"));

    // LLM 用量已入库
    let usage = crate::db::get_llm_usage(&conn).unwrap();
    assert_eq!(usage.len(), 1);
    assert_eq!(usage[0].model, "gpt-test");
    assert_eq!(usage[0].calls, 1);
    assert_eq!(usage[0].total_tokens, 150);
  }

  /// EPUB 内嵌封面提取与落盘：带封面 → 字节 + 扩展名；无封面 → None
  #[test]
  fn test_epub_cover_fallback() {
    let dir = std::env::temp_dir().join(format!("mna-cov-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let with_cover = dir.join("with-cover.epub");
    let without = dir.join("no-cover.epub");
    rbook::Epub::builder()
      .identifier("urn:cov1")
      .title("书")
      .author("人")
      .language("zh")
      .cover_image(("cover.png", b"fake png bytes".to_vec()))
      .write()
      .save(&with_cover)
      .unwrap();
    rbook::Epub::builder()
      .identifier("urn:cov2")
      .title("书")
      .author("人")
      .language("zh")
      .chapter(rbook::epub::EpubChapter::new("c1").xhtml_body("<p>x</p>"))
      .write()
      .save(&without)
      .unwrap();

    // 有内嵌封面：字节与扩展名正确
    let (bytes, ext) = extract_epub_cover_bytes(&with_cover).unwrap();
    assert_eq!(bytes, b"fake png bytes");
    assert_eq!(ext, "png");

    // 落盘：内容去重（同内容复用同一文件）
    let saved = save_epub_cover(&with_cover, &dir, "k1").unwrap();
    assert_eq!(saved.extension().unwrap().to_string_lossy(), "png");
    assert_eq!(std::fs::read(&saved).unwrap(), b"fake png bytes");
    let saved2 = save_epub_cover(&with_cover, &dir, "k2").unwrap();
    assert_eq!(saved, saved2, "相同内容应复用同一文件");

    // 无内嵌封面：提取与落盘均为 None（降级不报错）
    assert!(extract_epub_cover_bytes(&without).is_none());
    assert!(save_epub_cover(&without, &dir, "k2").is_none());

    std::fs::remove_dir_all(&dir).ok();
  }

  /// 版本标签组装（来源预爬 editions JSON 用）：出版社·丛书·年份·装帧·译者
  #[test]
  fn test_edition_label() {
    let e: spider::ClaspEdition = serde_json::from_str(
      r#"{
        "isPrimary": false,
        "coverUrl": "https://example.com/douban-222.jpg",
        "publisher": "人民文学出版社",
        "imprint": { "name": "午夜文库", "publisherName": "新星出版社" },
        "publishedAt": "2013-04-01T00:00:00.000Z",
        "binding": "平装",
        "translator": "郑桥"
      }"#,
    )
    .unwrap();
    assert_eq!(edition_label(&e), "人民文学出版社 · 午夜文库 · 2013 · 平装 · 郑桥");

    // 丛书名与出版社相同 → 不重复；空字段全部忽略
    let e2: spider::ClaspEdition = serde_json::from_str(
      r#"{ "publisher": "新星出版社", "imprint": { "name": "新星出版社" } }"#,
    )
    .unwrap();
    assert_eq!(edition_label(&e2), "新星出版社");
  }

}
