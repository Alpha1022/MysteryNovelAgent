use std::path::Path;

/// 复制 EPUB 到书库并按规则重命名
///
/// 返回书库内的文件名（相对路径），原始文件保持不动
pub fn copy_into_library(
  library_path: &Path,
  epub_path: &Path,
  author: &str,
  title: &str,
  series: Option<(&str, i64)>,
) -> anyhow::Result<String> {
  let name = to_ascii_filename(author, title, series);
  let dest = resolve_collision(library_path, &name);
  std::fs::copy(epub_path, &dest)?;
  tracing::info!("已复制入书库: {}", dest.display());
  Ok(dest.file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or(name))
}

/// 生成 ASCII 化的 EPUB 文件名
/// 规则：`{作者拼音}-{书名拼音}.epub`，系列书加 `[系列拼音-N]` 前缀
pub fn to_ascii_filename(
  author: &str,
  title: &str,
  series: Option<(&str, i64)>,
) -> String {
  let author_slug = slugify(author);
  let title_slug = slugify(title);

  let mut name = match series {
    Some((s, n)) => format!("[{}-{}] {}-{}", slugify(s), n, author_slug, title_slug),
    None => format!("{author_slug}-{title_slug}"),
  };

  if name.is_empty() {
    name = "untitled".to_string();
  }
  format!("{name}.epub")
}

/// 将字符串转为拼音 slug：汉字转拼音（音节间连字符），仅保留字母数字，其余转连字符
///
/// 韵母 ü（含带调 ǖǘǚǜ）转为 v（如「吕」→ lv）
pub fn slugify(s: &str) -> String {
  use pinyin::ToPinyin;

  let plain = |p: &str| -> String {
    p.chars()
      .map(|c| match c {
        'ü' | 'ǖ' | 'ǘ' | 'ǚ' | 'ǜ' => 'v',
        other => other,
      })
      .collect()
  };

  let mut out = String::new();
  let mut last_sep = true; // 起始为 true，避免开头出现连字符

  // to_pinyin() 与 chars() 一一对应：汉字产出 Some(拼音)，其他产出 None
  for (ch, py) in s.chars().zip(s.to_pinyin()) {
    match py {
      Some(p) => {
        if !last_sep && !out.is_empty() {
          out.push('-');
        }
        out.push_str(&plain(p.plain()));
        out.push('-');
        last_sep = true;
      }
      None => {
        if ch.is_ascii_alphanumeric() {
          out.push(ch.to_ascii_lowercase());
          last_sep = false;
        } else if !last_sep && !out.is_empty() {
          out.push('-');
          last_sep = true;
        }
      }
    }
  }

  out.trim_matches('-').to_string()
}

/// 处理文件名冲突：若已存在则追加 `-2`、`-3` ...
pub fn resolve_collision(dir: &Path, name: &str) -> std::path::PathBuf {
  let candidate = dir.join(name);
  if !candidate.exists() {
    return candidate;
  }

  let stem = name.strip_suffix(".epub").unwrap_or(name);
  for i in 2..1000 {
    let candidate = dir.join(format!("{stem}-{i}.epub"));
    if !candidate.exists() {
      return candidate;
    }
  }
  dir.join(format!("{stem}-{}.epub", std::process::id()))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_slugify() {
    // 注意：「辻」是日文汉字，拼音库映射为 shi
    assert_eq!(slugify("绫辻行人"), "ling-shi-xing-ren");
    assert!(slugify("Agatha Christie").contains("agatha"));
    assert!(!slugify("钟表馆事件：修").contains('：'));
    assert!(slugify("钟表馆事件").is_ascii());
    // 韵母 ü 转 v
    assert_eq!(slugify("吕布"), "lv-bu");
    assert_eq!(slugify("绿色"), "lv-se");
  }

  #[test]
  fn test_filename() {
    let name = to_ascii_filename("绫辻行人", "钟表馆事件", Some(("馆系列", 3)));
    assert!(name.starts_with("[guan-xi-lie-3]"));
    assert!(name.ends_with(".epub"));
    assert!(name.is_ascii());
  }
}
