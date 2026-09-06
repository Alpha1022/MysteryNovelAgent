//! 内置阅读器：按 spine 顺序提取 EPUB 章节正文（章节化 + 图片内联 data URI）
//!
//! 提取产物供 GUI 阅读弹窗渲染：正文剥离脚本与事件属性（安全），
//! 图片引用改写为 base64 data URI（阅读弹窗内可独立显示，无需资源服务）。

use std::collections::HashMap;
use std::path::Path;

use anyhow::Context;
use tracing::warn;

/// 单张内联图片大小上限（超过则保留原引用，不做内联）
const MAX_IMAGE_BYTES: usize = 10 * 1024 * 1024;
/// 全书内联图片总预算（超出后停止内联，防止超大 EPUB 撑爆内存）
const TOTAL_INLINE_BUDGET: usize = 64 * 1024 * 1024;

/// 一个章节的正文（已消毒，图片已内联）
pub struct ReaderChapter {
  pub title: String,
  pub html: String,
}

/// 阅读器内容提取结果
pub struct ReaderContent {
  pub title: String,
  pub chapters: Vec<ReaderChapter>,
}

/// 读取 EPUB：按 spine 顺序提取全部章节
pub fn extract_content(path: &Path) -> anyhow::Result<ReaderContent> {
  let epub = rbook::Epub::open(path.to_string_lossy().as_ref())
    .context("EPUB 解析失败（文件可能已损坏）")?;
  let title = epub
    .metadata()
    .title()
    .map(|t| t.value().trim().to_string())
    .filter(|t| !t.is_empty())
    .unwrap_or_else(|| "未命名".to_string());

  // 图片资源索引：manifest href（/根相对形式）→ (mime, bytes)
  let mut images: HashMap<String, (String, Vec<u8>)> = HashMap::new();
  let mut budget = TOTAL_INLINE_BUDGET;
  for img in epub.manifest().images() {
    let href = img.href().as_str().trim_start_matches('/').to_string();
    let mime = match img.kind().subtype() {
      "jpg" | "jpeg" => "image/jpeg",
      "png" => "image/png",
      "gif" => "image/gif",
      "svg" => "image/svg+xml",
      "webp" => "image/webp",
      _ => continue,
    };
    match img.read_bytes() {
      Ok(bytes) => {
        if bytes.len() <= MAX_IMAGE_BYTES && bytes.len() <= budget {
          budget -= bytes.len();
          images.insert(href, (mime.to_string(), bytes));
        }
      }
      Err(e) => warn!("阅读器跳过图片资源 [{href}]: {e}"),
    }
  }

  let mut chapters: Vec<ReaderChapter> = Vec::new();
  let mut reader = epub.reader_builder().create();
  while let Some(Ok(data)) = reader.read_next() {
    let raw = data.content().to_string();
    // 章节基准目录（根相对，含尾斜杠），用于解析相对资源引用
    let base_dir = match data.manifest_entry().href().as_str().rsplit_once('/') {
      Some((dir, _)) => format!("/{dir}/"),
      None => "/".to_string(),
    };
    let title = crate::utils::html_title(&raw)
      .unwrap_or_else(|| format!("第 {} 节", chapters.len() + 1));
    let html = sanitize_and_inline(&raw, &base_dir, &images);
    chapters.push(ReaderChapter { title, html });
  }

  if chapters.is_empty() {
    anyhow::bail!("EPUB 中没有可读章节");
  }
  Ok(ReaderContent { title, chapters })
}

/// 消毒正文（剥离 script/iframe/事件属性）并把图片引用内联为 data URI
fn sanitize_and_inline(
  html: &str,
  base_dir: &str,
  images: &HashMap<String, (String, Vec<u8>)>,
) -> String {
  // 剥离脚本与内嵌框架（自闭合与成对标签）
  let re_strip = regex::Regex::new(
    r"(?is)<script\b[^>]*>.*?</script\s*>|<script\b[^>]*/?>|<iframe\b[^>]*>.*?</iframe\s*>|<iframe\b[^>]*/?>",
  )
  .expect("固定正则");
  let html = re_strip.replace_all(html, "");

  // 剥离事件属性（onclick 等）
  let re_on = regex::Regex::new(r#"(?is)\son[a-z]+\s*=\s*("[^"]*"|'[^']*'|[^\s>]+)"#)
    .expect("固定正则");
  let html = re_on.replace_all(&html, "");

  // src / xlink:href 引用 → data URI（data:/http: 直接跳过，非资源属性不影响）
  let re_attr =
    regex::Regex::new(r#"(?i)(\b(?:src|xlink:href)\s*=\s*")([^"]+)(")"#).expect("固定正则");
  re_attr
    .replace_all(&html, |caps: &regex::Captures| {
      let url = caps.get(2).map(|m| m.as_str()).unwrap_or("");
      if url.starts_with("data:") || url.contains("://") {
        return caps[0].to_string();
      }
      let key = resolve_ref(base_dir, url);
      // 资源表以去前导斜杠的路径为键（与 manifest href 归一方式一致）
      let key = key.trim_start_matches('/');
      match images.get(key) {
        Some((mime, bytes)) => format!(
          "{}data:{mime};base64,{}{}",
          &caps[1],
          base64_encode(bytes),
          &caps[3]
        ),
        // 资源表中没有的引用保持原样（阅读时显示 alt/占位）
        None => caps[0].to_string(),
      }
    })
    .into_owned()
}

/// 解析章节内相对引用 → EPUB 根路径（/根相对形式，与 manifest href 一致）
fn resolve_ref(base_dir: &str, src: &str) -> String {
  let src = src.split(['?', '#']).next().unwrap_or(src);
  let combined = if src.starts_with('/') {
    src.to_string()
  } else {
    format!("{base_dir}{src}")
  };
  normalize_path(&combined)
}

/// 归一化路径（消解 ../ 与 ./，保留 / 前缀）
fn normalize_path(path: &str) -> String {
  let mut parts: Vec<&str> = Vec::new();
  for seg in path.split('/') {
    match seg {
      ".." => {
        parts.pop();
      }
      "." | "" => {}
      s => parts.push(s),
    }
  }
  format!("/{}", parts.join("/"))
}

/// 标准 base64 编码（避免引入新依赖的轻量实现）
fn base64_encode(data: &[u8]) -> String {
  const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
  let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
  for chunk in data.chunks(3) {
    let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
    let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
    out.push(TABLE[(n >> 18) as usize & 63] as char);
    out.push(TABLE[(n >> 12) as usize & 63] as char);
    out.push(if chunk.len() > 1 { TABLE[(n >> 6) as usize & 63] as char } else { '=' });
    out.push(if chunk.len() > 2 { TABLE[n as usize & 63] as char } else { '=' });
  }
  out
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_base64_encode() {
    assert_eq!(base64_encode(b""), "");
    assert_eq!(base64_encode(b"f"), "Zg==");
    assert_eq!(base64_encode(b"fo"), "Zm8=");
    assert_eq!(base64_encode(b"foo"), "Zm9v");
    assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    // 二进制
    assert_eq!(base64_encode(&[0xFF, 0xD8, 0xFF]), "/9j/");
  }

  #[test]
  fn test_resolve_ref() {
    // 相对引用基于章节目录
    assert_eq!(resolve_ref("/OEBPS/text/", "img/pic.jpg"), "/OEBPS/text/img/pic.jpg");
    assert_eq!(resolve_ref("/OEBPS/", "cover.jpg"), "/OEBPS/cover.jpg");
    // 上级目录
    assert_eq!(resolve_ref("/OEBPS/text/", "../images/p.jpg"), "/OEBPS/images/p.jpg");
    // 根相对引用
    assert_eq!(resolve_ref("/OEBPS/text/", "/img/x.png"), "/img/x.png");
    // 查询串与锚点剥离
    assert_eq!(resolve_ref("/OEBPS/", "a.png?v=1#x"), "/OEBPS/a.png");
  }

  /// 消毒 + 图片内联：script 移除、事件属性移除、图片转 data URI
  #[test]
  fn test_sanitize_and_inline() {
    let mut images = HashMap::new();
    images.insert("OEBPS/pic.jpg".to_string(), ("image/jpeg".to_string(), vec![0xFF, 0xD8, 0xFF]));
    let html = r#"<html><head><script>alert(1)</script></head>
      <body onload="evil()" onclick='x()'>
      <img src="pic.jpg"/><img src="../missing.png"/><iframe src="x"></iframe>
      </body></html>"#;
    let out = sanitize_and_inline(html, "/OEBPS/", &images);
    assert!(!out.contains("script"), "script 应被剥离: {out}");
    assert!(!out.contains("onload"), "事件属性应被剥离");
    assert!(!out.contains("iframe"));
    assert!(out.contains("data:image/jpeg;base64,/9j/"), "图片应内联: {out}");
    assert!(out.contains("missing.png"), "未命中的引用保持原样");
  }
}
