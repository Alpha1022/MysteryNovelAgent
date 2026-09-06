use std::path::{Path, PathBuf};

use dialoguer::{Confirm, Input};
use rusqlite::Connection;
use tracing::{info, warn};

use crate::agent;
use crate::config::AppConfig;
use crate::db;
use crate::ingestion;
use crate::library;

/// merge 命令入口：`merge --series <名>` 或 `merge --ids 1,2,3`
pub async fn run_merge(
  conn: &Connection,
  series: Option<String>,
  ids: Option<String>,
) -> anyhow::Result<()> {
  // 1. 解析待合并书籍
  let books: Vec<db::SeriesBook> = if let Some(name) = &series {
    let mut b = db::get_books_by_series(conn, name)?;
    b.sort_by_key(|b| b.series_order.unwrap_or(i64::MAX));
    b
  } else if let Some(ids_str) = &ids {
    let ids = parse_ids(ids_str)?;
    let mut out = Vec::new();
    for id in ids {
      let b = db::get_book_full(conn, id)?
        .ok_or_else(|| anyhow::anyhow!("书籍不存在: id={id}"))?;
      out.push(b);
    }
    out
  } else {
    anyhow::bail!("需要指定 --series <名称> 或 --ids <1,2,3>");
  };

  if books.len() < 2 {
    anyhow::bail!("合并至少需要两本书。");
  }

  // 2. 展示并确认
  println!("\n即将合并以下书籍：");
  for b in &books {
    let vol = b
      .series_order
      .map(|o| format!(" #{o}"))
      .unwrap_or_default();
    println!("  [{}] 《{}》{vol}", b.id, b.title);
  }
  if !Confirm::new()
    .with_prompt("确认合并？")
    .default(true)
    .interact()?
  {
    println!("已取消。");
    return Ok(());
  }

  // 3. 解析 EPUB 文件路径（书库文件优先，回退原始路径）
  let cfg = AppConfig::load();
  let lib = cfg.require_library_path()?;
  let mut sources: Vec<PathBuf> = Vec::new();
  for b in &books {
    let p = if !b.library_file.is_empty() {
      lib.join(&b.library_file)
    } else {
      PathBuf::from(&b.file_path)
    };
    if !p.exists() {
      anyhow::bail!("找不到 EPUB: {}（书籍《{}》）", p.display(), b.title);
    }
    sources.push(p);
  }

  // 4. 收集简介（DB description 优先，回退 EPUB 内 dc:description）
  let mut descriptions: Vec<String> = Vec::new();
  for (b, src) in books.iter().zip(&sources) {
    let d = b
      .description
      .clone()
      .filter(|s| !s.trim().is_empty())
      .or_else(|| epub_description(src));
    if let Some(d) = d {
      descriptions.push(d);
    }
  }

    println!("\n正在融合简介...");
    let combined_desc = if descriptions.is_empty() {
      String::new()
    } else {
      match agent::combine_descriptions(&descriptions).await {
        Ok(fusion) => {
          if let Some((model, usage)) = &fusion.llm {
            let _ = db::record_llm_usage(
              conn,
              model,
              usage.prompt_tokens,
              usage.completion_tokens,
              usage.total_tokens,
            );
          }
          if let Some(reason) = &fusion.degraded {
            println!("  ⚠ 简介未融合（已拼接）: {reason}");
          }
          fusion.text
        }
        Err(e) => {
          warn!("LLM 简介融合失败: {e}，降级为拼接");
          println!("  ⚠ 简介融合失败（已降级拼接）: {e}");
          descriptions.join("\n\n")
        }
      }
    };

  // 5. 合并元数据（标题/作者默认取第一本，用户可改）
  let (first_title, first_author, first_tags) = db::get_book_meta(conn, books[0].id)?;
  let default_title = series
    .clone()
    .unwrap_or_else(|| format!("{first_title} 合集"));

  let title: String = Input::new()
    .with_prompt("合并后书名")
    .allow_empty(true)
    .default(default_title)
    .interact_text()?;
  let title = if title.trim().is_empty() {
    series.clone().unwrap_or_else(|| format!("{first_title} 合集"))
  } else {
    title
  };

  let author: String = Input::new()
    .with_prompt("作者")
    .allow_empty(true)
    .default(first_author.clone())
    .interact_text()?;
  let author = if author.trim().is_empty() { first_author } else { author };

  // 6. 执行 EPUB 合并
  println!("\n正在合并 EPUB（章节 + 资源）...");
  let book_titles: Vec<String> = books.iter().map(|b| b.title.clone()).collect();
  let merged_bytes = merge_epubs(&sources, &book_titles, &title, &author)?;

  // 7. 存入书库
  let merged_name = library::to_ascii_filename(&author, &format!("{title} 合集"), None);
  let dest = library::resolve_collision(&lib, &merged_name);
  std::fs::write(&dest, &merged_bytes)?;
  let library_file = dest
    .file_name()
    .map(|s| s.to_string_lossy().into_owned())
    .unwrap_or(merged_name);
  info!("合并书已写入: {}", dest.display());

  // 8. 入库
  let merged_from = serde_json::to_string(&books.iter().map(|b| b.id).collect::<Vec<_>>())
    .unwrap_or_default();
  let book_id = db::insert_book(conn, &title, &author, &first_tags, &dest.to_string_lossy(), "[]")?;
  db::update_book_enrichment(
    conn,
    book_id,
    Some(combined_desc.as_str()).filter(|s| !s.trim().is_empty()),
    None,
    None,
    None,
    Some(&library_file),
    None,
  )?;
  // merged_from 单独更新（update_book_enrichment 不含该字段，用原生 SQL）
  conn.execute(
    "UPDATE books SET merged_from = ?1 WHERE id = ?2",
    rusqlite::params![merged_from, book_id],
  )?;

  println!("\n========== 合并完成 ==========");
  println!("  书名: {title}");
  println!("  作者: {author}");
  println!("  包含: {} 本", books.len());
  if !combined_desc.is_empty() {
    let preview: String = combined_desc.chars().take(60).collect();
    println!("  简介: {preview}...");
  }
  println!("  书库: {library_file}");
  println!("  数据库 ID: {book_id}");
  println!("==============================\n");
  Ok(())
}

fn parse_ids(s: &str) -> anyhow::Result<Vec<i64>> {
  s.split(',')
    .map(|p| p.trim().parse::<i64>().map_err(|_| anyhow::anyhow!("无效的书籍 ID: {p}")))
    .collect()
}

/// GUI 多选合并：合并计划（纯读取，短暂持锁执行）
pub struct MergePlan {
  /// 待合并书籍（按给定顺序）
  pub books: Vec<db::SeriesBook>,
  /// 各书 EPUB 路径（书库副本优先）
  pub sources: Vec<PathBuf>,
  /// 合并后书名（第一本书名 + " 合集"）
  pub title: String,
  /// 作者并集去重
  pub author: String,
  /// 标签并集去重（含"推理小说"不变量）
  pub tags: String,
  /// clasp 条目并集
  pub clasp_union: Vec<String>,
  /// 各书简介（LLM 合并候选）
  pub descriptions: Vec<String>,
}

/// 生成合并计划：读取原书、解析 EPUB 路径、汇总元数据
pub fn plan_merge(conn: &Connection, ids: &[i64]) -> anyhow::Result<MergePlan> {
  if ids.len() < 2 {
    anyhow::bail!("合并至少需要两本书。");
  }
  let mut books: Vec<db::SeriesBook> = Vec::new();
  for id in ids {
    let b = db::get_book_full(conn, *id)?
      .ok_or_else(|| anyhow::anyhow!("书籍不存在: id={id}"))?;
    books.push(b);
  }
  let cfg = AppConfig::load();
  let lib = cfg.require_library_path()?;
  let mut sources: Vec<PathBuf> = Vec::new();
  for b in &books {
    let p = if !b.library_file.is_empty() {
      lib.join(&b.library_file)
    } else {
      PathBuf::from(&b.file_path)
    };
    if !p.exists() {
      anyhow::bail!("找不到 EPUB: {}（书籍《{}》）", p.display(), b.title);
    }
    sources.push(p);
  }

  // 元数据：作者/标签并集去重；书名取第一本
  let mut authors: Vec<String> = Vec::new();
  let mut tags_union: Vec<String> = Vec::new();
  for b in &books {
    let (_, a, t) = db::get_book_meta(conn, b.id)?;
    for one in a.split(['、', ',', '，']) {
      let one = one.trim();
      if !one.is_empty() && !authors.iter().any(|x| x == one) {
        authors.push(one.to_string());
      }
    }
    for one in crate::utils::split_tags(&t) {
      if !tags_union.iter().any(|x| x == &one) {
        tags_union.push(one);
      }
    }
  }
  // 确保包含当前书库的固定标签（默认"推理小说"）
  for t in AppConfig::load().default_tags() {
    if !tags_union.iter().any(|x| x == &t) {
      tags_union.push(t);
    }
  }
  let (first_title, _, _) = db::get_book_meta(conn, books[0].id)?;

  // 简介候选（DB 优先，回退 EPUB 内嵌）
  let mut descriptions: Vec<String> = Vec::new();
  for (b, src) in books.iter().zip(&sources) {
    let d = b
      .description
      .clone()
      .filter(|s| !s.trim().is_empty())
      .or_else(|| epub_description(src));
    if let Some(d) = d {
      descriptions.push(d);
    }
  }

  // clasp 条目并集
  let mut clasp_union: Vec<String> = Vec::new();
  for b in &books {
    if let Ok(Some(d)) = db::get_book_detail(conn, b.id) {
      if let Some(json) = d.clasp_ids {
        if let Ok(arr) = serde_json::from_str::<Vec<String>>(&json) {
          for id in arr {
            if !clasp_union.contains(&id) {
              clasp_union.push(id);
            }
          }
        }
      }
    }
  }

  Ok(MergePlan {
    books,
    sources,
    title: format!("{first_title} 合集"),
    author: authors.join("、"),
    tags: crate::utils::join_tags(&tags_union),
    clasp_union,
    descriptions,
  })
}

/// 执行合并计划：EPUB 合并写入书库 → 入库 → 来源拼接 → 短评合并 → 删除原书
///
/// `description`：LLM 预合并的简介；None 时取第一本书的简介。
pub fn apply_merge(
  conn: &Connection,
  plan: MergePlan,
  description: Option<String>,
) -> anyhow::Result<(i64, String)> {
  let cfg = AppConfig::load();
  let lib = cfg.require_library_path()?;
  let MergePlan { books, sources, title, author, tags, clasp_union, descriptions } = plan;

  // 简介：优先 LLM 预合并结果，否则第一本
  let combined_desc = description
    .or_else(|| descriptions.first().cloned())
    .unwrap_or_default();

  // 合并 EPUB 并写入书库
  info!("正在合并 EPUB（章节 + 资源）...");
  let book_titles: Vec<String> = books.iter().map(|b| b.title.clone()).collect();
  let merged_bytes = merge_epubs(&sources, &book_titles, &title, &author)?;
  let merged_name = library::to_ascii_filename(&author, &title, None);
  let dest = library::resolve_collision(&lib, &merged_name);
  std::fs::write(&dest, &merged_bytes)?;
  let library_file = dest
    .file_name()
    .map(|s| s.to_string_lossy().into_owned())
    .unwrap_or(merged_name);

  // 封面：复制第一本书的封面缓存文件（原书删除后仍可用）
  let cover_dest = cfg
    .covers_dir()
    .ok()
    .and_then(|dir| {
      let first_cover = db::get_book_detail(conn, books[0].id)
        .ok()
        .flatten()
        .and_then(|d| d.cover_path)
        .and_then(|p| {
          let p = PathBuf::from(&p);
          p.is_file().then_some(p)
        })?;
      let ts = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
      let ext = first_cover
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("jpg")
        .to_lowercase();
      let dest = dir.join(format!("merged-{ts}.{ext}"));
      std::fs::copy(&first_cover, &dest).ok().map(|_| dest)
    });

  // 入库
  let clasp_json = serde_json::to_string(&clasp_union).unwrap_or_default();
  let merged_from = serde_json::to_string(&books.iter().map(|b| b.id).collect::<Vec<_>>())
    .unwrap_or_default();
  let book_id = db::insert_book(
    conn,
    &title,
    &author,
    &tags,
    &dest.to_string_lossy(),
    &clasp_json,
  )?;
  db::update_book_enrichment(
    conn,
    book_id,
    Some(combined_desc.as_str()).filter(|s| !s.trim().is_empty()),
    cover_dest.as_ref().map(|p| p.to_string_lossy().into_owned()).as_deref(),
    None,
    None,
    Some(&library_file),
    Some(clasp_json.as_str()),
  )?;
  if let Some(c) = &cover_dest {
    db::cover_ref_add(conn, c.to_string_lossy().as_ref())?;
  }
  conn.execute(
    "UPDATE books SET merged_from = ?1 WHERE id = ?2",
    rusqlite::params![merged_from, book_id],
  )?;
  // 归属当前书库（多书库）
  if let Some(lib_id) = AppConfig::load().current_library_id() {
    db::set_book_library(conn, book_id, lib_id)?;
  }

  // 来源项目按书序拼接（clasp 在前、豆瓣在后）
  let mut clasp_pending: Vec<ingestion::PendingSource> = Vec::new();
  let mut douban_pending: Vec<ingestion::PendingSource> = Vec::new();
  for b in &books {
    for s in db::list_sources(conn, b.id)? {
      let pending = source_row_to_pending(s);
      match pending.ref_key.contains("book.douban.com") {
        true => douban_pending.push(pending),
        false => clasp_pending.push(pending),
      }
    }
  }
  ingestion::persist_sources(conn, book_id, &clasp_pending, &douban_pending)?;

  // 短评合并（含个人/AI 书评）
  for b in &books {
    for c in db::get_comments_for_book(conn, b.id)? {
      db::insert_comment(
        conn,
        book_id,
        c.rating,
        &c.content,
        c.usefulness,
        &c.source,
        c.source_ref.as_deref(),
      )?;
    }
  }

  // 删除原书（书库 EPUB 副本删除；封面按引用计数清理）
  for b in &books {
    if let Ok(Some(d)) = db::get_book_detail(conn, b.id) {
      if let (Some(lib_path), Some(f)) = (cfg.library_path.as_ref(), d.library_file.as_deref().filter(|f| !f.is_empty())) {
        let p = lib_path.join(f);
        if p.is_file() {
          let _ = std::fs::remove_file(&p);
        }
      }
    }
    match db::delete_book(conn, b.id) {
      Ok(orphaned) => {
        // 引用归零的封面文件删除（仅限 covers 目录内）
        if let Ok(dir) = cfg.covers_dir() {
          for p in orphaned {
            let cp = Path::new(&p);
            if cp.starts_with(&dir) && cp.is_file() {
              let _ = std::fs::remove_file(cp);
            }
          }
        }
      }
      Err(e) => warn!("原书 [{}] 记录删除失败: {e}", b.id),
    }
    info!("原书 [{}]《{}》已移除", b.id, b.title);
  }

  Ok((book_id, title))
}

/// 来源行 → 待入库来源（短评另行合并拷贝；系列信息一并保留）
fn source_row_to_pending(s: db::SourceRow) -> ingestion::PendingSource {
  ingestion::PendingSource {
    ref_key: s.ref_key,
    title: s.title,
    author: s.author,
    cover_url: s.cover_url,
    cover_path: s.cover_path.map(PathBuf::from),
    summary: s.summary,
    tags: s
      .tags
      .as_deref()
      .and_then(|j| serde_json::from_str::<Vec<String>>(j).ok()),
    series: s.series_name.map(|n| (n, s.series_order)),
    editions_json: s.editions,
    ..Default::default()
  }
}

/// 从 EPUB 元数据读取 dc:description
fn epub_description(path: &Path) -> Option<String> {
  let epub = rbook::Epub::open(path.to_string_lossy().as_ref()).ok()?;
  let d = epub.metadata().description()?;
  let s = d.value().trim().to_string();
  if s.is_empty() { None } else { Some(s) }
}

/// 从 XHTML 内容中提取 <title> 作为章节标题
fn extract_html_title(xhtml: &str) -> Option<String> {
  crate::utils::html_title(xhtml)
}

/// 执行 EPUB 合并：逐本读取资源（图片/CSS 加前缀防冲突）与章节（自动重写引用路径）
fn merge_epubs(
  sources: &[PathBuf],
  book_titles: &[String],
  title: &str,
  author: &str,
) -> anyhow::Result<Vec<u8>> {
  use rbook::epub::rewrite::{EpubRewriteOptions, PathRewrite};
  use rbook::epub::EpubChapter;

  let mut builder = rbook::Epub::builder()
    .identifier(format!(
      "urn:mystery-agent:merged:{}",
      std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
    ))
    .title(title)
    .author(author)
    .language("zh");

  // 封面取第一本
  if let Some(first) = sources.first() {
    if let Ok(epub) = rbook::Epub::open(first.to_string_lossy().as_ref()) {
      if let Some(entry) = epub.manifest().cover_image() {
        match entry.read_bytes() {
          Ok(bytes) => {
            let sub = entry.kind().subtype().to_string();
            let ext = if sub == "jpeg" { "jpg".to_string() } else { sub };
            builder = builder.cover_image((format!("cover.{ext}"), bytes));
          }
          Err(e) => warn!("读取封面失败: {e}"),
        }
      }
    }
  }

  for (i, (path, btitle)) in sources.iter().zip(book_titles).enumerate() {
    let epub = rbook::Epub::open(path.to_string_lossy().as_ref())
      .map_err(|e| anyhow::anyhow!("打开 {} 失败: {e}", path.display()))?;
    let prefix = format!("b{i}/");

    // 资源：图片 + 样式，href 加前缀避免多本书冲突
    for img in epub.manifest().images() {
      let href = format!("{prefix}{}", img.href().as_str().trim_start_matches('/'));
      match img.read_bytes() {
        Ok(bytes) => builder = builder.resource((href, bytes)),
        Err(e) => warn!("跳过资源 {href}: {e}"),
      }
    }
    for st in epub.manifest().styles() {
      let href = format!("{prefix}{}", st.href().as_str().trim_start_matches('/'));
      match st.read_bytes() {
        Ok(bytes) => builder = builder.resource((href, bytes)),
        Err(e) => warn!("跳过样式 {href}: {e}"),
      }
    }

    // 章节：读取时重写资源引用路径（b{i}/ 前缀），标题取 <title> 标签
    let rewrite = EpubRewriteOptions::default().rewrite_paths(PathRewrite::prefix(prefix));
    let mut reader = epub.reader_builder().rewrite(rewrite).create();

    let mut children = Vec::new();
    let mut n = 0usize;
    while let Some(Ok(data)) = reader.read_next() {
      n += 1;
      let content = data.content().to_string();
      let ch_title = extract_html_title(&content).unwrap_or_else(|| format!("{n:02}"));
      children.push(EpubChapter::new(ch_title).xhtml(content));
    }

    if children.is_empty() {
      warn!("《{btitle}》没有可读内容，已跳过");
      continue;
    }

    // 每本原书作为一"卷"，其内容文件为子章节
    builder = builder.chapter(EpubChapter::new(btitle.clone()).children(children));
    info!("已并入《{btitle}》（{n} 个内容文件）");
  }

  builder
    .write()
    .compression(9)
    .to_vec()
    .map_err(|e| anyhow::anyhow!("生成合并 EPUB 失败: {e}"))
}
