use serde::{Deserialize, Serialize};
use thiserror::Error;

/// OpenAI 兼容 Chat Completion 的消息体
///
/// `tool_calls` / `tool_call_id` 用于 function calling 的 agent 循环
/// （assistant 消息携带 tool_calls，tool 消息以 tool_call_id 对应结果）
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Message {
  pub role: String,
  pub content: String,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub tool_calls: Option<Vec<serde_json::Value>>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub tool_call_id: Option<String>,
}

impl Message {
  pub fn system(content: impl Into<String>) -> Self {
    Self { role: "system".into(), content: content.into(), tool_calls: None, tool_call_id: None }
  }
  pub fn user(content: impl Into<String>) -> Self {
    Self { role: "user".into(), content: content.into(), tool_calls: None, tool_call_id: None }
  }
  pub fn assistant(content: impl Into<String>) -> Self {
    Self { role: "assistant".into(), content: content.into(), tool_calls: None, tool_call_id: None }
  }
}

/// LLM 配置，从设置（config.toml `llm` 段，多 Provider）或环境变量解析
pub struct LlmConfig {
  /// Provider 显示名（token 用量按 "{provider}/{model}" 累计）
  pub provider: String,
  pub api_key: String,
  pub base_url: String,
  pub model: String,
}

impl LlmConfig {
  /// 从环境变量构建配置；若未设置 OPENAI_API_KEY 则返回 None
  pub fn from_env() -> Option<Self> {
    let api_key = std::env::var("OPENAI_API_KEY").ok()?;
    if api_key.trim().is_empty() {
      return None;
    }
    Some(Self {
      provider: "env".into(),
      api_key,
      base_url: std::env::var("OPENAI_BASE_URL")
        .unwrap_or_else(|_| "https://api.openai.com/v1".into()),
      model: std::env::var("MODEL_NAME").unwrap_or_else(|_| "gpt-3.5-turbo".into()),
    })
  }

  /// 设置优先解析：config.toml `llm` 段（多 Provider）→ 环境变量兜底
  ///
  /// - Provider：default_provider 精确匹配（其 api_key 为空视为配置错误返回
  ///   None）→ 第一个 api_key 非空的 → 第一个；provider 列表为空时回退环境变量
  /// - 模型：default_model（须属于该 provider）→ 该 provider 的 models[0]
  ///   → MODEL_NAME → gpt-3.5-turbo
  pub fn resolve(settings: &crate::config::LlmSettings) -> Option<Self> {
    if settings.providers.is_empty() {
      return Self::from_env();
    }

    let provider = match settings.default_provider.as_deref() {
      Some(n) => settings.providers.iter().find(|p| p.name == n),
      None => None,
    };
    let provider = provider
      .or_else(|| {
        settings
          .providers
          .iter()
          .find(|p| !p.api_key.trim().is_empty())
      })
      .or_else(|| settings.providers.first())?;

    let api_key = provider.api_key.trim();
    if api_key.is_empty() {
      return None;
    }
    let base_url = provider.base_url.trim();
    if base_url.is_empty() {
      return None;
    }

    let model = settings
      .default_model
      .as_deref()
      .filter(|m| provider.models.iter().any(|x| x == m))
      .map(str::to_string)
      .or_else(|| {
        provider
          .models
          .first()
          .map(|m| m.trim().to_string())
          .filter(|m| !m.is_empty())
      })
      .unwrap_or_else(|| std::env::var("MODEL_NAME").unwrap_or_else(|_| "gpt-3.5-turbo".into()));

    Some(Self {
      provider: provider.name.clone(),
      api_key: api_key.to_string(),
      base_url: base_url.to_string(),
      model,
    })
  }
}

/// 从应用配置解析 LLM 配置（设置页 → 环境变量兜底）
pub fn resolve_config() -> Option<LlmConfig> {
  LlmConfig::resolve(&crate::config::AppConfig::load().llm)
}

/// 一次 LLM 调用的 token 用量（API 未返回 usage 时全 0）
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct TokenUsage {
  pub prompt_tokens: u64,
  pub completion_tokens: u64,
  pub total_tokens: u64,
}

/// chat 的结果：回复文本 + 所用模型 + token 用量
#[derive(Debug, Clone)]
pub struct ChatOutcome {
  pub content: String,
  pub model: String,
  pub usage: TokenUsage,
  /// 模型请求的工具调用（function calling；无调用时为 None）
  pub tool_calls: Option<Vec<serde_json::Value>>,
}

/// 简介融合结果（单条简介或未配置 LLM 时不产生用量）
#[derive(Debug, Clone)]
pub struct DescriptionFusion {
  pub text: String,
  /// 走了 LLM 时记录（模型, 用量），供 token 统计入库
  pub llm: Option<(String, TokenUsage)>,
  /// 降级原因（如未配置 LLM）；None 表示未降级
  /// （LLM 调用失败时走 Err 分支，由调用方记录原因）
  pub degraded: Option<String>,
}

/// LLM 调用错误
#[derive(Error, Debug)]
pub enum LlmError {
  #[error("网络错误: {0}")]
  Network(#[from] reqwest::Error),
  #[error("API 错误: {0}")]
  Api(String),
  #[error("解析响应失败: {0}")]
  Parse(String),
  /// token 预算已耗尽（不可重试；调整预算或清零用量后恢复）
  #[error("预算限制: {0}")]
  Budget(String),
}

impl LlmError {
  /// 是否值得重试：网络错误与 HTTP 429/5xx（限流/临时故障）；
  /// 其余（参数错误、鉴权失败等 4xx、预算超限）重试无意义
  pub fn is_retryable(&self) -> bool {
    match self {
      LlmError::Network(_) => true,
      LlmError::Api(msg) => {
        let s = msg.trim_start();
        s.starts_with("HTTP 429") || s.starts_with("HTTP 5")
      }
      LlmError::Parse(_) => false,
      LlmError::Budget(_) => false,
    }
  }
}

// ---- 内部请求/响应结构 ----

#[derive(Serialize)]
struct ChatRequest {
  model: String,
  messages: Vec<Message>,
  temperature: f32,
  #[serde(skip_serializing_if = "Option::is_none")]
  stream: Option<bool>,
  #[serde(skip_serializing_if = "Option::is_none")]
  tools: Option<Vec<serde_json::Value>>,
}

#[derive(Deserialize)]
struct ChatResponse {
  choices: Vec<ChatChoice>,
  #[serde(default)]
  usage: Option<ApiUsage>,
}

#[derive(Deserialize)]
struct ChatChoice {
  message: Message,
}

/// OpenAI 兼容 usage 字段（部分兼容 API 可能省略，缺省按 0 处理）
#[derive(Deserialize)]
struct ApiUsage {
  #[serde(default)]
  prompt_tokens: u64,
  #[serde(default)]
  completion_tokens: u64,
  #[serde(default)]
  total_tokens: u64,
}

/// 调用 OpenAI 兼容的 Chat Completion API（不带工具；带失败重试）
pub async fn chat(config: &LlmConfig, messages: &[Message]) -> Result<ChatOutcome, LlmError> {
  chat_with_tools(config, messages, None).await
}

/// 调用 OpenAI 兼容的 Chat Completion API（带失败重试，可挂载工具）
///
/// - 超时 30 秒；重试次数取设置 `llm.retry_count`（默认 2，最大 10）
/// - 仅对可重试错误（网络错误 / 429 / 5xx）重试，指数退避
/// - `tools`：OpenAI tools 参数原始 JSON（function calling）
/// - 返回回复文本（可能为空）+ 工具调用请求 + 所用模型 + token 用量
pub async fn chat_with_tools(
  config: &LlmConfig,
  messages: &[Message],
  tools: Option<&[serde_json::Value]>,
) -> Result<ChatOutcome, LlmError> {
  chat_with_tools_timeout(config, messages, tools, std::time::Duration::from_secs(30)).await
}

/// 同 [`chat_with_tools`]，但可指定单次请求的整体超时
///
/// 书虫 agent 循环应传更长超时（如 120 秒）：大书库的系统提示可达数十 KB，
/// 生成耗时随提示规模上涨，30 秒极易触发 reqwest 超时
/// （表现为「网络错误: error sending request for url (…/chat/completions)」）。
pub async fn chat_with_tools_timeout(
  config: &LlmConfig,
  messages: &[Message],
  tools: Option<&[serde_json::Value]>,
  timeout: std::time::Duration,
) -> Result<ChatOutcome, LlmError> {
  let app_cfg = crate::config::AppConfig::load();
  // 预算检查（所有 LLM 调用的统一咽喉：chat/融合/翻译/书虫循环都经过这里）。
  // 用量按次入库，多轮 agent 每轮调用前都会重新检查 → 达到预算自动中断。
  // 数据库不可读时按 0 用量放行（统计故障不应阻断功能）。
  if let Some(msg) = app_cfg.llm.budget_error(crate::db::llm_usage_total_at(
    &app_cfg.database_file(),
  )) {
    return Err(LlmError::Budget(msg));
  }
  let retries = app_cfg
    .llm
    .retry_count
    .unwrap_or(crate::config::DEFAULT_LLM_RETRY)
    .min(10) as usize;
  let mut attempt = 0usize;
  loop {
    match chat_once(config, messages, tools, timeout).await {
      Ok(outcome) => return Ok(outcome),
      Err(e) if e.is_retryable() && attempt < retries => {
        attempt += 1;
        tracing::warn!("LLM 调用失败，第 {attempt}/{retries} 次重试: {e}");
        tokio::time::sleep(std::time::Duration::from_millis(600 * attempt as u64)).await;
      }
      Err(e) => return Err(e),
    }
  }
}

/// chat 的单次尝试（不重试）
async fn chat_once(
  config: &LlmConfig,
  messages: &[Message],
  tools: Option<&[serde_json::Value]>,
  timeout: std::time::Duration,
) -> Result<ChatOutcome, LlmError> {
  let client = reqwest::Client::builder().timeout(timeout).build()?;

  let req = ChatRequest {
    model: config.model.clone(),
    messages: messages.to_vec(),
    temperature: 0.8,
    stream: Some(false),
    tools: tools.map(|t| t.to_vec()),
  };

  let resp = client
    .post(format!("{}/chat/completions", config.base_url))
    .bearer_auth(&config.api_key)
    .json(&req)
    .send()
    .await?;

  if !resp.status().is_success() {
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    return Err(LlmError::Api(format!("HTTP {status}: {body}")));
  }

  let chat_resp: ChatResponse = resp.json().await?;
  let choice = chat_resp
    .choices
    .into_iter()
    .next()
    .ok_or_else(|| LlmError::Parse("响应中没有 choices".into()))?;
  let usage = chat_resp.usage.map(|u| TokenUsage {
    prompt_tokens: u.prompt_tokens,
    completion_tokens: u.completion_tokens,
    total_tokens: u.total_tokens,
  });
  Ok(ChatOutcome {
    content: choice.message.content,
    tool_calls: choice.message.tool_calls,
    // 用量统计与展示使用 "provider/model"，多服务商下可区分同名模型
    model: format!("{}/{}", config.provider, config.model),
    usage: usage.unwrap_or_default(),
  })
}

/// 将若干本书的简介融合为一段连贯简介（用于合并本 / 系列合并）
///
/// - 单条简介时原样返回（不调 LLM，无用量，未降级）
/// - 未配置 LLM 时降级为用分隔符拼接，`degraded` 携带原因（无用量）
/// - LLM 调用成功时回传模型与 token 用量，供调用方入库统计
/// - LLM 调用失败时返回 Err，由调用方降级拼接并向用户展示原因
pub async fn combine_descriptions(descriptions: &[String]) -> Result<DescriptionFusion, LlmError> {
  // 过滤空简介
  let descs: Vec<&str> = descriptions
    .iter()
    .map(|s| s.trim())
    .filter(|s| !s.is_empty())
    .collect();

  if descs.is_empty() {
    return Ok(DescriptionFusion { text: String::new(), llm: None, degraded: None });
  }
  if descs.len() == 1 {
    return Ok(DescriptionFusion { text: descs[0].to_string(), llm: None, degraded: None });
  }

  // 未配置 LLM → 降级拼接（原因显式回传，供 GUI/CLI 提示）
  let Some(config) = resolve_config() else {
    let reason = "未配置 LLM（请在设置页填写 Endpoint / API Key / 模型，或设置 OPENAI_API_KEY）".to_string();
    tracing::warn!("简介融合降级: {reason}");
    return Ok(DescriptionFusion {
      text: descs
        .iter()
        .enumerate()
        .map(|(i, d)| format!("【第{}部】{d}", i + 1))
        .collect::<Vec<_>>()
        .join("\n\n"),
      llm: None,
      degraded: Some(reason),
    });
  };

  let numbered = descs
    .iter()
    .enumerate()
    .map(|(i, d)| format!("【第{}部】\n{d}", i + 1))
    .collect::<Vec<_>>()
    .join("\n\n");

  let prompt = format!(
    "以下是一部合订本/系列合集中包含的 {} 本书的简介。\
     请把它们融合成一段 150~300 字的连贯简介：\
     概括整部作品的整体内容，说明各部分之间的关系，\
     保留最具代表性的情节元素与氛围描写。\
     不要逐书罗列，不要使用标题或编号，直接输出简介正文。\n\n{numbered}",
    descs.len()
  );

  let messages = vec![Message::user(prompt)];
  let outcome = chat(&config, &messages).await?;
  Ok(DescriptionFusion {
    text: outcome.content,
    llm: Some((outcome.model, outcome.usage)),
    degraded: None,
  })
}

/// 将非中文文本翻译为简体中文（LLM 调用链自带失败重试）
///
/// 未配置 LLM 或调用失败时返回 Err，由调用方降级（保留原文并提示）。
pub async fn translate_to_chinese(text: &str) -> Result<ChatOutcome, LlmError> {
  let Some(config) = resolve_config() else {
    return Err(LlmError::Api(
      "未配置 LLM（请在设置页填写 Endpoint / API Key / 模型，或设置 OPENAI_API_KEY）".into(),
    ));
  };
  let messages = vec![
    Message::system(
      "你是专业译者。将用户提供的文本翻译为流畅的简体中文，只输出译文正文，不要任何解释、注释或前后缀。",
    ),
    Message::user(text.to_string()),
  ];
  chat(&config, &messages).await
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::config::{LlmProvider, LlmSettings};

  fn provider(name: &str, key: &str, base: &str, models: &[&str]) -> LlmProvider {
    LlmProvider {
      name: name.into(),
      base_url: base.into(),
      api_key: key.into(),
      models: models.iter().map(|m| m.to_string()).collect(),
    }
  }

  /// 设置优先解析：默认 provider + 默认模型精确命中
  #[test]
  fn test_resolve_settings_priority() {
    let s = LlmSettings {
      providers: vec![
        provider("OpenAI", "sk-a", "https://a.example.com/v1", &["gpt-4o", "gpt-4o-mini"]),
        provider("DeepSeek", "sk-b", "https://b.example.com/v1", &["deepseek-chat"]),
      ],
      default_provider: Some("DeepSeek".into()),
      default_model: Some("deepseek-chat".into()),
      ..Default::default()
    };
    let cfg = LlmConfig::resolve(&s).unwrap();
    assert_eq!(cfg.provider, "DeepSeek");
    assert_eq!(cfg.api_key, "sk-b");
    assert_eq!(cfg.model, "deepseek-chat");
  }

  /// default_model 缺省时回退该 provider 的 models[0]；用量键含 provider 前缀
  #[test]
  fn test_resolve_model_fallback() {
    let s = LlmSettings {
      providers: vec![provider("OpenAI", "sk-a", "https://a.example.com/v1", &["gpt-4o-mini"])],
      ..Default::default()
    };
    let cfg = LlmConfig::resolve(&s).unwrap();
    assert_eq!(cfg.provider, "OpenAI");
    assert_eq!(cfg.model, "gpt-4o-mini");
    assert!(!cfg.base_url.is_empty());
  }

  /// 无默认 provider 时选第一个有 key 的；默认 provider 的 key 为空视为配置错误
  #[test]
  fn test_resolve_skip_empty_key() {
    let s = LlmSettings {
      providers: vec![
        provider("A", "  ", "https://a.example.com/v1", &["m1"]),
        provider("B", "sk-b", "https://b.example.com/v1", &["m2"]),
      ],
      ..Default::default()
    };
    let cfg = LlmConfig::resolve(&s).unwrap();
    assert_eq!(cfg.provider, "B");

    // 显式指定默认 provider 但 key 为空 → None（配置错误显式暴露）
    let s2 = LlmSettings {
      providers: vec![provider("A", "", "https://a.example.com/v1", &["m1"])],
      default_provider: Some("A".into()),
      ..Default::default()
    };
    assert!(LlmConfig::resolve(&s2).is_none());
  }

  /// provider 列表为空 → 回退环境变量（未设置时 None）
  #[test]
  fn test_resolve_env_fallback() {
    let s = LlmSettings::default();
    if std::env::var("OPENAI_API_KEY").map(|v| v.trim().is_empty()).unwrap_or(true) {
      assert!(LlmConfig::resolve(&s).is_none());
    } else {
      assert!(LlmConfig::resolve(&s).is_some());
    }
  }

  /// 预算检查：达到/超过预算返回错误信息，未达或未配置预算放行
  #[test]
  fn test_budget_error() {
    let s = LlmSettings { budget_tokens: Some(1000), ..Default::default() };
    assert!(s.budget_error(999).is_none());
    let msg = s.budget_error(1000).unwrap();
    assert!(msg.contains("1000"), "信息应含预算值: {msg}");
    assert!(s.budget_error(5000).is_some());

    // 未配置预算 / 预算非正数 → 永不放行拦截
    let s2 = LlmSettings::default();
    assert!(s2.budget_error(i64::MAX).is_none());
    let s3 = LlmSettings { budget_tokens: Some(0), ..Default::default() };
    assert!(s3.budget_error(99999).is_none());
  }

  /// 预算错误不可重试（重试只会继续撞墙）
  #[test]
  fn test_budget_error_not_retryable() {
    let e = LlmError::Budget("已达上限".into());
    assert!(!e.is_retryable());
  }

  /// 模型价格解析：显式配置精确匹配 > 预设最长前缀匹配；未知模型不计成本
  #[test]
  fn test_price_for() {
    use crate::config::ModelPricing;
    let s = LlmSettings {
      pricing: vec![ModelPricing {
        model: "deepseek-chat".into(),
        input_per_m: 1.5,
        output_per_m: 6.0,
      }],
      ..Default::default()
    };
    // 显式配置覆盖预设
    assert_eq!(s.price_for("deepseek-chat"), Some((1.5, 6.0)));
    // 用量统计键（provider/model）与裸模型名等价
    assert_eq!(s.price_for("DeepSeek/deepseek-chat"), Some((1.5, 6.0)));
    // 预设表：最长前缀命中（gpt-4o-2024-08-06 → gpt-4o，而非 gpt-4）
    assert_eq!(s.price_for("gpt-4o-2024-08-06"), Some((18.0, 72.0)));
    assert_eq!(s.price_for("OpenAI/gpt-4o-mini"), Some((1.1, 4.3)));
    // 未知模型 → None（不计成本）
    assert_eq!(s.price_for("my-private-model"), None);
  }
}
