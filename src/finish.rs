use dialoguer::{Input, Select};
use thiserror::Error;
use tracing::{info, warn};

use crate::agent::{self, LlmConfig, Message};
use crate::db;

/// finish 模块错误枚举
#[derive(Error, Debug)]
pub enum FinishError {
  #[error("数据库错误: {0}")]
  DbError(#[from] rusqlite::Error),

  #[error("LLM 错误: {0}")]
  LlmError(String),

  #[error("用户取消了操作")]
  UserAbort,

  #[error("IO 错误: {0}")]
  IoError(#[from] std::io::Error),

  #[error("{0}")]
  Other(String),
}

// ============================= //
//  公共入口
// ============================= //

/// 标记读完 + 苏格拉底式 AI 对话 + 书评生成入库
pub async fn finish_book(conn: &rusqlite::Connection) -> Result<(), FinishError> {
  // ---- 阶段一：书籍选择与状态校验 ----
  let books = db::get_unfinished_books(conn)?;
  if books.is_empty() {
    println!("没有正在阅读的书籍，请先用 `add` 命令导入。");
    return Ok(());
  }

  let items: Vec<String> = books
    .iter()
    .map(|b| format!("《{}》- {}", b.title, b.author))
    .collect();

  let sel = Select::new()
    .with_prompt("选择要标记为已读的书籍")
    .items(&items)
    .default(0)
    .interact()
    .map_err(io_abort)?;

  let book_id = books[sel].id;
  let book_title = books[sel].title.clone();

  // 立即更新状态为已读，记录时间戳
  db::mark_book_finished(conn, book_id)?;
  info!("已标记「{}」为已读", book_title);

  // ---- 阶段二：上下文数据组装 ----
  let (title, author, tags) = db::get_book_meta(conn, book_id)?;
  let comments = db::get_top_comments(conn, book_id, 5)?;

  // ---- 检查 LLM 是否可用（设置页 → 环境变量兜底） ----
  let llm = agent::resolve_config();

  let review = match llm {
    Some(config) => {
      println!("\nAI 助手已就绪（模型: {}）", config.model);
      println!("输入 /done 结束对话并生成书评，输入 /skip 跳过 AI 直接手动写评论。\n");

      // ---- 阶段三：多轮苏格拉底式 AI 对话 ----
      match run_dialogue(conn, &config, &title, &author, &tags, &comments).await {
        Ok(r) => r,
        Err(e) => {
          warn!("AI 对话失败: {e}，降级为手动输入");
          println!("\nAI 对话出错: {e}");
          manual_review(&title)?
        }
      }
    }
    None => {
      println!("\n未设置 OPENAI_API_KEY，跳过 AI 对话。");
      println!("你可以直接输入一段书评（多行输入，以空行结束）：");
      manual_review(&title)?
    }
  };

  // ---- 阶段四：最终书评入库 ----
  if !review.trim().is_empty() {
    db::save_review(conn, book_id, &review)?;
    let _ = db::insert_my_comment(conn, book_id, &review);
    println!("\n书评已保存到数据库。");
  } else {
    println!("\n未输入书评，已跳过。");
  }

  println!("《{}》已标记为已读。", title);
  Ok(())
}

// ============================= //
//  苏格拉底式对话
// ============================= //

/// 构建系统提示词，注入书名、作者、标签和参考短评
fn build_system_prompt(title: &str, author: &str, tags: &str, comments: &[String]) -> String {
  // 参考短评格式化为编号列表
  let comments_text = if comments.is_empty() {
    "（暂无参考短评）".to_string()
  } else {
    comments
      .iter()
      .enumerate()
      .map(|(i, c)| format!("{}. {c}", i + 1))
      .collect::<Vec<_>>()
      .join("\n")
  };

  format!(
    r#"你是一位资深推理小说评论家，正在帮助读者复盘刚读完的《{title}》（作者：{author}）。
这本书的标签是：{tags}。
以下是其他读者的一些观点（供参考，若用户提及可与其探讨）：
{comments_text}

你的任务：
1. 通过提问引导用户说出对这本书的感受、对诡计/反转的评价、对人物的看法。
2. 问题要具体（例如："你觉得凶手的动机是否合理？""哪个场景让你最毛骨悚然？""你对这个结局满意吗？"）。
3. 当用户表达观点后，可以结合参考短评进行追问（"有读者认为节奏拖沓，你认同吗？"）。
4. 保持对话自然，每次回复控制在 100 字以内。
5. 当用户输入 /done 时，停止提问，输出一段基于本次对话的总结性书评。"#
  )
}

/// 运行多轮对话循环，返回最终生成的书评（每次 LLM 调用的 token 用量入库统计）
async fn run_dialogue(
  conn: &rusqlite::Connection,
  config: &LlmConfig,
  title: &str,
  author: &str,
  tags: &str,
  comments: &[String],
) -> Result<String, FinishError> {
  let system_prompt = build_system_prompt(title, author, tags, comments);

  // 初始化对话历史：system + 一个 user 消息触发开场白
  // 多数 API 要求 system 后紧跟 user，不允许 system → assistant 序列
  let mut history: Vec<Message> = vec![
    Message::system(system_prompt),
    Message::user("请用一句话开场，引导我开始聊聊这本书。"),
  ];

  /// 记录一次调用的 token 用量（统计失败不影响对话）
  fn record(conn: &rusqlite::Connection, outcome: &agent::ChatOutcome) {
    let _ = db::record_llm_usage(
      conn,
      &outcome.model,
      outcome.usage.prompt_tokens,
      outcome.usage.completion_tokens,
      outcome.usage.total_tokens,
    );
  }

  // AI 发出开场白
  let opening = agent::chat(config, &history)
    .await
    .map_err(|e| FinishError::LlmError(e.to_string()))?;
  record(conn, &opening);
  let opening = opening.content;
  println!("AI: {opening}\n");
  history.push(Message::assistant(opening));

  loop {
    // 读取用户输入
    let input: String = Input::new()
      .with_prompt("你")
      .allow_empty(true)
      .interact_text()
      .map_err(io_abort)?;

    let trimmed = input.trim();

    // 结束指令
    if trimmed == "/done" || trimmed == "/finish" {
      // ---- 阶段四：最终书评生成 ----
      println!("\n正在生成书评...\n");
      history.push(Message::user(
        "基于刚才我们所有的对话，请以第一人称\"我\"的口吻，写一篇 150~300 字的短评。\
         要求逻辑清晰，包含对故事核心悬念或人物的评价，情感真挚。",
      ));

      let review = agent::chat(config, &history)
        .await
        .map_err(|e| FinishError::LlmError(e.to_string()))?;
      record(conn, &review);
      let review = review.content;

      println!("--- 生成的书评 ---\n{review}\n");
      return Ok(review);
    }

    if trimmed == "/skip" {
      return manual_review(title);
    }

    // 空输入跳过本轮
    if trimmed.is_empty() {
      continue;
    }

    // 将用户消息加入历史并请求 AI 回复
    history.push(Message::user(trimmed));

    match agent::chat(config, &history).await {
      Ok(outcome) => {
        record(conn, &outcome);
        println!("AI: {}\n", outcome.content);
        history.push(Message::assistant(outcome.content));
      }
      Err(e) => {
        // API 出错时询问是否重试
        println!("\nAI 请求失败: {e}");
        let retry = dialoguer::Confirm::new()
          .with_prompt("是否重试？")
          .default(true)
          .interact()
          .map_err(io_abort)?;
        if !retry {
          println!("退出 AI 对话。");
          return manual_review(title);
        }
        // 移除刚才的用户消息，下轮重试
        history.pop();
      }
    }
  }
}

// ============================= //
//  手动输入降级
// ============================= //

/// 手动输入书评（多行，以空行结束）
fn manual_review(title: &str) -> Result<String, FinishError> {
  println!("\n请输入你对《{}》的书评（多行输入，单独一行输入空行结束）：", title);

  let mut lines = Vec::new();
  loop {
    let line: String = Input::new()
      .with_prompt("")
      .allow_empty(true)
      .interact_text()
      .map_err(io_abort)?;

    if line.trim().is_empty() {
      break;
    }
    lines.push(line);
  }

  Ok(lines.join("\n"))
}

// ============================= //
//  辅助
// ============================= //

fn io_abort(e: dialoguer::Error) -> FinishError {
  FinishError::Other(e.to_string())
}
