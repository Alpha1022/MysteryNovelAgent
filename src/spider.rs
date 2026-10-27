use scraper::{ElementRef, Html, Selector};
use tracing::warn;

use crate::utils::{count_han, normalize_ws};

// ============================= //
//  Claspclub API (JSON)
// ============================= //

/// 百科目录支持的类别（与网站 kind 参数一致）。
pub const CLASP_WIKI_KINDS: [&str; 9] = [
  "term", "culture", "event", "author", "series", "detective", "award", "publisher", "imprint",
];

/// 百科目录响应；不同类别的条目保留原始字段，避免丢失别名与来源信息。
#[derive(serde::Deserialize, Debug)]
pub struct ClaspWikiResponse {
  pub items: Vec<serde_json::Value>,
  #[serde(default)]
  pub pagination: Option<ClaspPagination>,
}

/// 查询百科目录；q 为空时浏览类别，服务端固定每页 48 条。
pub async fn search_clasp_wiki(
  client: &reqwest::Client,
  kind: &str,
  keyword: &str,
  page: i64,
) -> anyhow::Result<ClaspWikiResponse> {
  anyhow::ensure!(CLASP_WIKI_KINDS.contains(&kind), "不支持的百科类别");
  let response = client.get("https://claspclub.com/api/v1/wiki/entries")
    .query(&[("kind", kind), ("q", keyword), ("page", &page.to_string())])
    .timeout(std::time::Duration::from_secs(15)).send().await?
    .error_for_status()?;
  Ok(response.json::<ClaspWikiResponse>().await?)
}

/// 构造受限的公开详情 API 路径；拒绝路径注入，不接受模型提供的任意 URL。
pub fn clasp_detail_path(kind: &str, id: &str) -> anyhow::Result<String> {
  anyhow::ensure!(!id.is_empty() && id.len() <= 128
    && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_')),
    "条目 ID 必须为 1～128 位字母、数字、连字符或下划线");
  let resource = match kind {
    "book" => "books",
    "author" => "authors",
    "series" => "series",
    "detective" => "detectives",
    "publisher" => "publishers",
    "imprint" => "imprints",
    // 2026-10-27 实测：文化目录条目的 kind 也可能是 term，三类共用此详情接口。
    "term" | "culture" | "event" => "wiki/entries",
    "award" => "wiki/awards",
    _ => anyhow::bail!("不支持的详情类别"),
  };
  Ok(format!("/api/v1/{resource}/{id}"))
}

/// 获取书籍或百科的完整公开详情 JSON，保留各类别特有字段及原始来源。
pub async fn fetch_clasp_public_detail(
  client: &reqwest::Client,
  kind: &str,
  id: &str,
) -> anyhow::Result<serde_json::Value> {
  let path = clasp_detail_path(kind, id)?;
  let response = client.get(format!("https://claspclub.com{path}"))
    .timeout(std::time::Duration::from_secs(15)).send().await?.error_for_status()?;
  let data = response.json::<serde_json::Value>().await?;
  anyhow::ensure!(data.is_object(), "详情 API 返回非对象数据");
  Ok(data)
}

/// claspclub 搜索建议 API 响应体
/// 对应 GET https://claspclub.com/api/v1/search/suggestions?keyword={书名}
#[derive(serde::Deserialize, Debug)]
pub struct ClaspSuggestionResp {
  pub books: Vec<ClaspBookSuggestion>,
}

/// claspclub 分页搜索 API 响应体（含每本书的详细信息与总页数）
/// 对应 GET https://claspclub.com/api/v1/books?keyword={}&sort=doubanRating&page={}&pageSize={}
#[derive(serde::Deserialize, Debug, Clone)]
pub struct ClaspBooksResp {
  #[serde(default)]
  pub items: Vec<ClaspBookItem>,
  #[serde(default)]
  pub pagination: Option<ClaspPagination>,
  /// "fuzzy" = 无精确匹配，平台正在返回相近结果（视为未搜到）
  #[serde(rename = "searchMode", default)]
  pub search_mode: Option<String>,
}

/// 分页信息
#[derive(serde::Deserialize, Debug, Clone, Default)]
pub struct ClaspPagination {
  #[serde(default)]
  pub page: i64,
  #[serde(rename = "pageSize", default)]
  pub page_size: i64,
  #[serde(default)]
  pub total: i64,
  #[serde(rename = "totalPages", default)]
  pub total_pages: i64,
}

/// 作者（分页搜索接口嵌套结构）
#[derive(serde::Deserialize, Debug, Clone)]
pub struct ClaspAuthor {
  #[serde(default)]
  pub name: String,
}

/// 标签（分页搜索接口为对象数组）
#[derive(serde::Deserialize, Debug, Clone)]
pub struct ClaspTag {
  #[serde(default)]
  pub name: String,
}

/// 分页搜索返回的单本书（携带无剧透简介，可省去详情 API 调用）
/// 注意：该接口不返回系列信息（series 仍需详情 API 或放弃）
#[derive(serde::Deserialize, Debug, Clone)]
pub struct ClaspBookItem {
  pub id: String,
  #[serde(default)]
  pub title: String,
  /// 主要作者（部分条目仅此字段）
  #[serde(rename = "author", default)]
  pub author: Option<ClaspAuthor>,
  /// 作者列表（常规来源，取第一个）
  #[serde(default)]
  pub authors: Vec<ClaspAuthor>,
  #[serde(rename = "coverUrl", default)]
  pub cover_url: Option<String>,
  #[serde(rename = "summaryNoSpoiler", default)]
  pub summary_no_spoiler: Option<String>,
  #[serde(default)]
  pub tags: Vec<ClaspTag>,
  #[serde(rename = "doubanRating", default)]
  pub douban_rating: Option<f64>,
}

impl ClaspBookItem {
  /// 作者名：authors[0] 优先，author 兜底
  pub fn author_name(&self) -> String {
    self
      .authors
      .first()
      .map(|a| a.name.trim().to_string())
      .filter(|s| !s.is_empty())
      .or_else(|| {
        self
          .author
          .as_ref()
          .map(|a| a.name.trim().to_string())
          .filter(|s| !s.is_empty())
      })
      .unwrap_or_default()
  }

  /// 标签名列表
  pub fn tag_names(&self) -> Vec<String> {
    self.tags.iter().map(|t| t.name.trim().to_string()).filter(|t| !t.is_empty()).collect()
  }
}

/// 分页搜索条目 → 统一建议结构（豆瓣链接从封面 URL 推断）
impl From<&ClaspBookItem> for ClaspBookSuggestion {
  fn from(item: &ClaspBookItem) -> Self {
    let douban_url = item
      .cover_url
      .as_deref()
      .and_then(extract_douban_id_from_cover)
      .map(|id| douban_book_url(&id));
    ClaspBookSuggestion {
      title: item.title.trim().to_string(),
      author_name: item.author_name(),
      id: item.id.clone(),
      tags: item.tag_names(),
      cover_url: item.cover_url.clone(),
      summary: item.summary_no_spoiler.clone().filter(|s| !s.trim().is_empty()),
      douban_url,
    }
  }
}

/// API 返回的单本书的数据
#[derive(serde::Deserialize, Debug, Clone)]
pub struct ClaspBookSuggestion {
  pub title: String,
  #[serde(rename = "authorName")]
  pub author_name: String,
  pub id: String,
  #[serde(default)]
  pub tags: Vec<String>,
  #[serde(rename = "coverUrl", default)]
  pub cover_url: Option<String>,
  /// 分页搜索接口携带的无剧透简介（旧建议接口无此字段 → 导入时回退详情 API）
  #[serde(default)]
  pub summary: Option<String>,
  /// 豆瓣书籍页链接（分页搜索接口从封面 URL 推断；详情接口用精准版本链接）
  #[serde(default)]
  pub douban_url: Option<String>,
}

/// 从封面 URL 中提取豆瓣 subject ID
/// 例: https://...book-covers/douban-26771719.jpg -> "26771719"
pub fn extract_douban_id_from_cover(cover_url: &str) -> Option<String> {
  // 封面 URL 形如: .../douban-26771719.jpg
  let re = regex::Regex::new(r"douban-(\d+)").ok()?;
  re
    .captures(cover_url)
    .and_then(|c| c.get(1))
    .map(|m| m.as_str().to_string())
}

/// 根据豆瓣 subject ID 构造书籍页 URL
pub fn douban_book_url(subject_id: &str) -> String {
  format!("https://book.douban.com/subject/{subject_id}/")
}

/// 从豆瓣书籍页或短评页链接提取 subject ID，拒绝其他域名及非数字 ID。
pub fn douban_subject_id(input: &str) -> Option<String> {
  let url = reqwest::Url::parse(input.trim()).ok()?;
  if !matches!(url.scheme(), "http" | "https") || url.host_str() != Some("book.douban.com") {
    return None;
  }
  let mut parts = url.path_segments()?;
  if parts.next()? != "subject" { return None; }
  let id = parts.next()?;
  (!id.is_empty() && id.bytes().all(|b| b.is_ascii_digit())).then(|| id.to_string())
}

/// 豆瓣在线评论页的必要信息，不保留整页 HTML。
pub struct DoubanDiscussionPage {
  pub book_title: Option<String>,
  pub items: Vec<serde_json::Value>,
  pub has_next: bool,
}

/// 豆瓣书评全文接口响应：html 是正文，body 含作者、剧透提示及操作区。
#[derive(serde::Deserialize)]
pub struct DoubanFullReviewResponse {
  pub html: Option<String>,
  pub body: Option<String>,
  #[serde(default)]
  pub votes: serde_json::Value,
}

fn first_element<'a>(root: ElementRef<'a>, selectors: &[&str]) -> Option<ElementRef<'a>> {
  selectors.iter().filter_map(|s| Selector::parse(s).ok())
    .find_map(|s| root.select(&s).next())
}

fn element_text(root: ElementRef<'_>, selectors: &[&str]) -> Option<String> {
  first_element(root, selectors).map(|e| douban_html_text(&e.html())).filter(|s| !s.is_empty())
}

fn vote_number(root: ElementRef<'_>, selectors: &[&str]) -> Option<i64> {
  let text = element_text(root, selectors)?;
  text.chars().filter(|c| c.is_ascii_digit()).collect::<String>().parse().ok()
}

/// HTML 转纯文本，保留段落与 br；忽略脚本、样式及展开按钮。
pub fn douban_html_text(html: &str) -> String {
  fn collect(root: ElementRef<'_>, out: &mut String) {
    let tag = root.value().name();
    if matches!(tag, "script" | "style" | "head" | "iframe" | "noscript")
      || root.value().classes().any(|c| matches!(c, "unfold" | "spoiler-tip" | "main-title-tip")) { return; }
    let block = matches!(tag, "p" | "div" | "section" | "blockquote" | "li" | "br" | "h1" | "h2" | "h3" | "tr");
    if block { out.push('\n'); }
    for child in root.children() {
      match child.value() {
        scraper::Node::Text(t) => out.push_str(t),
        scraper::Node::Element(_) => if let Some(e) = ElementRef::wrap(child) { collect(e, out); },
        _ => {}
      }
    }
    if block { out.push('\n'); }
  }
  let doc = Html::parse_fragment(html);
  let mut out = String::new();
  collect(doc.root_element(), &mut out);
  out.lines().map(normalize_ws).filter(|s| !s.is_empty()).collect::<Vec<_>>().join("\n")
}

/// 解析豆瓣在线短评/热门书评列表；反爬或未知结构报错，避免伪装成空结果。
pub fn parse_douban_discussions(html: &str, reviews: bool) -> anyhow::Result<DoubanDiscussionPage> {
  let doc = Html::parse_document(html);
  let root = doc.root_element();
  // 2026-10-27 实测：短评为 #comments > li.comment-item；书评为 .review-list .review-item。
  // 旧版结构兜底：短评 li[data-cid]、书评 .main.review-item。
  let selectors = if reviews { [".review-list .review-item", ".main.review-item"] }
    else { ["#comments li.comment-item", "li[data-cid]"] };
  let mut items = Vec::new();
  for selector in selectors {
    let selector = Selector::parse(selector).map_err(|e| anyhow::anyhow!("评论选择器错误: {e}"))?;
    for item in doc.select(&selector) {
      // 短评：.comment-info a / .comment-time / span.short / .vote-count。
      // 书评：h2 a[href] / .main-hd .name / .main-meta / .short-content / .action .up、.down。
      let content = element_text(item, if reviews { &[".short-content", ".review-short"] }
        else { &["span.short", "p.comment-content"] });
      let Some(content) = content else { continue; };
      let id = item.value().attr(if reviews { "id" } else { "data-cid" })
        .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit())).map(str::to_string);
      let mut value = serde_json::json!({"id":id,"rating":parse_rating(&item),"content":content,
        "author":element_text(item, if reviews { &[".main-hd .name", "header a.name"] } else { &[".comment-info a", ".avatar a"] }),
        "date":element_text(item, if reviews { &[".main-meta", "header time"] } else { &[".comment-time", ".comment-info time"] }),
        "usefulness":vote_number(item, if reviews { &[".action .up", ".action [title='有用']"] } else { &[".vote-count", "span[id^='c-']"] }).unwrap_or(0)});
      if reviews {
        value["title"] = serde_json::json!(element_text(item, &["h2 a", "h2"]));
        value["spoiler"] = serde_json::json!(first_element(item, &[".spoiler-tip", ".main-title-tip"]).is_some());
        value["uselessness"] = serde_json::json!(vote_number(item, &[".action .down", ".action [title='没用']"]));
      }
      if let Some(id) = id {
        value["url"] = serde_json::json!(if reviews { format!("https://book.douban.com/review/{id}/") }
          else { format!("https://book.douban.com/comment/{id}/") });
      }
      items.push(value);
    }
    if !items.is_empty() { break; }
  }
  // 正常空列表有 #comments / .review-list 容器；验证/登录页没有这些容器。
  anyhow::ensure!(!items.is_empty() || first_element(root, if reviews { &[".review-list"] } else { &["#comments"] }).is_some(),
    "豆瓣页面不是可识别的评论列表，可能需要登录、遇到访问限制或页面结构变化");
  // 短评 #paginator a[data-page=next]；书评 .paginator 的 rel=next（a/link 两种结构）。
  let has_next = first_element(root, &["#paginator a[data-page='next'][href]", ".paginator [rel='next'][href]", ".paginator .next a[href]"]).is_some();
  let book_title = element_text(root, &["#content h1", "h1"]).map(|s| {
    if reviews { s.split("的书评").next().unwrap_or(&s).trim().to_string() }
    else { s.trim_end_matches("短评").trim().to_string() }
  });
  Ok(DoubanDiscussionPage { book_title, items, has_next })
}

/// 提取书评全文接口的正文、作者、剧透标识、投票数与正文图片链接。
pub fn parse_douban_full_review(data: DoubanFullReviewResponse) -> anyhow::Result<serde_json::Value> {
  let body = data.body.as_deref().unwrap_or("");
  let doc = Html::parse_fragment(body);
  let root = doc.root_element();
  // 实测 body 的 .review-content[data-author] 包含正文，.main-title-tip 为剧透提示。
  // html 缺失时退回 body 内 .review-content（旧结构 #link-report-*）。
  let content_html = data.html.filter(|s| !s.trim().is_empty())
    .or_else(|| first_element(root, &[".review-content", "[id^='link-report-']"]).map(|e| e.inner_html()))
    .ok_or_else(|| anyhow::anyhow!("书评全文接口未返回正文，可能需要登录或遇到访问限制"))?;
  let content = douban_html_text(&content_html);
  anyhow::ensure!(!content.is_empty(), "书评正文没有可提取文本");
  let content_doc = Html::parse_fragment(&content_html);
  // 正文中的 img[data-original] / img[src]；仅返回地址，不下载图片。
  let image_selector = Selector::parse("img").map_err(|e| anyhow::anyhow!("图片选择器错误: {e}"))?;
  let images: Vec<_> = content_doc.select(&image_selector).filter_map(|e| e.value().attr("data-original").or_else(|| e.value().attr("src")))
    .filter(|s| s.starts_with("https://") || s.starts_with("http://")).take(20).collect();
  Ok(serde_json::json!({"content":content,
    "author":first_element(root, &[".review-content[data-author]", "[data-author]"]).and_then(|e| e.value().attr("data-author")),
    "spoiler":first_element(root, &[".main-title-tip", ".spoiler-tip"]).is_some(),
    "usefulness":data.votes.get("useful_count").or_else(|| data.votes.get("usecount")),
    "uselessness":data.votes.get("useless_count"),"image_urls":images}))
}

/// 获取固定豆瓣域名的公开页面；HTTP 错误/登录重定向明确失败，不绕过验证码。
pub async fn fetch_douban_public(client: &reqwest::Client, url: &str) -> anyhow::Result<String> {
  let parsed = reqwest::Url::parse(url)?;
  anyhow::ensure!(parsed.scheme() == "https" && parsed.host_str() == Some("book.douban.com"), "仅允许豆瓣图书公开接口");
  let bid = format!("{:x}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default().as_nanos());
  let response = client.get(parsed).timeout(std::time::Duration::from_secs(15))
    .header("User-Agent", "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36")
    .header("Referer", "https://book.douban.com/").header("Cookie", format!("bid={}", &bid[..bid.len().min(11)]))
    .send().await?.error_for_status()?;
  anyhow::ensure!(response.url().host_str() == Some("book.douban.com"), "豆瓣请求重定向到登录或验证页面");
  Ok(response.text().await?)
}

/// 调用详情 API 获取书籍的简介、封面、系列、豆瓣链接
pub async fn fetch_book_detail(
  client: &reqwest::Client,
  clasp_id: &str,
) -> anyhow::Result<ClaspBookDetail> {
  let url = format!("https://claspclub.com/api/v1/books/{clasp_id}");
  let resp = client.get(&url).send().await?;
  if !resp.status().is_success() {
    anyhow::bail!("详情 API 返回 HTTP {}", resp.status());
  }
  Ok(resp.json::<ClaspBookDetail>().await?)
}

/// 调用分页搜索 API：按豆瓣评分排序，返回条目（含简介）与总页数
///
/// 对应 GET https://claspclub.com/api/v1/books?keyword={}&sort=doubanRating&page={}&pageSize={}
pub async fn search_books_paged(
  client: &reqwest::Client,
  keyword: &str,
  page: i64,
  page_size: i64,
) -> anyhow::Result<ClaspBooksResp> {
  let encoded =
    percent_encoding::utf8_percent_encode(keyword, percent_encoding::NON_ALPHANUMERIC);
  let url = format!(
    "https://claspclub.com/api/v1/books?keyword={encoded}&sort=doubanRating&page={page}&pageSize={page_size}"
  );
  let resp = client.get(&url).send().await?;
  if !resp.status().is_success() {
    anyhow::bail!("分页搜索 API 返回 HTTP {}", resp.status());
  }
  Ok(resp.json::<ClaspBooksResp>().await?)
}

// ============================= //
//  ClaspBookMeta (整合后的元数据)
// ============================= //

/// claspclub 书籍详情 API 响应（仅取所需字段）
/// 对应 GET https://claspclub.com/api/v1/books/{id}
#[derive(serde::Deserialize, Debug)]
pub struct ClaspBookDetail {
  #[serde(default)]
  pub title: Option<String>,
  #[serde(default)]
  pub author: Option<ClaspAuthor>,
  #[serde(default)]
  pub tags: Vec<ClaspTag>,
  #[serde(rename = "summaryNoSpoiler", default)]
  pub summary_no_spoiler: Option<String>,
  #[serde(rename = "coverUrl", default)]
  pub cover_url: Option<String>,
  #[serde(default)]
  pub series: Option<ClaspSeriesInfo>,
  #[serde(default)]
  pub editions: Vec<ClaspEdition>,
}

/// 系列信息（详情 API 的 "series" 项）
///
/// 实际响应格式（GET /api/v1/books/{id}）：
/// `{"description": "…", "id": "…", "name": "馆系列", "order": 5}`；
/// 单行本该字段为 `null`。字段解析失败时降级为 None，不使整个详情解析失败。
#[derive(serde::Deserialize, Debug, Clone)]
pub struct ClaspSeriesInfo {
  #[serde(default)]
  pub name: Option<String>,
  /// 卷号：宽松解析（兼容整数 / 浮点 / 数字字符串）
  #[serde(default, deserialize_with = "deserialize_lenient_i64")]
  pub order: Option<i64>,
}

/// 宽松整数解析：兼容 JSON 整数 / 浮点（3.0 → 3）/ 数字字符串（"3" → 3），
/// 无法识别时返回 None（系列信息是尽力而为的字段，不阻断详情解析）
fn deserialize_lenient_i64<'de, D>(deserializer: D) -> Result<Option<i64>, D::Error>
where
  D: serde::Deserializer<'de>,
{
  use serde::Deserialize as _;
  let v = Option::<serde_json::Value>::deserialize(deserializer)?;
  let parsed = match v {
    None | Some(serde_json::Value::Null) => None,
    Some(serde_json::Value::Number(n)) => n
      .as_i64()
      .or_else(|| n.as_f64().map(|f| f.round() as i64)),
    Some(serde_json::Value::String(s)) => s
      .trim()
      .parse::<i64>()
      .ok()
      .or_else(|| s.trim().parse::<f64>().ok().map(|f| f.round() as i64)),
    Some(_) => None,
  };
  Ok(parsed)
}

/// 版本信息（豆瓣链接提取 + 更换封面弹窗的版本封面列表）
#[derive(serde::Deserialize, Debug, Clone)]
pub struct ClaspEdition {
  #[serde(rename = "isPrimary", default)]
  pub is_primary: bool,
  #[serde(rename = "doubanUrl", default)]
  pub douban_url: Option<String>,
  /// 版本封面（更换封面弹窗展示）
  #[serde(rename = "coverUrl", default)]
  pub cover_url: Option<String>,
  /// 出版社
  #[serde(default)]
  pub publisher: Option<String>,
  /// 丛书/文库（如"午夜文库"）
  #[serde(default)]
  pub imprint: Option<ClaspImprint>,
  /// 装帧（平装/精装）
  #[serde(default)]
  pub binding: Option<String>,
  /// 语种（简体中文/繁体中文/…）
  #[serde(default)]
  pub language: Option<String>,
  /// 出版时间（ISO 8601，展示时取年份）
  #[serde(rename = "publishedAt", default)]
  pub published_at: Option<String>,
  #[serde(default)]
  pub translator: Option<String>,
}

/// 丛书/文库信息
#[derive(serde::Deserialize, Debug, Clone)]
pub struct ClaspImprint {
  #[serde(default)]
  pub name: Option<String>,
  #[serde(rename = "publisherName", default)]
  pub publisher_name: Option<String>,
}

impl ClaspBookDetail {
  /// 取主版本的豆瓣链接，兜底取第一个有豆瓣链接的版本
  pub fn primary_douban_url(&self) -> Option<String> {
    self
      .editions
      .iter()
      .find(|e| e.is_primary)
      .and_then(|e| e.douban_url.clone())
      .or_else(|| {
        self
          .editions
          .iter()
          .find_map(|e| e.douban_url.clone())
      })
  }
}

/// claspclub 书籍元数据（最终用于用户确认和写入 EPUB）
#[derive(Debug, Clone, Default)]
pub struct ClaspBookMeta {
  pub title: String,
  pub author: String,
  pub tags: Vec<String>,
  /// 来源 clasp 条目 ID（合并本可能多条）
  pub clasp_ids: Vec<String>,
  /// 豆瓣链接（合并本可能多条）
  pub douban_urls: Vec<String>,
  /// 系列（合并本仅当所有条目同系列同卷号时继承）
  pub series_name: Option<String>,
  pub series_order: Option<i64>,
}

impl ClaspBookMeta {
  /// 从 API 搜索建议构造元数据（单本传长度为 1 的数组，合并本传多条）
  /// 标签/作者取去重并集（作者以顿号拼接）；豆瓣链接取所有条目封面推断的链接
  pub fn from_suggestions(items: &[ClaspBookSuggestion]) -> Self {
    let mut tags: Vec<String> = Vec::new();
    let mut douban_urls: Vec<String> = Vec::new();
    let clasp_ids: Vec<String> = items.iter().map(|s| s.id.clone()).collect();

    for s in items {
      for t in &s.tags {
        if !tags.contains(t) {
          tags.push(t.clone());
        }
      }
      if let Some(url) = s
        .cover_url
        .as_deref()
        .and_then(extract_douban_id_from_cover)
        .map(|id| douban_book_url(&id))
      {
        if !douban_urls.contains(&url) {
          douban_urls.push(url);
        }
      }
    }

    // 标题默认取第一条；作者取并集去重（多来源合并本）
    let title = items.first().map(|s| s.title.clone()).unwrap_or_default();
    let mut authors: Vec<String> = Vec::new();
    for s in items {
      let a = s.author_name.trim();
      if !a.is_empty() && !authors.iter().any(|x| x == a) {
        authors.push(a.to_string());
      }
    }

    ClaspBookMeta {
      title,
      author: authors.join("、"),
      tags,
      clasp_ids,
      douban_urls,
      series_name: None,
      series_order: None,
    }
  }
}

// ============================= //
//  豆瓣短评解析
// ============================= //

/// 一条豆瓣短评
#[derive(Debug, Clone)]
pub struct Comment {
  pub rating: Option<i32>,
  pub content: String,
  pub usefulness: i32,
}

/// 解析豆瓣短评页面 HTML，提取所有评论项
pub fn parse_douban_comments(html: &str) -> Vec<Comment> {
  let document = Html::parse_document(html);

  // 对应豆瓣短评页: <li class="comment-item">
  // 兜底选择器: li.comment-item, li[data-cid]
  let sel =
    Selector::parse("li.comment-item").unwrap_or_else(|_| Selector::parse("li[data-cid]").unwrap());

  document
    .select(&sel)
    .filter_map(|li| parse_one_comment(&li))
    .collect()
}

/// 解析单条 `<li class="comment-item">`
fn parse_one_comment(li: &ElementRef) -> Option<Comment> {
  // 评分: <span class="user-stars allstar50 allstar40 ..."> -> 取最高 allstarXX
  // 兜底选择器: span[class*="allstar"], span.user-stars
  let rating = parse_rating(li);

  // 内容: <span class="short">
  // 兜底选择器: span.short, p.comment-content
  let content = parse_content(li)?;

  // 有用数: <span class="vote-count"> 或 <span id="c-XXX" class="vote-count">
  // 兜底选择器: span.vote-count, span[id^="c-"]
  let usefulness = parse_usefulness(li);

  Some(Comment { rating, content, usefulness })
}

fn parse_rating(li: &ElementRef) -> Option<i32> {
  let sels = [
    Selector::parse(r#"span[class*="allstar"]"#).ok(),
    Selector::parse("span.user-stars").ok(),
  ];
  for sel in sels.into_iter().flatten() {
    if let Some(span) = li.select(&sel).next() {
      for class in span.value().classes() {
        if let Some(num) = class.strip_prefix("allstar") {
          if let Ok(n) = num.parse::<i32>() {
            return Some(n / 10);
          }
        }
      }
    }
  }
  None
}

fn parse_content(li: &ElementRef) -> Option<String> {
  let sels = [
    Selector::parse("span.short").ok(),
    Selector::parse("p.comment-content").ok(),
  ];
  for sel in sels.into_iter().flatten() {
    if let Some(elem) = li.select(&sel).next() {
      let text = normalize_ws(&elem.text().collect::<String>());
      if !text.is_empty() {
        return Some(text);
      }
    }
  }
  None
}

fn parse_usefulness(li: &ElementRef) -> i32 {
  let sels = [
    Selector::parse(r#"span.vote-count"#).ok(),
    Selector::parse(r#"span[id^="c-"]"#).ok(),
  ];
  for sel in sels.into_iter().flatten() {
    if let Some(elem) = li.select(&sel).next() {
      let text = elem.text().collect::<String>();
      let digits: String = text.chars().filter(|c| c.is_ascii_digit()).collect();
      if let Ok(n) = digits.parse::<i32>() {
        return n;
      }
    }
  }
  0
}

/// 质量过滤：忽略 <15 汉字的评论，按有用数降序排列，取前 5 条
/// 如果不足 5 条，放宽条件取最长的 5 条
pub fn filter_comments(raw: Vec<Comment>) -> Vec<Comment> {
  const TOP_N: usize = 5;

  // Step 1: 过滤短评
  let mut good: Vec<Comment> = raw
    .iter()
    .filter(|c| count_han(&c.content) >= 15)
    .cloned()
    .collect();

  // Step 2: 按有用数降序
  good.sort_by(|a, b| b.usefulness.cmp(&a.usefulness));

  // Step 3: 如果 >= 10 条，取前 10
  if good.len() >= TOP_N {
    return good.into_iter().take(TOP_N).collect();
  }

  // Step 4: 放宽 —— 从全部评论中取最长的 10 条
  warn!("高质量评论不足 {TOP_N} 条，放宽过滤条件");
  let mut all = raw.clone();
  all.sort_by(|a, b| count_han(&b.content).cmp(&count_han(&a.content)));
  all.into_iter().take(TOP_N).collect()
}

/// 将豆瓣书籍 URL 拼接为短评页 URL
pub fn douban_comments_url(book_url: &str) -> String {
  let base = book_url.trim_end_matches('/');
  format!("{base}/comments/")
}

/// 解析豆瓣书籍页 HTML，提取内容简介（段落以换行拼接）
///
/// 对应豆瓣书籍页内容简介区（`div.related_info` 内首个 `div.indent#link-report`）：
/// - 短简介：`<div class="indent" id="link-report"><div class="intro"><p>…</p></div></div>`
/// - 长简介（会被截短）：
///   `<span class="short"><div class="intro">…截短…(展开全部)</div></span>`
///   `<span class="all hidden"><div class="intro">…完整…</div></span>`
///   → 优先取 `span.all` 内的完整版（作者简介等后续区块也是同款结构，
///     因此必须限定在 `#link-report` 区内查找，不能全文档取第一个）。
/// 兜底: 全文档第一个 `div.intro`（页面改版丢 #link-report 时）→ og:description meta
pub fn parse_douban_description(html: &str) -> Option<String> {
  let document = Html::parse_document(html);

  // 内容简介区：`<div class="indent" id="link-report">`（书籍页内容简介容器）
  let sel_report = Selector::parse(r#"div#link-report"#).ok()?;
  let sel_intro = Selector::parse("div.intro").ok()?;
  // 长简介完整版：`#link-report` 内 `<span class="all hidden"><div class="intro">…</div></span>`
  let sel_all = Selector::parse("span.all div.intro").ok()?;

  let scope = document.select(&sel_report).next();
  let intro = scope
    .and_then(|s| s.select(&sel_all).next())
    // 短简介 / 无 span.all 结构：取区内第一个 div.intro（长简介截短版也在此，仅兜底）
    .or_else(|| scope.and_then(|s| s.select(&sel_intro).next()))
    // 页面结构漂移：回退全文档第一个 div.intro
    .or_else(|| document.select(&sel_intro).next());

  if let Some(intro) = intro {
    let sel_p = Selector::parse("p").ok()?;
    let text = intro
      .select(&sel_p)
      .map(|p| normalize_ws(&p.text().collect::<String>()))
      // 展开链接段落（“(展开全部)”）在结构漂移兜底路径下可能混入，防御性剔除
      .filter(|t| !t.is_empty() && t != "(展开全部)")
      .collect::<Vec<_>>()
      .join("\n");
    let text = text.trim().to_string();
    if !text.is_empty() {
      return Some(text);
    }
  }
  // 兜底: og:description meta
  let sel_meta = Selector::parse(r#"meta[property="og:description"]"#).ok()?;
  document
    .select(&sel_meta)
    .next()
    .and_then(|m| m.value().attr("content"))
    .map(|s| normalize_ws(s).trim().to_string())
    .filter(|s| !s.is_empty())
}

/// 从豆瓣书籍页解析出的条目元数据（来源展示与简介用）
#[derive(Debug, Clone, Default)]
pub struct DoubanBookMeta {
  pub title: Option<String>,
  pub author: Option<String>,
  pub cover_url: Option<String>,
  pub summary: Option<String>,
}

/// 解析豆瓣书籍页 HTML，提取书名 / 作者 / 封面 / 简介（来源管理展示用）
///
/// - 书名: og:title（兜底 <title> 去掉 " (豆瓣)" 后缀）
/// - 作者: #info 中 "作者" 字段后的链接/文本（多个以顿号拼接）
/// - 封面: og:image（兜底 #mainpic img）
/// - 简介: parse_douban_description
pub fn parse_douban_book_meta(html: &str) -> DoubanBookMeta {
  let document = Html::parse_document(html);
  let mut meta = DoubanBookMeta::default();

  // 书名: <meta property="og:title" content="白夜行">
  if let Ok(sel) = Selector::parse(r#"meta[property="og:title"]"#) {
    meta.title = document
      .select(&sel)
      .next()
      .and_then(|m| m.value().attr("content"))
      .map(|s| normalize_ws(s).trim().to_string())
      .filter(|s| !s.is_empty());
  }
  if meta.title.is_none() {
    // 兜底: <title>白夜行 (豆瓣)</title>
    if let Ok(sel) = Selector::parse("title") {
      meta.title = document
        .select(&sel)
        .next()
        .map(|t| t.text().collect::<String>())
        .map(|t| t.split(" (豆瓣)").next().unwrap_or("").trim().to_string())
        .filter(|s| !s.is_empty());
    }
  }

  // 作者: #info 内 "作者" 字段，值在其后的 <a> 或文本中
  if let (Ok(info_sel), Ok(span_sel), Ok(a_sel)) = (
    Selector::parse("div#info"),
    Selector::parse("span"),
    Selector::parse("a"),
  ) {
    let mut authors: Vec<String> = Vec::new();
    'outer: for info in document.select(&info_sel) {
      for span in info.select(&span_sel) {
        let text = span.text().collect::<String>();
        let t = text.trim();
        if t.starts_with("作者") {
          // span 文本形如 "作者: 东野圭吾" 或含多个 <a>
          for a in span.select(&a_sel) {
            let name = normalize_ws(&a.text().collect::<String>()).trim().to_string();
            if !name.is_empty() && !authors.contains(&name) {
              authors.push(name);
            }
          }
          if authors.is_empty() {
            // 无 <a> 时从纯文本冒号后取值
            if let Some(v) = t.split([':', '：']).nth(1) {
              let name = normalize_ws(v)
                .trim()
                .trim_end_matches("更多...")
                .trim()
                .to_string();
              if !name.is_empty() {
                authors.push(name);
              }
            }
          }
          break 'outer;
        }
      }
    }
    meta.author = (!authors.is_empty()).then(|| authors.join("、"));
  }

  // 封面: og:image，兜底 #mainpic img
  if let Ok(sel) = Selector::parse(r#"meta[property="og:image"]"#) {
    meta.cover_url = document
      .select(&sel)
      .next()
      .and_then(|m| m.value().attr("content"))
      .map(|s| s.trim().to_string())
      .filter(|s| !s.is_empty());
  }
  if meta.cover_url.is_none() {
    if let Ok(sel) = Selector::parse(r#"div#mainpic img"#) {
      meta.cover_url = document
        .select(&sel)
        .next()
        .and_then(|img| img.value().attr("src"))
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    }
  }

  meta.summary = parse_douban_description(html);
  meta
}

/// 抓取 claspclub 书籍页的用户短评（第一页，按热门排序；失败仅告警跳过）
///
/// 对应 GET https://claspclub.com/api/v1/books/{id}/comments?area=all&page=1&pageSize=12&sort=popular
/// 响应格式与搜索接口类似（items 数组 + pagination）；字段解析做防御式兼容，
/// 复用豆瓣短评同款过滤（≥15 汉字，按有用数 top5）。
pub async fn fetch_clasp_reviews(
  client: &reqwest::Client,
  clasp_id: &str,
) -> anyhow::Result<Vec<Comment>> {
  let url = format!(
    "https://claspclub.com/api/v1/books/{clasp_id}/comments?area=all&page=1&pageSize=12&sort=popular"
  );
  let resp = client.get(&url).send().await?;
  if !resp.status().is_success() {
    anyhow::bail!("claspclub 评论接口返回 HTTP {}", resp.status());
  }
  let value: serde_json::Value = resp.json().await?;

  let items = value
    .get("items")
    .or_else(|| value.get("comments"))
    .or_else(|| value.get("data"))
    .and_then(|v| v.as_array())
    .or_else(|| value.as_array())
    .cloned()
    .unwrap_or_default();

  let get_str = |item: &serde_json::Value, keys: &[&str]| -> Option<String> {
    keys.iter().find_map(|k| {
      item
        .get(k)
        .and_then(|v| v.as_str())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    })
  };
  let get_num = |item: &serde_json::Value, keys: &[&str]| -> Option<i32> {
    keys.iter().find_map(|k| {
      item
        .get(k)
        .and_then(|v| v.as_i64())
        .map(|n| n.clamp(0, i32::MAX as i64) as i32)
    })
  };

  let raw = items
    .iter()
    .filter_map(|item| {
      let content = get_str(item, &["content", "body", "text", "review", "comment"])?;
      let usefulness = get_num(item, &["likeCount", "likes", "usefulCount", "voteCount"]).unwrap_or(0);
      // 评分兼容 5 分制 / 10 分制：>5 视为 10 分制折算
      let rating = get_num(item, &["rating", "score", "stars"]).map(|r| if r > 5 { r / 2 } else { r });
      Some(Comment { rating, content, usefulness })
    })
    .collect();

  Ok(raw)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn online_douban_parsers_preserve_metadata_and_reject_unknown_pages() {
    let comments = parse_douban_discussions(r#"<h1>示例书 短评</h1><div id="comments">
      <li class="comment-item" data-cid="12"><span class="comment-info"><a>读者甲</a>
      <span class="allstar40"></span><a class="comment-time">2026-01-01</a></span>
      <span class="short">文字<b>评价</b>&amp;感想<script>忽略</script></span><span class="vote-count">42</span></li>
      </div><div id="paginator"><a data-page="next" href="?start=20">后页</a></div>"#, false).unwrap();
    assert_eq!(comments.book_title.as_deref(), Some("示例书"));
    assert_eq!(comments.items[0]["content"], "文字评价&感想");
    assert_eq!(comments.items[0]["rating"], 4);
    assert_eq!(comments.items[0]["usefulness"], 42);
    assert!(comments.has_next);
    let reviews = parse_douban_discussions(r#"<h1>示例书的书评 (2)</h1><div class="review-list">
      <div class="main review-item" id="99"><header><a class="name">读者乙</a><span class="main-meta">2026-02-02</span></header>
      <h2><a href="/review/99/">文章标题</a></h2><div class="short-content"><p class="spoiler-tip">剧透</p>
      正文摘要<a class="unfold">展开</a></div><div class="action"><a class="up">9</a><a class="down">2</a></div></div>
      </div><div class="paginator"><link rel="next" href="?start=20"></div>"#, true).unwrap();
    assert_eq!(reviews.book_title.as_deref(), Some("示例书"));
    assert_eq!(reviews.items[0]["id"], "99");
    assert_eq!(reviews.items[0]["title"], "文章标题");
    assert_eq!(reviews.items[0]["content"], "正文摘要");
    assert_eq!(reviews.items[0]["spoiler"], true);
    assert!(reviews.has_next);
    assert!(parse_douban_discussions("<h1>安全验证</h1>", false).is_err());
    assert!(parse_douban_discussions("<div class='review-list'></div>", true).unwrap().items.is_empty());
    let fallback = parse_douban_discussions("<li data-cid='3'><p class='comment-content'>旧结构</p></li>", false).unwrap();
    assert_eq!(fallback.items[0]["content"], "旧结构");
  }

  #[test]
  fn full_review_parser_strips_html_and_preserves_paragraphs_and_spoiler() {
    let data: DoubanFullReviewResponse = serde_json::from_value(serde_json::json!({
      "html":"<p>第一段<b>文字</b></p>第二段<br>第三段 &amp; 结语<script>忽略</script><img src='https://img.doubanio.com/test.jpg'>",
      "body":"<div class='main-title-tip'>剧透</div><div class='review-content' data-author='作者甲'>正文</div>",
      "votes":{"useful_count":9,"useless_count":1}
    })).unwrap();
    let result = parse_douban_full_review(data).unwrap();
    assert_eq!(result["content"], "第一段文字\n第二段\n第三段 & 结语");
    assert_eq!(result["author"], "作者甲");
    assert_eq!(result["spoiler"], true);
    assert_eq!(result["usefulness"], 9);
    assert_eq!(result["image_urls"][0], "https://img.doubanio.com/test.jpg");
    let fallback: DoubanFullReviewResponse = serde_json::from_value(serde_json::json!({"body":"<div class='review-content'>旧版全文</div>"})).unwrap();
    assert_eq!(parse_douban_full_review(fallback).unwrap()["content"], "旧版全文");
    let blocked: DoubanFullReviewResponse = serde_json::from_value(serde_json::json!({"body":"请登录"})).unwrap();
    assert!(parse_douban_full_review(blocked).is_err());
  }

  #[test]
  fn test_extract_douban_id() {
    let url = "https://clasp-book-images.oss-cn-hangzhou.aliyuncs.com/book-covers/douban-26771719.jpg";
    assert_eq!(
      extract_douban_id_from_cover(url),
      Some("26771719".to_string())
    );

    assert_eq!(extract_douban_id_from_cover("https://example.com/no-douban.jpg"), None);
  }

  /// 短评过滤：≥15 汉字 + 按有用数降序取 5 条；不足时放宽取最长 5 条
  #[test]
  fn test_filter_comments_top5() {
    let mk = |n: usize, han: usize, useful: i32| Comment {
      rating: None,
      content: "好".repeat(han) + &format!("填充{n}"),
      usefulness: useful,
    };
    // 12 条高质量评论 → 只取有用数最高的 5 条
    let raw: Vec<Comment> = (0..12).map(|i| mk(i, 20, i as i32)).collect();
    let filtered = filter_comments(raw);
    assert_eq!(filtered.len(), 5);
    assert_eq!(filtered[0].usefulness, 11);
    assert_eq!(filtered[4].usefulness, 7);

    // 高质量不足 → 放宽取最长 5 条
    let raw: Vec<Comment> = (0..12).map(|i| mk(i, if i < 2 { 20 } else { 3 }, 0)).collect();
    let filtered = filter_comments(raw);
    assert_eq!(filtered.len(), 5);
    // 两条高质量评论排最前
    assert!(count_han(&filtered[0].content) >= 20);
  }

  #[test]
  fn test_douban_urls() {
    assert_eq!(
      douban_book_url("26771719"),
      "https://book.douban.com/subject/26771719/"
    );
    assert_eq!(
      douban_comments_url("https://book.douban.com/subject/26771719/"),
      "https://book.douban.com/subject/26771719/comments/"
    );
  }

  /// 豆瓣简介解析：div.intro 段落拼接；缺失时兜底 og:description
  #[test]
  fn test_parse_douban_description() {
    let html = r#"<html><body><div id="intro"><div class="intro"><p>第一段简介。</p><p>第二段介绍。</p></div></div></body></html>"#;
    assert_eq!(
      parse_douban_description(html).as_deref(),
      Some("第一段简介。\n第二段介绍。")
    );

    let fallback = r#"<html><head><meta property="og:description" content="兜底简介内容"/></head><body></body></html>"#;
    assert_eq!(
      parse_douban_description(fallback).as_deref(),
      Some("兜底简介内容")
    );

    assert!(parse_douban_description("<html><body></body></html>").is_none());
  }

  /// 豆瓣长简介解析：#link-report 内 span.short（截短）+ span.all hidden（完整），
  /// 必须取完整版；且不能串到作者简介区（同款 short/all 结构）
  #[test]
  fn test_parse_douban_description_long() {
    // 结构来源：https://book.douban.com/subject/30354903/
    // 内容简介长文被豆瓣截短，完整版在 span.all hidden 内；作者简介区有同款结构
    let html = r#"<html><body>
      <div class="related_info">
        <h2><span>内容简介</span></h2>
        <div class="indent" id="link-report">
          <span class="short">
            <div class="intro">
              <p>第一条营销文案。</p>
              <p>第二条营销文案被截短...</p>
              <p><a href="javascript:void(0)" class="j a_show_full">(展开全部)</a></p>
            </div>
          </span>
          <span class="all hidden">
            <div class="intro">
              <p>第一条营销文案。</p>
              <p>第二条营销文案的完整内容。</p>
              <p>内容简介正文段落。</p>
            </div>
          </span>
        </div>
        <h2><span>作者简介</span></h2>
        <div class="indent">
          <span class="short"><div class="intro"><p>作者简介截短版。</p></div></span>
          <span class="all hidden"><div class="intro"><p>作者简介完整版，绝不能被当成内容简介。</p></div></span>
        </div>
      </div>
    </body></html>"#;
    assert_eq!(
      parse_douban_description(html).as_deref(),
      Some("第一条营销文案。\n第二条营销文案的完整内容。\n内容简介正文段落。")
    );
  }

  /// 豆瓣短简介解析：#link-report 内直接是 div.intro（无 short/all 结构），
  /// 同样不得串到作者简介区
  #[test]
  fn test_parse_douban_description_short() {
    // 结构来源：https://book.douban.com/subject/26771719/
    let html = r#"<html><body>
      <div class="related_info">
        <h2><span>内容简介</span></h2>
        <div class="indent" id="link-report">
          <div class="intro"><p>钟表馆的完整内容简介，篇幅不长未被截短。</p></div>
        </div>
        <h2><span>作者简介</span></h2>
        <div class="indent">
          <span class="short"><div class="intro"><p>作者简介截短版。</p></div></span>
          <span class="all hidden"><div class="intro"><p>作者简介完整版，绝不能被当成内容简介。</p></div></span>
        </div>
      </div>
    </body></html>"#;
    assert_eq!(
      parse_douban_description(html).as_deref(),
      Some("钟表馆的完整内容简介，篇幅不长未被截短。")
    );
  }

  /// searchMode=fuzzy 反序列化
  #[test]
  fn test_search_mode_fuzzy() {
    let resp: ClaspBooksResp =
      serde_json::from_str(r#"{"items": [], "searchMode": "fuzzy"}"#).unwrap();
    assert_eq!(resp.search_mode.as_deref(), Some("fuzzy"));

    let exact: ClaspBooksResp = serde_json::from_str(r#"{"items": []}"#).unwrap();
    assert!(exact.search_mode.is_none());
  }

  /// 系列字段锁定真实 API 格式：series 为 {description,id,name,order}，单行本为 null；
  /// 卷号兼容整数/浮点/字符串，异常值降级为 None
  #[test]
  fn test_clasp_series_real_format() {
    // 实测响应片段（钟表馆事件，馆系列第 5 卷）
    let detail: ClaspBookDetail = serde_json::from_str(
      r#"{
        "title": "钟表馆事件",
        "series": {
          "description": "馆系列是日本新本格推理的代表作。",
          "id": "cmpuvekk50007til5qtacyljn",
          "name": "馆系列",
          "order": 5
        }
      }"#,
    )
    .unwrap();
    let s = detail.series.expect("series 应被解析");
    assert_eq!(s.name.as_deref(), Some("馆系列"));
    assert_eq!(s.order, Some(5));

    // 单行本（白夜行）：series 为 null
    let standalone: ClaspBookDetail =
      serde_json::from_str(r#"{"title": "白夜行", "series": null}"#).unwrap();
    assert!(standalone.series.is_none());

    // 卷号格式漂移：浮点 / 字符串 / 缺失 / 垃圾值 → 均不阻断解析
    let drift: ClaspBookDetail =
      serde_json::from_str(r#"{"series": {"name": "A", "order": 3.0}}"#).unwrap();
    assert_eq!(drift.series.unwrap().order, Some(3));
    let drift: ClaspBookDetail =
      serde_json::from_str(r#"{"series": {"name": "A", "order": "7"}}"#).unwrap();
    assert_eq!(drift.series.unwrap().order, Some(7));
    let drift: ClaspBookDetail =
      serde_json::from_str(r#"{"series": {"name": "A"}}"#).unwrap();
    assert_eq!(drift.series.unwrap().order, None);
    let drift: ClaspBookDetail =
      serde_json::from_str(r#"{"series": {"name": "A", "order": [1]}}"#).unwrap();
    assert_eq!(drift.series.unwrap().order, None);
  }

  #[test]
  fn test_from_suggestion() {
    let s = ClaspBookSuggestion {
      title: "钟表馆事件".into(),
      author_name: "绫辻行人".into(),
      id: "cmq0a3te5000tbhl5sydiqlaa".into(),
      tags: vec!["本格推理".into(), "密室".into()],
      cover_url: Some(
        "https://clasp-book-images.oss-cn-hangzhou.aliyuncs.com/book-covers/douban-26771719.jpg"
          .into(),
      ),
      summary: None,
      douban_url: None,
    };

    let meta = ClaspBookMeta::from_suggestions(std::slice::from_ref(&s));
    assert_eq!(meta.title, "钟表馆事件");
    assert_eq!(meta.author, "绫辻行人");
    assert_eq!(meta.tags, vec!["本格推理", "密室"]);
    assert_eq!(
      meta.douban_urls.first().map(|s| s.as_str()),
      Some("https://book.douban.com/subject/26771719/")
    );
    assert_eq!(meta.clasp_ids, vec!["cmq0a3te5000tbhl5sydiqlaa"]);
  }

  /// 分页搜索条目 → 统一建议结构：作者/标签取自嵌套对象，豆瓣链接从封面推断
  #[test]
  fn test_book_item_to_suggestion() {
    let item = ClaspBookItem {
      id: "cmq0a3te5000tbhl5sydiqlaa".into(),
      title: "钟表馆事件".into(),
      author: None,
      authors: vec![ClaspAuthor { name: "绫辻行人".into() }],
      cover_url: Some(
        "https://clasp-book-images.oss-cn-hangzhou.aliyuncs.com/book-covers/douban-26771719.jpg"
          .into(),
      ),
      summary_no_spoiler: Some("无剧透简介".into()),
      tags: vec![
        ClaspTag { name: "本格推理".into() },
        ClaspTag { name: "密室".into() },
      ],
      douban_rating: Some(8.5),
    };

    let s = ClaspBookSuggestion::from(&item);
    assert_eq!(s.title, "钟表馆事件");
    assert_eq!(s.author_name, "绫辻行人");
    assert_eq!(s.tags, vec!["本格推理", "密室"]);
    assert_eq!(s.summary.as_deref(), Some("无剧透简介"));
    assert_eq!(
      s.douban_url.as_deref(),
      Some("https://book.douban.com/subject/26771719/")
    );

    // 无 authors 列表时回退 author 字段
    let item2 = ClaspBookItem {
      authors: vec![],
      author: Some(ClaspAuthor { name: "东野圭吾".into() }),
      ..item
    };
    assert_eq!(ClaspBookSuggestion::from(&item2).author_name, "东野圭吾");
  }
}
