use std::path::{Path, PathBuf};

use rusqlite::Connection;
use tracing::info;

use crate::config::AppConfig;
use crate::db;

/// 书架页输出文件名（放在书库根目录，封面以相对路径引用）
const SHELF_FILE: &str = "shelf.html";

/// 渲染 HTML 书架到书库目录，返回输出路径
pub fn render(conn: &Connection) -> anyhow::Result<PathBuf> {
  let cfg = AppConfig::load();
  let lib = cfg.require_library_path()?;
  let covers_dir = cfg.covers_dir()?;
  render_to(conn, &lib, &covers_dir)
}

/// 渲染到指定书库目录（供测试与手动调用）
pub fn render_to(conn: &Connection, lib: &Path, covers_dir: &Path) -> anyhow::Result<PathBuf> {
  let rows = db::get_shelf_books(conn)?;

  let mut cards = String::new();
  for (i, b) in rows.iter().enumerate() {
    cards.push_str(&render_card(b, i, covers_dir));
  }

  let html = build_page(&cards, rows.len());
  let out = lib.join(SHELF_FILE);
  std::fs::write(&out, html)?;
  info!("书架已生成: {}", out.display());
  Ok(out)
}

/// 渲染单张书籍卡片
fn render_card(b: &db::ShelfRow, idx: usize, covers_dir: &Path) -> String {
  // 封面：DB 中存的是绝对路径，页面用相对路径 `covers/{文件名}` 引用
  let cover_html = cover_rel(b.cover_path.as_deref(), covers_dir)
    .map(|src| format!(r#"<img class="cover" src="{src}" alt="cover" loading="lazy">"#))
    .unwrap_or_else(|| {
      format!(
        r#"<div class="cover placeholder"><span>{}</span></div>"#,
        escape(&b.title)
      )
    });

  let badge = match b.status.as_str() {
    "已读" => r#"<span class="badge read">已读</span>"#,
    "在读" => r#"<span class="badge reading">在读</span>"#,
    _ => r#"<span class="badge wish">想读</span>"#,
  };

  // 已读书籍悬浮展示个人短评
  let tip = if b.status == "已读" && !b.my_review.trim().is_empty() {
    format!(r#"<div class="tip"><b>我的短评</b>{}</div>"#, escape(&b.my_review))
  } else {
    String::new()
  };

  let series_label = if b.series_name.is_empty() {
    String::new()
  } else {
    match b.series_order {
      Some(o) => format!(r#"<div class="series">{} #{}</div>"#, escape(&b.series_name), o),
      None => format!(r#"<div class="series">{}</div>"#, escape(&b.series_name)),
    }
  };

  // claspclub 详情页链接
  let clasp_url = serde_json::from_str::<Vec<String>>(&b.clasp_ids)
    .ok()
    .and_then(|v| v.first().cloned())
    .filter(|id| !id.is_empty())
    .map(|id| format!("https://claspclub.com/books/{id}"));

  let inner = format!(
    r#"{badge}{cover_html}<div class="meta"><div class="t">{}</div><div class="a">{}</div>{series_label}</div>{tip}"#,
    escape(&b.title),
    escape(&b.author)
  );

  let delay = idx * 45;
  let card = match &clasp_url {
    Some(u) => format!(
      r#"<a class="card" href="{u}" target="_blank" rel="noopener" style="animation-delay:{delay}ms">{inner}</a>"#
    ),
    None => format!(r#"<div class="card" style="animation-delay:{delay}ms">{inner}</div>"#),
  };

  card
}

/// 将 DB 中的封面绝对路径转换为页面相对路径（仅当文件确实存在于 covers 目录）
fn cover_rel(cover_path: Option<&str>, covers_dir: &Path) -> Option<String> {
  let p = cover_path?;
  let name = Path::new(p).file_name()?.to_string_lossy().into_owned();
  if covers_dir.join(&name).exists() {
    Some(format!("covers/{name}"))
  } else {
    None
  }
}

/// HTML 转义
fn escape(s: &str) -> String {
  s.replace('&', "&amp;")
    .replace('<', "&lt;")
    .replace('>', "&gt;")
    .replace('"', "&quot;")
}

/// 页面模板（占位符替换，避免 format! 大量花括号转义）
fn build_page(cards: &str, count: usize) -> String {
  const TEMPLATE: &str = include_str!("shelf_template.html");
  TEMPLATE
    .replace("__CARDS__", cards)
    .replace("__COUNT__", &count.to_string())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_render_shelf() {
    let dir = std::env::temp_dir().join(format!("mna-shelf-{}", std::process::id()));
    let lib = dir.join("lib");
    let covers = lib.join("covers");
    std::fs::create_dir_all(&covers).unwrap();

    let conn = db::open_db(dir.join("test.db").to_str().unwrap()).unwrap();

    // 已读书：有封面、有短评、有 clasp 链接、有系列
    let id1 = db::insert_book(
      &conn,
      "钟表馆事件",
      "绫辻行人",
      "本格推理,推理小说",
      "/orig/a.epub",
      r#"["clasp123"]"#,
    )
    .unwrap();
    db::update_book_enrichment(
      &conn,
      id1,
      Some("简介"),
      Some(covers.join("clasp123.jpg").to_str().unwrap()),
      Some("馆系列"),
      Some(3),
      Some("a.epub"),
      None,
    )
    .unwrap();
    conn
      .execute(
        "UPDATE books SET status='已读', my_review='诡计精妙，结局震撼' WHERE id=?1",
        rusqlite::params![id1],
      )
      .unwrap();

    // 想读书：无封面无链接
    db::insert_book(&conn, "占星术杀人魔法", "岛田庄司", "推理小说", "/orig/b.epub", "[]")
      .unwrap();

    // 封面文件真实存在才会被引用
    std::fs::write(covers.join("clasp123.jpg"), b"fake").unwrap();

    let out = render_to(&conn, &lib, &covers).unwrap();
    assert!(out.ends_with("shelf.html"));

    let html = std::fs::read_to_string(&out).unwrap();
    assert!(html.contains("钟表馆事件"));
    assert!(html.contains("占星术杀人魔法"));
    assert!(html.contains("badge read"));
    assert!(html.contains("badge wish"));
    assert!(html.contains("我的短评"));
    assert!(html.contains("诡计精妙，结局震撼"));
    assert!(html.contains("https://claspclub.com/books/clasp123"));
    assert!(html.contains(r#"src="covers/clasp123.jpg""#));
    assert!(html.contains("馆系列 #3"));
    assert!(html.contains("2"));

    std::fs::remove_dir_all(&dir).ok();
  }
}
