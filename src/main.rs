use mystery_novel_agent::{config, db, device, finish, ingestion, merge, shelf};

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand};
use comfy_table::{presets::UTF8_FULL, ContentArrangement, Table};

#[derive(Parser)]
#[command(name = "mystery-novel-agent", about = "推理小说阅读管理助手")]
struct Cli {
  #[command(subcommand)]
  command: Commands,
}

#[derive(Subcommand)]
enum Commands {
  /// 导入 EPUB 并增强元数据（传入文件夹则批量导入其中所有 EPUB）
  Add {
    /// EPUB 文件路径，或包含多个 EPUB 的文件夹
    epub_path: PathBuf,
    /// 合并本模式：支持多选 claspclub 条目并融合简介
    #[arg(long)]
    merged: bool,
  },
  /// 标记读完 + AI 书评对话
  Finish,
  /// 查看书库中的书籍
  List {
    /// 限制显示数量
    #[arg(long)]
    limit: Option<usize>,
    /// 按书名或作者过滤
    #[arg(long)]
    search: Option<String>,
  },
  /// 从书库中删除一本书
  Delete {
    /// 书籍 ID
    id: i64,
  },
  /// 检测已连接的阅读设备并同步书籍
  Sync,
  /// 合并系列书籍为单个 EPUB（LLM 融合简介）
  Merge {
    /// 按系列名合并
    #[arg(long)]
    series: Option<String>,
    /// 按书籍 ID 列表合并（逗号分隔）
    #[arg(long)]
    ids: Option<String>,
  },
  /// 书库路径配置
  Library {
    #[command(subcommand)]
    action: LibraryConfigAction,
  },
  /// 渲染 HTML 书架到书库目录（shelf.html）
  Shelf,
}

#[derive(Subcommand)]
enum LibraryConfigAction {
  /// 设置书库路径
  Set { path: PathBuf },
  /// 显示当前书库路径
  Show,
}

#[tokio::main]
async fn main() {
  tracing_subscriber::fmt()
    .with_env_filter(
      tracing_subscriber::EnvFilter::from_default_env().add_directive("info".parse().unwrap()),
    )
    .init();

  let cli = Cli::parse();
  let db_path = config::AppConfig::load().database_file();
  let conn = db::open_db(&db_path.to_string_lossy()).expect("无法打开数据库");

  let result = match cli.command {
    Commands::Add { epub_path, merged } => {
      run_add(&conn, epub_path, ingestion::IngestOptions { merged, batch: false }).await
    }
    Commands::Finish => finish::finish_book(&conn)
      .await
      .map_err(|e| anyhow::anyhow!("{e}")),
    Commands::List { limit, search } => run_list(&conn, limit, search),
    Commands::Delete { id } => match db::delete_book(&conn, id) {
      Ok(orphaned) => {
        // 引用归零的封面文件删除（仅限 covers 目录内）
        if let Ok(dir) = config::AppConfig::load().covers_dir() {
          for p in orphaned {
            let cp = std::path::Path::new(&p);
            if cp.starts_with(&dir) && cp.is_file() {
              let _ = std::fs::remove_file(cp);
            }
          }
        }
        println!("已删除 (id={id})");
        Ok(())
      }
      Err(e) => Err(anyhow::anyhow!("{e}")),
    },
    Commands::Sync => device::run_sync(&conn)
      .await
      .map_err(|e| anyhow::anyhow!("{e}")),
    Commands::Merge { series, ids } => merge::run_merge(&conn, series, ids)
      .await
      .map_err(|e| anyhow::anyhow!("{e}")),
    Commands::Library { action } => run_library_config(action),
    Commands::Shelf => match shelf::render(&conn) {
      Ok(path) => {
        println!("书架已生成: {}", path.display());
        Ok(())
      }
      Err(e) => Err(anyhow::anyhow!("{e}")),
    },
  };

  if let Err(e) = result {
    eprintln!("错误: {e}");
    std::process::exit(1);
  }
}

/// add 命令：单个 EPUB 或文件夹（批量导入文件夹下所有 EPUB）
/// 导入成功后自动渲染一次 HTML 书架（单个直接渲染，批量在完成后渲染一次）
async fn run_add(
  conn: &rusqlite::Connection,
  epub_path: PathBuf,
  opts: ingestion::IngestOptions,
) -> anyhow::Result<()> {
  if !epub_path.is_dir() {
    ingestion::ingest_book(epub_path, conn, opts)
      .await
      .map_err(|e| anyhow::anyhow!("{e}"))?;
    auto_render_shelf(conn);
    return Ok(());
  }

  // 批量模式：递归收集文件夹下所有 EPUB，按路径排序；唯一结果自动静默处理
  let mut epubs: Vec<PathBuf> = Vec::new();
  collect_epubs(&epub_path, &mut epubs)?;
  epubs.sort();

  if epubs.is_empty() {
    anyhow::bail!("文件夹下没有 EPUB 文件: {}", epub_path.display());
  }

  let total = epubs.len();
  println!("发现 {total} 个 EPUB 文件，开始批量导入（唯一结果自动处理，多结果需交互）：");

  let batch_opts = ingestion::IngestOptions { merged: opts.merged, batch: true };

  let mut ok_cnt = 0usize;
  let mut skip_cnt = 0usize;
  let mut fail_cnt = 0usize;

  for (i, path) in epubs.iter().enumerate() {
    println!("\n========== [{}/{}] {} ==========", i + 1, total, path.display());

    match ingestion::ingest_book(path.clone(), conn, batch_opts).await {
      Ok(()) => ok_cnt += 1,
      Err(ingestion::IngestionError::UserAbort) => {
        skip_cnt += 1;
        println!("已跳过当前书籍。");
        if !confirm_continue()? {
          println!("批量导入已中止。");
          break;
        }
      }
      Err(e) => {
        fail_cnt += 1;
        eprintln!("导入失败: {e}");
        if !confirm_continue()? {
          println!("批量导入已中止。");
          break;
        }
      }
    }
  }

  println!("\n批量导入结束：成功 {ok_cnt} 本，跳过 {skip_cnt} 本，失败 {fail_cnt} 本");

  if ok_cnt > 0 {
    auto_render_shelf(conn);
  }
  Ok(())
}

/// add 成功后自动渲染书架（失败仅告警，不影响导入结果）
fn auto_render_shelf(conn: &rusqlite::Connection) {
  match shelf::render(conn) {
    Ok(path) => println!("书架已更新: {}", path.display()),
    Err(e) => eprintln!("书架渲染失败: {e}"),
  }
}

fn confirm_continue() -> anyhow::Result<bool> {
  Ok(
    dialoguer::Confirm::new()
      .with_prompt("继续处理剩余书籍？")
      .default(true)
      .interact()?,
  )
}

/// 递归收集目录下的所有 EPUB 文件
fn collect_epubs(dir: &Path, out: &mut Vec<PathBuf>) -> anyhow::Result<()> {
  for entry in std::fs::read_dir(dir)? {
    let path = entry?.path();
    if path.is_dir() {
      collect_epubs(&path, out)?;
    } else if path
      .extension()
      .map(|x| x.eq_ignore_ascii_case("epub"))
      .unwrap_or(false)
    {
      out.push(path);
    }
  }
  Ok(())
}

fn run_library_config(action: LibraryConfigAction) -> anyhow::Result<()> {  match action {
    LibraryConfigAction::Set { path } => {
      let abs = dunce::canonicalize(&path).unwrap_or(path.clone());
      std::fs::create_dir_all(&abs)?;
      let mut cfg = config::AppConfig::load();
      cfg.library_path = Some(abs);
      cfg.save()?;
      println!("书库路径已设置。");
    }
    LibraryConfigAction::Show => {
      let cfg = config::AppConfig::load();
      match cfg.library_path {
        Some(p) => println!("书库路径: {}", p.display()),
        None => println!("尚未配置书库路径。"),
      }
    }
  }
  Ok(())
}

fn run_list(
  conn: &rusqlite::Connection,
  limit: Option<usize>,
  search: Option<String>,
) -> anyhow::Result<()> {
  let books = db::get_all_books(conn)?;

  let filtered: Vec<_> = books
    .into_iter()
    .filter(|b| {
      if let Some(ref kw) = search {
        let kw = kw.to_lowercase();
        b.title.to_lowercase().contains(&kw) || b.author.to_lowercase().contains(&kw)
      } else {
        true
      }
    })
    .take(limit.unwrap_or(usize::MAX))
    .collect();

  let mut table = Table::new();
  table
    .set_content_arrangement(ContentArrangement::Dynamic)
    .load_preset(UTF8_FULL)
    .set_header(vec!["ID", "书名", "作者", "标签", "状态", "系列", "书库文件"]);

  for b in &filtered {
    // 系列列：卷号已知时展示 "系列名 #N"，缺卷号只显示系列名
    let series = match (b.series_name.as_str(), b.series_order) {
      (n, Some(o)) if !n.is_empty() => format!("{n} #{o}"),
      (n, _) if !n.is_empty() => n.to_string(),
      _ => String::new(),
    };
    table.add_row(vec![
      b.id.to_string(),
      b.title.clone(),
      b.author.clone(),
      b.tags.clone(),
      b.status.clone(),
      series,
      b.library_file.clone(),
    ]);
  }

  println!("{table}");
  println!("\n共 {} 本书", filtered.len());
  Ok(())
}
