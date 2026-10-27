//! 书虫的只读 EPUB 正文工具；定位按 spine 节号与纯文本段号（均从 1 开始）。

use std::path::Path;
use anyhow::Context;
use scraper::{Html, Node};
use serde_json::{Value, json};

/// 一节正文；超长段落拆成不超过 1200 字的定位单元。
pub struct TextChapter {
  pub title: String,
  pub paragraphs: Vec<String>,
}

/// 不加载图片的纯文本 EPUB。
pub struct BookText {
  pub chapters: Vec<TextChapter>,
}

fn paragraphs(html: &str) -> Vec<String> {
  let doc = Html::parse_document(html);
  let mut text = String::new();
  collect_text(doc.root_element(), &mut text);
  text.lines().flat_map(|line| {
    let line = line.split_whitespace().collect::<Vec<_>>().join(" ");
    let chars: Vec<char> = line.chars().collect();
    chars.chunks(1200).map(|c| c.iter().collect::<String>()).collect::<Vec<_>>()
  }).filter(|s| !s.is_empty()).collect()
}

fn collect_text(element: scraper::ElementRef<'_>, out: &mut String) {
  let name = element.value().name();
  if matches!(name, "head" | "script" | "style" | "iframe" | "noscript") {
    return;
  }
  let block = matches!(name, "p" | "div" | "section" | "article" | "li" | "blockquote" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "br" | "tr");
  if block { out.push('\n'); }
  for child in element.children() {
    match child.value() {
      Node::Text(t) => out.push_str(t),
      Node::Element(_) => {
        if let Some(e) = scraper::ElementRef::wrap(child) { collect_text(e, out); }
      }
      _ => {}
    }
  }
  if block { out.push('\n'); }
}

/// 从 EPUB 读取正文；遇到坏章节显式报错，避免返回看似完整的残缺正文。
pub fn extract(path: &Path) -> anyhow::Result<BookText> {
  let epub = rbook::Epub::open(path.to_string_lossy().as_ref()).context("EPUB 解析失败")?;
  let mut reader = epub.reader_builder().create();
  let mut chapters = Vec::new();
  while let Some(result) = reader.read_next() {
    let data = result.context("EPUB 章节读取失败")?;
    let raw = data.content().to_string();
    chapters.push(TextChapter {
      title: crate::utils::html_title(&raw).unwrap_or_else(|| format!("第 {} 节", chapters.len() + 1)),
      paragraphs: paragraphs(&raw),
    });
  }
  anyhow::ensure!(!chapters.is_empty(), "EPUB 中没有章节");
  Ok(BookText { chapters })
}

fn number(args: &Value, key: &str, default: usize, max: usize) -> Result<usize, String> {
  match args.get(key) {
    None => Ok(default),
    Some(v) => v.as_u64().filter(|n| *n > 0 && *n <= max as u64)
      .map(|n| n as usize).ok_or_else(|| format!("{key} 必须是 1 到 {max} 的整数")),
  }
}

fn passage(book_id: i64, chapter: usize, paragraph: usize, text: &str) -> Value {
  json!({"chapter": chapter, "paragraph": paragraph,
    "source": format!("book:{book_id}/chapter:{chapter}/paragraph:{paragraph}"), "text": text})
}

/// 执行有分页、输出限额及来源定位的正文查询。
pub fn query(book: &BookText, book_id: i64, name: &str, args: &Value) -> Result<Value, String> {
  match name {
    "get_book_outline" => {
      let start = number(args, "start_chapter", 1, book.chapters.len())?;
      let limit = number(args, "limit", 30, 100)?;
      let items: Vec<_> = book.chapters.iter().enumerate().skip(start - 1).take(limit)
        .map(|(i, c)| json!({"chapter": i + 1, "title": c.title, "paragraph_count": c.paragraphs.len()})).collect();
      Ok(json!({"book_id": book_id, "chapter_count": book.chapters.len(), "chapters": items,
        "next_chapter": (start - 1 + items.len() < book.chapters.len()).then_some(start + items.len()),
        "note": "节按 EPUB spine 排序，可能包含扉页；段号为纯文本定位单元，超长段会拆分。"}))
    }
    "read_book_section" => {
      if args.get("chapter").is_none() { return Err("缺少 chapter 参数".into()); }
      let chapter = number(args, "chapter", 1, book.chapters.len())?;
      let c = &book.chapters[chapter - 1];
      if c.paragraphs.is_empty() { return Ok(json!({"book_id": book_id, "chapter": chapter, "paragraphs": [], "note": "本节无可提取文本，可能为图片页"})); }
      let start = number(args, "start_paragraph", 1, c.paragraphs.len())?;
      let limit = number(args, "limit", 5, 10)?;
      let items: Vec<_> = c.paragraphs.iter().enumerate().skip(start - 1).take(limit)
        .map(|(i, p)| passage(book_id, chapter, i + 1, p)).collect();
      Ok(json!({"book_id": book_id, "title": c.title, "paragraph_count": c.paragraphs.len(), "paragraphs": items,
        "next_paragraph": (start - 1 + items.len() < c.paragraphs.len()).then_some(start + items.len())}))
    }
    "search_book_text" => {
      let keyword = args["keyword"].as_str().map(str::trim).filter(|s| !s.is_empty())
        .ok_or("keyword 不能为空")?.to_lowercase();
      let limit = number(args, "limit", 5, 10)?;
      let start = number(args, "start_match", 1, usize::MAX)?;
      let chapter = args.get("chapter").map(|_| number(args, "chapter", 1, book.chapters.len())).transpose()?;
      let hits: Vec<_> = book.chapters.iter().enumerate()
        .filter(|(i, _)| chapter.is_none_or(|n| n == i + 1))
        .flat_map(|(ci, c)| c.paragraphs.iter().enumerate().filter(|(_, p)| p.to_lowercase().contains(&keyword))
          .map(move |(pi, _)| (ci, pi))).collect();
      let items: Vec<_> = hits.iter().skip(start - 1).take(limit).map(|&(ci, pi)| {
        let c = &book.chapters[ci];
        let context: Vec<_> = (pi.saturating_sub(1)..=(pi + 1).min(c.paragraphs.len() - 1))
          .map(|i| passage(book_id, ci + 1, i + 1, &c.paragraphs[i])).collect();
        json!({"chapter_title": c.title, "match_paragraph": pi + 1, "context": context})
      }).collect();
      Ok(json!({"book_id": book_id, "total_matches": hits.len(), "matches": items,
        "next_match": (start.saturating_sub(1).saturating_add(items.len()) < hits.len()).then(|| start + items.len())}))
    }
    _ => Err("未知正文工具".into()),
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn extracts_blocks_without_scripts_and_preserves_inline_text() {
    assert_eq!(paragraphs("<head><title>忽略</title></head><body><p>甲<b>说</b>：&amp;乙</p><div>九点<br/>离开</div><script>忽略</script></body>"), vec!["甲说：&乙", "九点", "离开"]);
    assert_eq!(paragraphs(&format!("<p>{}</p>", "甲".repeat(2401))).iter().map(|p| p.chars().count()).collect::<Vec<_>>(), vec![1200, 1200, 1]);
  }

  #[test]
  fn queries_paginate_and_return_exact_sources() {
    let book = BookText { chapters: vec![TextChapter { title: "第一章".into(), paragraphs: vec!["前文".into(), "甲说九点离开".into(), "后文".into(), "九点钟".into()] }] };
    let r = query(&book, 7, "search_book_text", &json!({"keyword":"九点", "limit":1})).unwrap();
    assert_eq!(r["total_matches"], 2);
    assert_eq!(r["next_match"], 2);
    assert_eq!(r["matches"][0]["context"][1]["source"], "book:7/chapter:1/paragraph:2");
    let r = query(&book, 7, "read_book_section", &json!({"chapter":1,"start_paragraph":2,"limit":1})).unwrap();
    assert_eq!(r["paragraphs"][0]["text"], "甲说九点离开");
    assert_eq!(r["next_paragraph"], 3);
    assert!(query(&book, 7, "read_book_section", &json!({"chapter":0})).is_err());
    assert!(query(&book, 7, "search_book_text", &json!({"keyword":" "})).is_err());
    assert_eq!(query(&book, 7, "get_book_outline", &json!({})).unwrap()["chapters"][0]["paragraph_count"], 4);
    assert_eq!(query(&book, 7, "search_book_text", &json!({"keyword":"不存在"})).unwrap()["total_matches"], 0);
  }

  #[test]
  fn sample_epubs_are_readable_and_unchanged() {
    for name in ["钟表馆事件.epub", "密室收藏家.epub"] {
      let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("sample").join(name);
      let before = std::fs::read(&path).unwrap();
      let book = extract(&path).unwrap();
      assert!(book.chapters.iter().map(|c| c.paragraphs.len()).sum::<usize>() > 100);
      let (ci, c) = book.chapters.iter().enumerate().find(|(_, c)| !c.paragraphs.is_empty()).unwrap();
      let result = query(&book, 1, "read_book_section", &json!({"chapter": ci + 1, "limit": 1})).unwrap();
      assert_eq!(result["paragraphs"][0]["text"], c.paragraphs[0]);
      assert_eq!(before, std::fs::read(&path).unwrap());
    }
  }
}
