use std::path::Path;

/// 统计字符串中的汉字数量（用于短评质量过滤与语言检测）
pub fn count_han(s: &str) -> usize {
  s.chars().filter(|c| ('\u{4e00}'..='\u{9fff}').contains(c)).count()
}

/// 繁体中文转简体中文（简介翻译的本地快速路径，无需 LLM）
pub fn to_simplified(text: &str) -> String {
  character_converter::traditional_to_simplified(text).into_owned()
}

/// 从文件路径提取不带 `.epub` 后缀的文件名，用作备用搜索词
pub fn filename_stem(path: &Path) -> String {
  path
    .file_stem()
    .map(|s| s.to_string_lossy().into_owned())
    .unwrap_or_default()
}

/// 规范化空白：合并连续空格/换行，去除首尾
pub fn normalize_ws(s: &str) -> String {
  s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 将标签列表格式化为逗号分隔字符串
pub fn join_tags(tags: &[String]) -> String {
  tags.join(", ")
}

/// 将逗号分隔的字符串拆分为标签列表
pub fn split_tags(s: &str) -> Vec<String> {
  s
    .split([',', '，'])
    .map(|t| t.trim().to_string())
    .filter(|t| !t.is_empty())
    .collect()
}

/// 从 XHTML 内容中提取 `<title>` 标签文本（章节标题提取；无则 None）
pub fn html_title(xhtml: &str) -> Option<String> {
  let re = regex::Regex::new(r"(?is)<title[^>]*>(.*?)</title>").ok()?;
  re
    .captures(xhtml)
    .map(|c| c[1].trim().to_string())
    .filter(|s| !s.is_empty())
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn test_count_han() {
    assert_eq!(count_han("好看"), 2);
    assert_eq!(count_han("这本书非常好看，强烈推荐"), 11);
    assert_eq!(count_han("hello world"), 0);
  }

  #[test]
  fn test_split_join_tags() {
    let tags = split_tags("本格推理, 日本, 妖怪推理");
    assert_eq!(tags, vec!["本格推理", "日本", "妖怪推理"]);
    assert_eq!(join_tags(&tags), "本格推理, 日本, 妖怪推理");
  }

  #[test]
  fn test_traditional_to_simplified() {
    let t = character_converter::traditional_to_simplified("鐘錶館事件");
    assert_eq!(t, "钟表馆事件");
    // 简体输入应保持不变
    assert_eq!(character_converter::traditional_to_simplified("白夜行"), "白夜行");
  }
}
