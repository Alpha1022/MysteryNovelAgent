use rusqlite::{Connection, Result as SqlResult};
use serde::Serialize;
use std::path::Path;
use tracing::{info, warn};

/// 打开（或创建）本地 SQLite 数据库，并初始化表结构 + 迁移
pub fn open_db(path: &str) -> SqlResult<Connection> {
  let conn = Connection::open(path)?;
  // 同步（WebDav）会以第二个连接并发读写同一数据库文件；
  // 默认 busy_timeout 为 0，任何瞬时锁冲突都会直接报 "database is locked"，
  // 统一等待 5s 让短事务自然错开（移动端启动高峰尤其必要）
  conn.busy_timeout(std::time::Duration::from_secs(5))?;
  init_tables(&conn)?;
  migrate(&conn)?;
  info!("数据库已就绪: {path}");
  Ok(conn)
}

/// 初始化 `books`、`comments` 表，并预留 `embeddings` 表供未来 RAG 扩展
pub fn init_tables(conn: &Connection) -> SqlResult<()> {
  conn.execute_batch(
    "
    CREATE TABLE IF NOT EXISTS books (
      id          INTEGER PRIMARY KEY AUTOINCREMENT,
      title       TEXT NOT NULL,
      author      TEXT,
      tags        TEXT,
      file_path   TEXT,
      clasp_id    TEXT
    );

    CREATE TABLE IF NOT EXISTS comments (
      id          INTEGER PRIMARY KEY AUTOINCREMENT,
      book_id     INTEGER NOT NULL,
      rating      INTEGER,
      content     TEXT NOT NULL,
      usefulness  INTEGER DEFAULT 0,
      source      TEXT DEFAULT '豆瓣',
      FOREIGN KEY (book_id) REFERENCES books(id)
    );

    -- 预留 RAG 向量表，暂不填充数据
    CREATE TABLE IF NOT EXISTS embeddings (
      id          INTEGER PRIMARY KEY AUTOINCREMENT,
      book_id     INTEGER,
      chunk_text  TEXT,
      vector      BLOB,
      FOREIGN KEY (book_id) REFERENCES books(id)
    );

    -- LLM token 用量统计（按模型累计）
    CREATE TABLE IF NOT EXISTS llm_usage (
      model             TEXT PRIMARY KEY,
      calls             INTEGER DEFAULT 0,
      prompt_tokens     INTEGER DEFAULT 0,
      completion_tokens INTEGER DEFAULT 0,
      total_tokens      INTEGER DEFAULT 0,
      last_used         DATETIME
    );

    -- 来源管理：每本书的有序 claspclub / 豆瓣项目
    -- kind: 'clasp'（ref = clasp 条目 ID）| 'douban'（ref = 豆瓣书籍页 URL）
    CREATE TABLE IF NOT EXISTS sources (
      id         INTEGER PRIMARY KEY AUTOINCREMENT,
      book_id    INTEGER NOT NULL,
      kind       TEXT NOT NULL,
      ref        TEXT NOT NULL,
      position   INTEGER NOT NULL DEFAULT 0,
      title      TEXT,
      author     TEXT,
      cover_url  TEXT,
      cover_path TEXT,
      summary    TEXT,
      tags       TEXT,
      editions   TEXT,
      FOREIGN KEY (book_id) REFERENCES books(id)
    );
    CREATE INDEX IF NOT EXISTS idx_sources_book ON sources(book_id, kind, position);

    -- 封面文件引用计数：引用归零时才允许删除缓存文件
    CREATE TABLE IF NOT EXISTS cover_refs (
      path TEXT PRIMARY KEY,
      refs INTEGER NOT NULL DEFAULT 0
    );
    ",
  )?;
  Ok(())
}

/// 迁移：为旧表补充新增字段（ALTER TABLE 幂等，列已存在时静默忽略）
fn migrate(conn: &Connection) -> SqlResult<()> {
  let _ = conn.execute("ALTER TABLE books ADD COLUMN status TEXT DEFAULT '想读'", []);
  let _ = conn.execute("ALTER TABLE books ADD COLUMN my_review TEXT", []);
  let _ = conn.execute("ALTER TABLE books ADD COLUMN finished_date DATETIME", []);
  let _ = conn.execute("ALTER TABLE comments ADD COLUMN is_mine INTEGER DEFAULT 0", []);
  // 书库管理新增字段
  let _ = conn.execute("ALTER TABLE books ADD COLUMN description TEXT", []);
  let _ = conn.execute("ALTER TABLE books ADD COLUMN cover_path TEXT", []);
  let _ = conn.execute("ALTER TABLE books ADD COLUMN series_name TEXT", []);
  let _ = conn.execute("ALTER TABLE books ADD COLUMN series_order INTEGER", []);
  let _ = conn.execute("ALTER TABLE books ADD COLUMN library_file TEXT", []);
  // 合并本：来源 clasp 条目 ID 的 JSON 数组；合并书：来源本地书籍 ID 的 JSON 数组
  let _ = conn.execute("ALTER TABLE books ADD COLUMN clasp_ids TEXT", []);
  let _ = conn.execute("ALTER TABLE books ADD COLUMN merged_from TEXT", []);
  // 豆瓣书籍页链接（JSON 数组；重新匹配 / 手动填写后用于重抓短评与封面兜底）
  let _ = conn.execute("ALTER TABLE books ADD COLUMN douban_urls TEXT", []);
  // 短评来源定位：来源项目的 clasp ID / 豆瓣链接（悬停展示封面书名、按来源重抓）
  let _ = conn.execute("ALTER TABLE comments ADD COLUMN source_ref TEXT", []);
  // 多书库：书籍归属的书库 ID（旧数据 NULL，由启动迁移归入默认书库）
  let _ = conn.execute("ALTER TABLE books ADD COLUMN library_id TEXT", []);
  // 来源项目的标签（JSON 数组；仅 claspclub 来源有）
  let _ = conn.execute("ALTER TABLE sources ADD COLUMN tags TEXT", []);
  // 来源项目的系列信息（仅 claspclub 来源有）
  let _ = conn.execute("ALTER TABLE sources ADD COLUMN series_name TEXT", []);
  let _ = conn.execute("ALTER TABLE sources ADD COLUMN series_order INTEGER", []);
  seed_cover_refs(conn);
  Ok(())
}

/// 将无书库归属的书籍归入指定书库（启动迁移），返回归入数量
pub fn assign_legacy_books(conn: &Connection, library_id: &str) -> SqlResult<usize> {
  conn.execute(
    "UPDATE books SET library_id = ?1 WHERE library_id IS NULL",
    rusqlite::params![library_id],
  )
}

/// 封面引用计数种子：按当前数据**重算**（幂等且可自愈历史漂移）
///
/// 每次打开数据库时重建计数表：计数 = books.cover_path 引用 + sources.cover_path
/// 引用 + sources.editions JSON 内版本封面路径的出现次数。仅在打开时执行，
/// 无并发增减，重算结果即为精确值。WebDav 从云端同步后也调用以重算。
pub fn seed_cover_refs(conn: &Connection) {
  let _ = conn.execute("DELETE FROM cover_refs", []);
  let _ = conn.execute_batch(
    "INSERT INTO cover_refs (path, refs)
     SELECT p,
            (SELECT COUNT(*) FROM books WHERE cover_path = p)
          + (SELECT COUNT(*) FROM sources WHERE cover_path = p)
     FROM (
       SELECT DISTINCT cover_path AS p FROM books WHERE cover_path IS NOT NULL
       UNION
       SELECT DISTINCT cover_path FROM sources WHERE cover_path IS NOT NULL
     );",
  );
  // editions JSON 内的版本封面路径（SQL 无法解析 JSON 数组，Rust 侧补齐）
  let all: Vec<String> = match conn
    .prepare("SELECT editions FROM sources WHERE editions IS NOT NULL")
    .and_then(|mut s| {
      s.query_map([], |r| r.get::<_, String>(0))
        .map(|rows| rows.filter_map(|x| x.ok()).collect())
    }) {
    Ok(v) => v,
    Err(_) => return,
  };
  let mut counts: std::collections::HashMap<String, i64> = std::collections::HashMap::new();
  for json in &all {
    for p in editions_paths(Some(json)) {
      *counts.entry(p).or_insert(0) += 1;
    }
  }
  for (p, c) in counts {
    let _ = conn.execute(
      "INSERT INTO cover_refs (path, refs) VALUES (?1, ?2)
       ON CONFLICT(path) DO UPDATE SET refs = ?2",
      rusqlite::params![p, c],
    );
  }
}

/// 封面文件是否仍有有效引用登记（refs > 0；无登记视为无引用）
pub fn cover_has_ref(conn: &Connection, path: &str) -> SqlResult<bool> {
  let refs: i64 = conn
    .query_row(
      "SELECT refs FROM cover_refs WHERE path = ?1",
      rusqlite::params![path],
      |r| r.get(0),
    )
    .unwrap_or(0);
  Ok(refs > 0)
}

/// 清扫封面缓存目录：删除无任何引用登记的文件（引用计数重算后调用），返回删除数
pub fn cleanup_unreferenced_covers(conn: &Connection, dir: &Path) -> usize {
  let Ok(entries) = std::fs::read_dir(dir) else {
    return 0;
  };
  let mut removed = 0usize;
  for entry in entries.flatten() {
    let p = entry.path();
    if !p.is_file() {
      continue;
    }
    let s = p.to_string_lossy().into_owned();
    match cover_has_ref(conn, &s) {
      Ok(true) => {}
      Ok(false) => {
        if std::fs::remove_file(&p).is_ok() {
          removed += 1;
        }
      }
      Err(e) => warn!("封面引用检查失败 [{s}]: {e}"),
    }
  }
  removed
}

/// 当前仍存活的书/来源引用数（释放时的双保险校验）
fn live_cover_refs(conn: &Connection, path: &str) -> SqlResult<i64> {
  let books: i64 = conn.query_row(
    "SELECT COUNT(*) FROM books WHERE cover_path = ?1",
    rusqlite::params![path],
    |r| r.get(0),
  )?;
  let sources: i64 = conn.query_row(
    "SELECT COUNT(*) FROM sources WHERE cover_path = ?1",
    rusqlite::params![path],
    |r| r.get(0),
  )?;
  Ok(books + sources)
}

/// 封面文件被引用一次：计数 +1（无登记行则新建）
pub fn cover_ref_add(conn: &Connection, path: &str) -> SqlResult<()> {
  conn.execute(
    "INSERT INTO cover_refs (path, refs) VALUES (?1, 1)
     ON CONFLICT(path) DO UPDATE SET refs = refs + 1",
    rusqlite::params![path],
  )?;
  Ok(())
}

/// 封面引用释放：计数 -1，归零时清除登记并返回 true（调用方应删除文件）
///
/// 未登记的旧数据按实际存活引用数补账，且归零时以存活引用双保险。
pub fn cover_ref_release(conn: &Connection, path: &str) -> SqlResult<bool> {
  let updated = conn.execute(
    "UPDATE cover_refs SET refs = refs - 1 WHERE path = ?1 AND refs > 0",
    rusqlite::params![path],
  )?;
  if updated == 0 {
    // 未登记（旧数据）：按当前存活引用数补登记后减一
    let live = live_cover_refs(conn, path)?;
    let after = live.saturating_sub(1) as i64;
    conn.execute(
      "INSERT INTO cover_refs (path, refs) VALUES (?1, ?2)
       ON CONFLICT(path) DO UPDATE SET refs = ?2",
      rusqlite::params![path, after],
    )?;
  }
  let refs: i64 = conn.query_row(
    "SELECT refs FROM cover_refs WHERE path = ?1",
    rusqlite::params![path],
    |r| r.get(0),
  )?;
  if refs <= 0 {
    conn.execute(
      "DELETE FROM cover_refs WHERE path = ?1",
      rusqlite::params![path],
    )?;
    // 双保险：仍有书/来源引用时不删文件
    if live_cover_refs(conn, path)? == 0 {
      return Ok(true);
    }
  }
  Ok(false)
}

/// 插入一本书，返回其自增 ID
/// `clasp_ids` 为来源 clasp 条目 ID 的 JSON 数组字符串（单本也存数组，如 `["id1"]`；可为空）
pub fn insert_book(
  conn: &Connection,
  title: &str,
  author: &str,
  tags: &str,
  file_path: &str,
  clasp_ids: &str,
) -> SqlResult<i64> {
  conn.execute(
    "INSERT INTO books (title, author, tags, file_path, clasp_ids) VALUES (?1, ?2, ?3, ?4, ?5)",
    rusqlite::params![title, author, tags, file_path, clasp_ids],
  )?;
  Ok(conn.last_insert_rowid())
}

/// 更新书籍的增强元数据（简介、封面、系列、书库文件、clasp 条目）
#[allow(clippy::too_many_arguments)]
pub fn update_book_enrichment(
  conn: &Connection,
  book_id: i64,
  description: Option<&str>,
  cover_path: Option<&str>,
  series_name: Option<&str>,
  series_order: Option<i64>,
  library_file: Option<&str>,
  clasp_ids: Option<&str>,
) -> SqlResult<()> {
  conn.execute(
    "UPDATE books SET
       description   = COALESCE(?1, description),
       cover_path    = COALESCE(?2, cover_path),
       series_name   = COALESCE(?3, series_name),
       series_order  = COALESCE(?4, series_order),
       library_file  = COALESCE(?5, library_file),
       clasp_ids     = COALESCE(?6, clasp_ids)
     WHERE id = ?7",
    rusqlite::params![
      description,
      cover_path,
      series_name,
      series_order,
      library_file,
      clasp_ids,
      book_id
    ],
  )?;
  Ok(())
}

/// 本地数据库中的书籍简要信息
pub struct LocalBook {
  pub id: i64,
  pub title: String,
  pub author: String,
  pub tags: String,
  pub status: String,
  pub series_name: String,
  pub series_order: Option<i64>,
  pub library_file: String,
}

/// 获取本地数据库中的所有书籍
pub fn get_all_books(conn: &Connection) -> SqlResult<Vec<LocalBook>> {
  let mut stmt = conn.prepare(
    "SELECT id, title, COALESCE(author,''), COALESCE(tags,''),
            COALESCE(status,''), COALESCE(series_name,''), series_order, COALESCE(library_file,'')
     FROM books ORDER BY id",
  )?;
  let rows = stmt.query_map([], |row| {
    Ok(LocalBook {
      id: row.get(0)?,
      title: row.get(1)?,
      author: row.get(2)?,
      tags: row.get(3)?,
      status: row.get::<_, Option<String>>(4)?.unwrap_or_default(),
      series_name: row.get(5)?,
      series_order: row.get(6)?,
      library_file: row.get(7)?,
    })
  })?;
  rows.collect()
}

/// 插入一条短评
///
/// `source`：'豆瓣' / 'claspclub'（网络短评）或 'AI助手生成'（个人书评）；
/// `source_ref`：来源项目定位（clasp ID / 豆瓣链接），个人书评传 None
pub fn insert_comment(
  conn: &Connection,
  book_id: i64,
  rating: Option<i32>,
  content: &str,
  usefulness: i32,
  source: &str,
  source_ref: Option<&str>,
) -> SqlResult<()> {
  conn.execute(
    "INSERT INTO comments (book_id, rating, content, usefulness, source, source_ref)
     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
    rusqlite::params![book_id, rating, content, usefulness, source, source_ref],
  )?;
  Ok(())
}

// ============================= //
//  finish 模块所需查询
// ============================= //

/// 书籍简要信息（用于列表选择）
pub struct BookSummary {
  pub id: i64,
  pub title: String,
  pub author: String,
}

/// 查询所有未读完的书籍（status 为 NULL、'想读' 或 '在读'）
pub fn get_unfinished_books(conn: &Connection) -> SqlResult<Vec<BookSummary>> {
  let mut stmt = conn.prepare(
    "SELECT id, title, COALESCE(author,'') FROM books
     WHERE status IS NULL OR status IN ('想读', '在读') ORDER BY id",
  )?;
  let rows = stmt.query_map([], |row| {
    Ok(BookSummary {
      id: row.get(0)?,
      title: row.get(1)?,
      author: row.get(2)?,
    })
  })?;
  rows.collect()
}

/// 获取书籍元数据 (title, author, tags)
pub fn get_book_meta(conn: &Connection, book_id: i64) -> SqlResult<(String, String, String)> {
  conn.query_row(
    "SELECT title, COALESCE(author,''), COALESCE(tags,'') FROM books WHERE id = ?1",
    rusqlite::params![book_id],
    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
  )
}

/// 获取按有用数降序排列的前 N 条豆瓣短评内容
pub fn get_top_comments(conn: &Connection, book_id: i64, limit: usize) -> SqlResult<Vec<String>> {
  let mut stmt = conn.prepare(
    "SELECT content FROM comments
     WHERE book_id = ?1 AND (is_mine IS NULL OR is_mine = 0)
     ORDER BY usefulness DESC LIMIT ?2",
  )?;
  let rows = stmt.query_map(rusqlite::params![book_id, limit as i64], |row| {
    Ok(row.get::<_, String>(0)?)
  })?;
  rows.collect()
}

/// 更新书籍状态为已读，并记录完成时间
pub fn mark_book_finished(conn: &Connection, book_id: i64) -> SqlResult<()> {
  conn.execute(
    "UPDATE books SET status = '已读', finished_date = datetime('now','localtime') WHERE id = ?1",
    rusqlite::params![book_id],
  )?;
  Ok(())
}

/// 保存用户书评到 books.my_review
pub fn save_review(conn: &Connection, book_id: i64, review: &str) -> SqlResult<()> {
  conn.execute(
    "UPDATE books SET my_review = ?1 WHERE id = ?2",
    rusqlite::params![review, book_id],
  )?;
  Ok(())
}

/// 插入一条 AI 生成的评论到 comments 表，is_mine = 1
pub fn insert_my_comment(conn: &Connection, book_id: i64, content: &str) -> SqlResult<()> {
  conn.execute(
    "INSERT INTO comments (book_id, content, source, is_mine) VALUES (?1, ?2, 'AI助手生成', 1)",
    rusqlite::params![book_id, content],
  )?;
  Ok(())
}

// ============================= //
//  书库管理所需查询
// ============================= //

/// 从本地数据库删除一本书及其所有评论与来源项目
///
/// 释放书籍封面与全部来源封面的引用；返回已无引用的封面路径（调用方删文件）。
pub fn delete_book(conn: &Connection, book_id: i64) -> SqlResult<Vec<String>> {
  let mut orphaned: Vec<String> = Vec::new();
  // 书籍封面引用释放
  if let Ok(Some(cover)) = conn.query_row(
    "SELECT cover_path FROM books WHERE id = ?1",
    rusqlite::params![book_id],
    |r| r.get::<_, Option<String>>(0),
  ) {
    if cover_ref_release(conn, &cover)? {
      orphaned.push(cover);
    }
  }
  // 来源封面与版本封面引用释放
  for s in list_sources(conn, book_id)? {
    if let Some(p) = s.cover_path {
      if cover_ref_release(conn, &p)? {
        orphaned.push(p);
      }
    }
    orphaned.extend(cover_ref_release_editions(conn, s.editions.as_deref())?);
  }
  conn.execute(
    "DELETE FROM comments WHERE book_id = ?1",
    rusqlite::params![book_id],
  )?;
  conn.execute(
    "DELETE FROM sources WHERE book_id = ?1",
    rusqlite::params![book_id],
  )?;
  conn.execute(
    "DELETE FROM books WHERE id = ?1",
    rusqlite::params![book_id],
  )?;
  Ok(orphaned)
}

/// 按系列名查询书籍（返回 id、title、series_order），按卷号排序
pub struct SeriesBook {
  pub id: i64,
  pub title: String,
  pub file_path: String,
  pub library_file: String,
  pub series_order: Option<i64>,
  pub description: Option<String>,
}

/// 查询某系列下的所有书籍（series_order 升序，NULL 在最后）
pub fn get_books_by_series(conn: &Connection, series_name: &str) -> SqlResult<Vec<SeriesBook>> {
  let mut stmt = conn.prepare(
    "SELECT id, title, COALESCE(file_path,''), COALESCE(library_file,''), series_order, description
     FROM books WHERE series_name = ?1
     ORDER BY series_order IS NULL, series_order, id",
  )?;
  let rows = stmt.query_map(rusqlite::params![series_name], |row| {
    Ok(SeriesBook {
      id: row.get(0)?,
      title: row.get(1)?,
      file_path: row.get(2)?,
      library_file: row.get(3)?,
      series_order: row.get(4)?,
      description: row.get(5)?,
    })
  })?;
  rows.collect()
}

/// 查询单本书的完整信息（用于 merge / sync 等）
pub fn get_book_full(conn: &Connection, book_id: i64) -> SqlResult<Option<SeriesBook>> {
  let result = conn
    .query_row(
      "SELECT id, title, COALESCE(file_path,''), COALESCE(library_file,''), series_order, description
       FROM books WHERE id = ?1",
      rusqlite::params![book_id],
      |row| {
        Ok(SeriesBook {
          id: row.get(0)?,
          title: row.get(1)?,
          file_path: row.get(2)?,
          library_file: row.get(3)?,
          series_order: row.get(4)?,
          description: row.get(5)?,
        })
      },
    )
    .ok();
  Ok(result)
}

// ============================= //
//  书架渲染所需查询
// ============================= //

/// 书架页面的单本书数据
pub struct ShelfRow {
  pub title: String,
  pub author: String,
  pub status: String,
  pub my_review: String,
  pub cover_path: Option<String>,
  /// clasp 条目 ID 的 JSON 数组字符串
  pub clasp_ids: String,
  pub series_name: String,
  pub series_order: Option<i64>,
}

/// 获取书架渲染所需的全部书籍数据
pub fn get_shelf_books(conn: &Connection) -> SqlResult<Vec<ShelfRow>> {
  let mut stmt = conn.prepare(
    "SELECT title, COALESCE(author,''), COALESCE(status,'想读'), COALESCE(my_review,''),
            cover_path, COALESCE(clasp_ids,''), COALESCE(series_name,''), series_order
     FROM books ORDER BY id",
  )?;
  let rows = stmt.query_map([], |row| {
    Ok(ShelfRow {
      title: row.get(0)?,
      author: row.get(1)?,
      status: row.get(2)?,
      my_review: row.get(3)?,
      cover_path: row.get(4)?,
      clasp_ids: row.get(5)?,
      series_name: row.get(6)?,
      series_order: row.get(7)?,
    })
  })?;
  rows.collect()
}

// ============================= //
//  GUI 查询（Tauri 前端专用）
// ============================= //

/// 书库网格卡片数据
#[derive(Serialize)]
pub struct BookCardRow {
  pub id: i64,
  pub title: String,
  pub author: String,
  pub tags: String,
  pub status: String,
  pub cover_path: Option<String>,
  pub series_name: String,
  pub series_order: Option<i64>,
  pub clasp_ids: String,
}

/// 按 status 筛选 + 标题/作者模糊搜索 + 书库过滤，返回网格所需字段
pub fn get_book_cards(
  conn: &Connection,
  status: Option<&str>,
  search: Option<&str>,
  library_id: Option<&str>,
) -> SqlResult<Vec<BookCardRow>> {
  let mut stmt = conn.prepare(
    "SELECT id, title, COALESCE(author,''), COALESCE(tags,''),
            COALESCE(status,'想读'), cover_path, COALESCE(series_name,''),
            series_order, COALESCE(clasp_ids,'')
     FROM books
     WHERE (?1 IS NULL OR COALESCE(status,'想读') = ?1)
       AND (?2 IS NULL
            OR LOWER(title) LIKE '%' || ?2 || '%'
            OR LOWER(COALESCE(author,'')) LIKE '%' || ?2 || '%')
       AND (?3 IS NULL OR library_id = ?3)
     ORDER BY id",
  )?;
  let rows = stmt.query_map(rusqlite::params![status, search, library_id], |row| {
    Ok(BookCardRow {
      id: row.get(0)?,
      title: row.get(1)?,
      author: row.get(2)?,
      tags: row.get(3)?,
      status: row.get(4)?,
      cover_path: row.get(5)?,
      series_name: row.get(6)?,
      series_order: row.get(7)?,
      clasp_ids: row.get(8)?,
    })
  })?;
  rows.collect()
}

/// 书籍详情页完整数据
#[derive(Serialize)]
pub struct BookDetailRow {
  pub id: i64,
  pub title: String,
  pub author: String,
  pub tags: String,
  pub status: String,
  pub description: Option<String>,
  pub cover_path: Option<String>,
  pub my_review: Option<String>,
  pub finished_date: Option<String>,
  pub series_name: Option<String>,
  pub series_order: Option<i64>,
  pub clasp_ids: Option<String>,
  pub merged_from: Option<String>,
  /// 豆瓣书籍页链接（JSON 数组；重抓短评 / 封面兜底数据源）
  pub douban_urls: Option<String>,
  /// 书库内规范文件名（封面替换时定位 EPUB 副本）
  pub library_file: Option<String>,
  /// 原始导入文件路径（书库副本缺失时打开兜底）
  pub file_path: Option<String>,
  /// 归属书库 ID（多书库：打开本地文件 / 阅读器按归属定位书库目录）
  pub library_id: Option<String>,
}

/// 获取单本书的完整详情
pub fn get_book_detail(conn: &Connection, book_id: i64) -> SqlResult<Option<BookDetailRow>> {
  let result = conn
    .query_row(
      "SELECT id, title, COALESCE(author,''), COALESCE(tags,''),
              COALESCE(status,'想读'), description, cover_path, my_review,
              CAST(finished_date AS TEXT), series_name, series_order,
              clasp_ids, merged_from, douban_urls, library_file, file_path, library_id
       FROM books WHERE id = ?1",
      rusqlite::params![book_id],
      |row| {
        Ok(BookDetailRow {
          id: row.get(0)?,
          title: row.get(1)?,
          author: row.get(2)?,
          tags: row.get(3)?,
          status: row.get(4)?,
          description: row.get(5)?,
          cover_path: row.get(6)?,
          my_review: row.get(7)?,
          finished_date: row.get(8)?,
          series_name: row.get(9)?,
          series_order: row.get(10)?,
          clasp_ids: row.get(11)?,
          merged_from: row.get(12)?,
          douban_urls: row.get(13)?,
          library_file: row.get(14)?,
          file_path: row.get(15)?,
          library_id: row.get(16)?,
        })
      },
    )
    .ok();
  Ok(result)
}

/// 短评行（含评分/有用数/来源/是否个人评论/来源项目定位）
#[derive(Serialize)]
pub struct CommentRow {
  pub id: i64,
  pub rating: Option<i32>,
  pub content: String,
  pub usefulness: i32,
  pub source: String,
  pub is_mine: i64,
  /// 来源项目（clasp ID / 豆瓣链接）；旧数据与个人书评为 None
  pub source_ref: Option<String>,
}

/// 获取指定书籍的全部短评（按有用数降序，含个人 AI 评论）
pub fn get_comments_for_book(conn: &Connection, book_id: i64) -> SqlResult<Vec<CommentRow>> {
  let mut stmt = conn.prepare(
    "SELECT id, rating, content, usefulness,
            COALESCE(source,'豆瓣'), COALESCE(is_mine, 0), source_ref
     FROM comments WHERE book_id = ?1
     ORDER BY is_mine ASC, usefulness DESC",
  )?;
  let rows = stmt.query_map(rusqlite::params![book_id], |row| {
    Ok(CommentRow {
      id: row.get(0)?,
      rating: row.get(1)?,
      content: row.get(2)?,
      usefulness: row.get(3)?,
      source: row.get(4)?,
      is_mine: row.get(5)?,
      source_ref: row.get(6)?,
    })
  })?;
  rows.collect()
}

/// 记录书籍所属书库（导入/合并入库时设置）
pub fn set_book_library(conn: &Connection, book_id: i64, library_id: &str) -> SqlResult<()> {
  conn.execute(
    "UPDATE books SET library_id = ?1 WHERE id = ?2",
    rusqlite::params![library_id, book_id],
  )?;
  Ok(())
}

/// Chatbot 知识行（书籍精简信息，供系统提示注入）
pub struct ChatbotBookRow {
  pub id: i64,
  pub title: String,
  pub author: String,
  pub status: String,
  pub tags: String,
  pub description: Option<String>,
  pub my_review: Option<String>,
  pub finished_date: Option<String>,
}

/// Chatbot 工具/概览：按关键词、状态、作者、标签检索书籍（可选限定书库）
///
/// keyword 匹配书名/作者；author/tag 为包含匹配（库内以逗号/顿号分隔存储）。
pub fn search_chatbot_books(
  conn: &Connection,
  library_id: Option<&str>,
  keyword: Option<&str>,
  status: Option<&str>,
  author: Option<&str>,
  tag: Option<&str>,
  limit: i64,
) -> SqlResult<Vec<ChatbotBookRow>> {
  let mut stmt = conn.prepare(
    "SELECT id, title, COALESCE(author,''), COALESCE(status,'想读'), COALESCE(tags,''),
            description, my_review, CAST(finished_date AS TEXT)
     FROM books
     WHERE (?1 IS NULL OR library_id = ?1)
       AND (?2 IS NULL
            OR LOWER(title) LIKE '%' || ?2 || '%'
            OR LOWER(COALESCE(author,'')) LIKE '%' || ?2 || '%')
       AND (?3 IS NULL OR COALESCE(status,'想读') = ?3)
       AND (?4 IS NULL OR LOWER(COALESCE(author,'')) LIKE '%' || LOWER(?4) || '%')
       AND (?5 IS NULL OR LOWER(COALESCE(tags,'')) LIKE '%' || LOWER(?5) || '%')
     ORDER BY id
     LIMIT ?6",
  )?;
  let rows = stmt.query_map(
    rusqlite::params![library_id, keyword, status, author, tag, limit],
    |row| {
      Ok(ChatbotBookRow {
        id: row.get(0)?,
        title: row.get(1)?,
        author: row.get(2)?,
        status: row.get(3)?,
        tags: row.get(4)?,
        description: row.get(5)?,
        my_review: row.get(6)?,
        finished_date: row.get(7)?,
      })
    },
  )?;
  rows.collect()
}

/// 更新书籍封面路径（封面替换）：登记新封面引用，释放旧引用
///
/// 返回被释放且已无引用的旧封面路径（调用方删除文件）。
pub fn update_book_cover(
  conn: &Connection,
  book_id: i64,
  cover_path: &str,
) -> SqlResult<Option<String>> {
  let old: Option<String> = conn
    .query_row(
      "SELECT cover_path FROM books WHERE id = ?1",
      rusqlite::params![book_id],
      |r| r.get(0),
    )
    .unwrap_or(None);
  conn.execute(
    "UPDATE books SET cover_path = ?1 WHERE id = ?2",
    rusqlite::params![cover_path, book_id],
  )?;
  cover_ref_add(conn, cover_path)?;
  if let Some(old_path) = old.as_deref() {
    if old_path != cover_path && cover_ref_release(conn, old_path)? {
      return Ok(Some(old_path.to_string()));
    }
  }
  Ok(None)
}

/// 更新书籍的 clasp 匹配与豆瓣链接（重新匹配 / 手动填写豆瓣链接）
///
/// `clasp_ids_json` 为 None 时保持原值；`douban_urls_json` 为 JSON 数组字符串
pub fn set_book_match_urls(
  conn: &Connection,
  book_id: i64,
  clasp_ids_json: Option<&str>,
  douban_urls_json: &str,
) -> SqlResult<()> {
  conn.execute(
    "UPDATE books SET clasp_ids = COALESCE(?1, clasp_ids), douban_urls = ?2 WHERE id = ?3",
    rusqlite::params![clasp_ids_json, douban_urls_json, book_id],
  )?;
  Ok(())
}

/// 更新书籍简介（重新匹配 / 豆瓣链接增强后的简介刷新）
pub fn set_book_description(
  conn: &Connection,
  book_id: i64,
  description: Option<&str>,
) -> SqlResult<()> {
  conn.execute(
    "UPDATE books SET description = ?1 WHERE id = ?2",
    rusqlite::params![description, book_id],
  )?;
  Ok(())
}

/// 删除书籍的豆瓣抓取短评（保留个人/AI 书评），返回删除条数
pub fn delete_douban_comments(conn: &Connection, book_id: i64) -> SqlResult<usize> {
  conn.execute(
    "DELETE FROM comments WHERE book_id = ?1 AND COALESCE(is_mine, 0) = 0",
    rusqlite::params![book_id],
  )
}

// ============================= //
//  来源管理（claspclub / 豆瓣有序项目）
// ============================= //

/// 来源项目行
#[derive(Debug, Clone, Serialize)]
pub struct SourceRow {
  pub kind: String,
  pub ref_key: String,
  pub position: i64,
  pub title: Option<String>,
  pub author: Option<String>,
  pub cover_url: Option<String>,
  pub cover_path: Option<String>,
  pub summary: Option<String>,
  /// 标签 JSON 数组（仅 claspclub 来源）
  pub tags: Option<String>,
  /// 系列信息（仅 claspclub 来源）
  pub series_name: Option<String>,
  pub series_order: Option<i64>,
  /// clasp 版本封面 JSON：[{label,url,path}]
  pub editions: Option<String>,
}

/// 列出书籍的全部来源项目（clasp 在前、豆瓣在后，各自按 position 升序）
pub fn list_sources(conn: &Connection, book_id: i64) -> SqlResult<Vec<SourceRow>> {
  let mut stmt = conn.prepare(
    "SELECT kind, ref, position, title, author, cover_url, cover_path, summary, tags, series_name, series_order, editions
     FROM sources WHERE book_id = ?1
     ORDER BY kind ASC, position, id",
  )?;
  let rows = stmt.query_map(rusqlite::params![book_id], |row| {
    Ok(SourceRow {
      kind: row.get(0)?,
      ref_key: row.get(1)?,
      position: row.get(2)?,
      title: row.get(3)?,
      author: row.get(4)?,
      cover_url: row.get(5)?,
      cover_path: row.get(6)?,
      summary: row.get(7)?,
      tags: row.get(8)?,
      series_name: row.get(9)?,
      series_order: row.get(10)?,
      editions: row.get(11)?,
    })
  })?;
  rows.collect()
}

/// 解析 editions JSON 中的本地封面路径
fn editions_paths(editions: Option<&str>) -> Vec<String> {
  serde_json::from_str::<Vec<serde_json::Value>>(editions.unwrap_or("[]"))
    .unwrap_or_default()
    .into_iter()
    .filter_map(|v| {
      v.get("path")
        .and_then(|p| p.as_str())
        .map(str::to_string)
        .filter(|p| !p.trim().is_empty())
    })
    .collect()
}

/// 登记一组版本封面引用
fn cover_ref_add_editions(conn: &Connection, editions: Option<&str>) -> SqlResult<()> {
  for p in editions_paths(editions) {
    cover_ref_add(conn, &p)?;
  }
  Ok(())
}

/// 释放一组版本封面引用；返回已无引用的路径（调用方删文件）
fn cover_ref_release_editions(conn: &Connection, editions: Option<&str>) -> SqlResult<Vec<String>> {
  let mut orphaned = Vec::new();
  for p in editions_paths(editions) {
    if cover_ref_release(conn, &p)? {
      orphaned.push(p);
    }
  }
  Ok(orphaned)
}

/// 插入一个来源项目（按给定位置；封面与版本封面文件登记引用）
#[allow(clippy::too_many_arguments)]
pub fn insert_source(
  conn: &Connection,
  book_id: i64,
  kind: &str,
  ref_key: &str,
  position: i64,
  title: Option<&str>,
  author: Option<&str>,
  cover_url: Option<&str>,
  cover_path: Option<&str>,
  summary: Option<&str>,
  tags: Option<&str>,
  series_name: Option<&str>,
  series_order: Option<i64>,
  editions: Option<&str>,
) -> SqlResult<()> {
  conn.execute(
    "INSERT INTO sources (book_id, kind, ref, position, title, author, cover_url, cover_path, summary, tags, series_name, series_order, editions)
     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
    rusqlite::params![
      book_id, kind, ref_key, position, title, author, cover_url, cover_path, summary, tags,
      series_name, series_order, editions
    ],
  )?;
  if let Some(p) = cover_path {
    cover_ref_add(conn, p)?;
  }
  cover_ref_add_editions(conn, editions)?;
  Ok(())
}

/// 更新书籍标签（来源标签重设）
pub fn set_book_tags(conn: &Connection, book_id: i64, tags: &str) -> SqlResult<()> {
  conn.execute(
    "UPDATE books SET tags = ?1 WHERE id = ?2",
    rusqlite::params![tags, book_id],
  )?;
  Ok(())
}

/// 更新书籍系列（None 系列名 = 清除系列）
pub fn set_book_series(
  conn: &Connection,
  book_id: i64,
  series_name: Option<&str>,
  series_order: Option<i64>,
) -> SqlResult<()> {
  conn.execute(
    "UPDATE books SET series_name = ?1, series_order = ?2 WHERE id = ?3",
    rusqlite::params![series_name, series_order, book_id],
  )?;
  Ok(())
}

// ============================= //
//  WebDav 从云端同步：远端快照整库覆盖
// ============================= //

/// 远端书籍行（merge 用）
struct RemoteBookRow {
  remote_id: i64,
  title: String,
  author: String,
  tags: String,
  file_path: String,
  status: String,
  my_review: String,
  finished_date: Option<String>,
  description: String,
  cover_path: Option<String>,
  series_name: Option<String>,
  series_order: Option<i64>,
  library_file: String,
  clasp_ids: String,
  merged_from: String,
  douban_urls: String,
}

/// 封面路径重定位：按文件名在本地封面缓存中查找（封面文件名为内容哈希，跨设备一致）
///
/// 对端数据库中的封面路径可能是任一平台风格（Windows 反斜杠 / Unix 斜杠），
/// 必须按两种分隔符共同拆分取末段 —— 移动端（Unix）解析 Windows 路径时
/// `Path::file_name` 会把整串路径当作文件名，导致同步后封面全部重定位失败。
fn rebase_cover_path(path: &str, covers_dir: &Path) -> Option<String> {
  let name = path
    .trim_end_matches(|c| c == '/' || c == '\\')
    .rsplit(|c| c == '/' || c == '\\')
    .next()?
    .trim();
  if name.is_empty() {
    return None;
  }
  let local = covers_dir.join(name);
  local.is_file().then(|| local.to_string_lossy().into_owned())
}

/// 从云端覆盖同步的统计
#[derive(Debug, Default, Clone, Copy)]
pub struct ReplaceStats {
  /// 远端已有、本地也已有 → 用远端数据整行覆盖
  pub updated: usize,
  /// 本地缺失 → 整行并入
  pub inserted: usize,
  /// 本地有、远端已无 → 删除（仅限登记过 library_file 的书籍）
  pub deleted: usize,
}

/// 用远端数据库快照**全量覆盖**本地指定书库（WebDav「从云端同步」）：
///
/// - 作用域：`library_file` ∈ `remote_files`（远端书库目录中实际存在的 EPUB 集合）。
///   数据库快照是全量的，唯有远端目录能界定"本次同步的书库"包含哪些书
///   （跨设备书库 ID 并不一致，不能按 library_id 过滤）
/// - 远端书籍按 library_file 匹配本地书籍：命中则整行覆盖（file_path 保留本地值，
///   其为设备侧溯源信息），未命中则整行并入，library_id 一律归属本地目标书库
/// - 本地该书库中远端已不存在的书籍（登记过 library_file 的）整行删除
/// - 每本书的短评与来源项目随远端全量替换；cover_path 按文件名重定位到本地封面缓存
/// - 末尾按当前数据重算封面引用计数（自愈一切漂移）
///
/// 不属于远端文件集合的无 library_file 书籍（历史数据）保持不动。
pub fn replace_library_from_remote(
  conn: &Connection,
  remote: &Connection,
  library_id: &str,
  covers_dir: &Path,
  remote_files: &std::collections::HashSet<String>,
) -> SqlResult<ReplaceStats> {
  let sql = "SELECT id, title, COALESCE(author,''), COALESCE(tags,''), COALESCE(file_path,''),
      COALESCE(status,'想读'), COALESCE(my_review,''), finished_date,
      COALESCE(description,''), cover_path, series_name, series_order,
      COALESCE(library_file,''), COALESCE(clasp_ids,'[]'), COALESCE(merged_from,''),
      COALESCE(douban_urls,'[]')
    FROM books";
  let mut stmt = remote.prepare(sql)?;
  let rows = stmt.query_map([], |r| {
    Ok(RemoteBookRow {
      remote_id: r.get(0)?,
      title: r.get(1)?,
      author: r.get(2)?,
      tags: r.get(3)?,
      file_path: r.get(4)?,
      status: r.get(5)?,
      my_review: r.get(6)?,
      finished_date: r.get(7)?,
      description: r.get(8)?,
      cover_path: r.get(9)?,
      series_name: r.get(10)?,
      series_order: r.get(11)?,
      library_file: r.get(12)?,
      clasp_ids: r.get(13)?,
      merged_from: r.get(14)?,
      douban_urls: r.get(15)?,
    })
  })?;
  let mut remote_books: Vec<RemoteBookRow> = Vec::new();
  for row in rows {
    match row {
      Ok(b) => remote_books.push(b),
      Err(e) => warn!("远端书籍行读取失败，已跳过: {e}"),
    }
  }
  drop(stmt);

  let mut stats = ReplaceStats::default();
  // 单事务执行：原子覆盖 + 全程仅一次写锁获取（避免与 GUI 连接反复争锁）
  let tx = conn.unchecked_transaction()?;
  // 本地目标书库的书籍（library_file → id）
  let mut stmt = tx.prepare(
    "SELECT id, COALESCE(library_file,'') FROM books WHERE library_id = ?1",
  )?;
  let local_rows: Vec<(i64, String)> = stmt
    .query_map(rusqlite::params![library_id], |r| {
      Ok((r.get(0)?, r.get(1)?))
    })?
    .collect::<Result<_, _>>()?;
  drop(stmt);
  let local_map: std::collections::HashMap<String, i64> = local_rows
    .iter()
    .filter(|(_, lf)| !lf.is_empty())
    .map(|(id, lf)| (lf.clone(), *id))
    .collect();

  for b in &remote_books {
    // 只覆盖远端目录中真实存在的书
    if b.library_file.is_empty() || !remote_files.contains(&b.library_file) {
      continue;
    }
    let cover_local = b
      .cover_path
      .as_deref()
      .and_then(|p| rebase_cover_path(p, covers_dir));
    if let Some(local_id) = local_map.get(&b.library_file).copied() {
      // 整行覆盖（file_path 保留本地溯源值，library_id 归属本地书库）
      tx.execute(
        "UPDATE books SET title = ?1, author = ?2, tags = ?3, status = ?4,
                my_review = ?5, finished_date = ?6, description = ?7, cover_path = ?8,
                series_name = ?9, series_order = ?10, clasp_ids = ?11, merged_from = ?12,
                douban_urls = ?13
         WHERE id = ?14",
        rusqlite::params![
          b.title, b.author, b.tags, b.status, b.my_review, b.finished_date,
          b.description, cover_local, b.series_name, b.series_order, b.clasp_ids,
          b.merged_from, b.douban_urls, local_id
        ],
      )?;
      // 短评与来源项目随远端全量替换
      tx.execute(
        "DELETE FROM comments WHERE book_id = ?1",
        rusqlite::params![local_id],
      )?;
      tx.execute(
        "DELETE FROM sources WHERE book_id = ?1",
        rusqlite::params![local_id],
      )?;
      copy_remote_comments(&tx, remote, b.remote_id, local_id)?;
      copy_remote_sources(&tx, remote, b.remote_id, local_id, covers_dir)?;
      stats.updated += 1;
    } else {
      tx.execute(
        "INSERT INTO books (title, author, tags, file_path, status, my_review, finished_date,
                            description, cover_path, series_name, series_order, library_file,
                            clasp_ids, merged_from, douban_urls, library_id)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
        rusqlite::params![
          b.title, b.author, b.tags, b.file_path, b.status, b.my_review, b.finished_date,
          b.description, cover_local, b.series_name, b.series_order, b.library_file,
          b.clasp_ids, b.merged_from, b.douban_urls, library_id
        ],
      )?;
      let new_id = tx.last_insert_rowid();
      copy_remote_comments(&tx, remote, b.remote_id, new_id)?;
      copy_remote_sources(&tx, remote, b.remote_id, new_id, covers_dir)?;
      stats.inserted += 1;
    }
  }

  // 本地该书库中远端已不存在的书 → 整行删除（仅限登记过 library_file 的；
  // 无 library_file 的历史数据不属于文件同步域，保持不动）
  for (id, lf) in &local_rows {
    if lf.is_empty() || remote_files.contains(lf) {
      continue;
    }
    tx.execute(
      "DELETE FROM comments WHERE book_id = ?1",
      rusqlite::params![id],
    )?;
    tx.execute(
      "DELETE FROM sources WHERE book_id = ?1",
      rusqlite::params![id],
    )?;
    tx.execute("DELETE FROM books WHERE id = ?1", rusqlite::params![id])?;
    stats.deleted += 1;
  }

  if stats.updated > 0 || stats.inserted > 0 || stats.deleted > 0 {
    seed_cover_refs(&tx);
  }
  tx.commit()?;
  Ok(stats)
}

/// 拷贝远端某本书的全部短评到本地新书
fn copy_remote_comments(
  conn: &Connection,
  remote: &Connection,
  remote_book_id: i64,
  new_book_id: i64,
) -> SqlResult<()> {
  let mut stmt = remote.prepare(
    "SELECT rating, content, COALESCE(usefulness,0), COALESCE(source,'豆瓣'),
            COALESCE(is_mine,0), source_ref
     FROM comments WHERE book_id = ?1",
  )?;
  let rows = stmt.query_map(rusqlite::params![remote_book_id], |r| {
    Ok((
      r.get::<_, Option<i32>>(0)?,
      r.get::<_, String>(1)?,
      r.get::<_, i32>(2)?,
      r.get::<_, String>(3)?,
      r.get::<_, i32>(4)?,
      r.get::<_, Option<String>>(5)?,
    ))
  })?;
  let mut copied: Vec<(Option<i32>, String, i32, String, i32, Option<String>)> = Vec::new();
  for row in rows.flatten() {
    copied.push(row);
  }
  drop(stmt);
  for (rating, content, usefulness, source, is_mine, source_ref) in copied {
    conn.execute(
      "INSERT INTO comments (book_id, rating, content, usefulness, source, is_mine, source_ref)
       VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
      rusqlite::params![new_book_id, rating, content, usefulness, source, is_mine, source_ref],
    )?;
  }
  Ok(())
}

/// 拷贝远端某本书的全部来源项目到本地新书（封面路径重定位）
fn copy_remote_sources(
  conn: &Connection,
  remote: &Connection,
  remote_book_id: i64,
  new_book_id: i64,
  covers_dir: &Path,
) -> SqlResult<()> {
  let mut stmt = remote.prepare(
    "SELECT kind, ref, position, title, author, cover_url, cover_path, summary,
            tags, series_name, series_order, editions
     FROM sources WHERE book_id = ?1 ORDER BY kind, position",
  )?;
  let rows = stmt.query_map(rusqlite::params![remote_book_id], |r| {
    Ok((
      r.get::<_, String>(0)?,
      r.get::<_, String>(1)?,
      r.get::<_, i64>(2)?,
      r.get::<_, Option<String>>(3)?,
      r.get::<_, Option<String>>(4)?,
      r.get::<_, Option<String>>(5)?,
      r.get::<_, Option<String>>(6)?,
      r.get::<_, Option<String>>(7)?,
      r.get::<_, Option<String>>(8)?,
      r.get::<_, Option<String>>(9)?,
      r.get::<_, Option<i64>>(10)?,
      r.get::<_, Option<String>>(11)?,
    ))
  })?;
  let mut copied: Vec<(String, String, i64, Option<String>, Option<String>, Option<String>, Option<String>, Option<String>, Option<String>, Option<String>, Option<i64>, Option<String>)> = Vec::new();
  for row in rows.flatten() {
    copied.push(row);
  }
  drop(stmt);
  for (kind, r#ref, position, title, author, cover_url, cover_path, summary, tags, series_name, series_order, editions) in copied {
    let cover_local =
      cover_path.as_deref().and_then(|p| rebase_cover_path(p, covers_dir));
    conn.execute(
      "INSERT INTO sources (book_id, kind, ref, position, title, author, cover_url, cover_path,
                            summary, tags, series_name, series_order, editions)
       VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
      rusqlite::params![
        new_book_id, kind, r#ref, position, title, author, cover_url, cover_local,
        summary, tags, series_name, series_order, editions
      ],
    )?;
  }
  Ok(())
}

/// 更新来源项目的系列信息（重设系列时回填旧数据用）
pub fn update_source_series(
  conn: &Connection,
  book_id: i64,
  kind: &str,
  ref_key: &str,
  series_name: Option<&str>,
  series_order: Option<i64>,
) -> SqlResult<()> {
  conn.execute(
    "UPDATE sources SET series_name = ?1, series_order = ?2
     WHERE book_id = ?3 AND kind = ?4 AND ref = ?5",
    rusqlite::params![series_name, series_order, book_id, kind, ref_key],
  )?;
  Ok(())
}

/// 按有序 ref 列表重排某一类来源的 position
pub fn reorder_sources(
  conn: &Connection,
  book_id: i64,
  kind: &str,
  refs_in_order: &[String],
) -> SqlResult<()> {
  for (i, r) in refs_in_order.iter().enumerate() {
    conn.execute(
      "UPDATE sources SET position = ?1 WHERE book_id = ?2 AND kind = ?3 AND ref = ?4",
      rusqlite::params![i as i64, book_id, kind, r],
    )?;
  }
  Ok(())
}

/// 刷新来源项目的元数据（重爬短评时同步更新书名/作者/封面/简介/标签/系列/版本封面）
///
/// 封面与版本封面变化时转移引用；返回被释放且已无引用的旧路径（调用方删文件）。
/// 系列为直接覆盖（爬取结果是权威数据；无系列传 None 表示该来源确无系列信息）。
#[allow(clippy::too_many_arguments)]
pub fn update_source_meta(
  conn: &Connection,
  book_id: i64,
  kind: &str,
  ref_key: &str,
  title: Option<&str>,
  author: Option<&str>,
  cover_url: Option<&str>,
  cover_path: Option<&str>,
  summary: Option<&str>,
  tags: Option<&str>,
  series_name: Option<&str>,
  series_order: Option<i64>,
  editions: Option<&str>,
) -> SqlResult<Vec<String>> {
  let old: Option<(Option<String>, Option<String>, Option<String>)> = conn
    .query_row(
      "SELECT cover_path, tags, editions FROM sources WHERE book_id = ?1 AND kind = ?2 AND ref = ?3",
      rusqlite::params![book_id, kind, ref_key],
      |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )
    .ok();
  conn.execute(
    "UPDATE sources SET
       title        = COALESCE(?1, title),
       author       = COALESCE(?2, author),
       cover_url    = COALESCE(?3, cover_url),
       cover_path   = ?4,
       summary      = COALESCE(?5, summary),
       tags         = COALESCE(?6, tags),
       series_name  = ?7,
       series_order = ?8,
       editions     = ?9
     WHERE book_id = ?10 AND kind = ?11 AND ref = ?12",
    rusqlite::params![
      title, author, cover_url, cover_path, summary, tags,
      series_name, series_order, editions, book_id, kind, ref_key
    ],
  )?;
  let mut orphaned: Vec<String> = Vec::new();
  let (old_cover, _old_tags, old_editions) = match &old {
    Some((c, t, e)) => (c.as_deref(), t.as_deref(), e.as_deref()),
    None => (None, None, None),
  };
  // 主封面路径转移
  if old_cover != cover_path {
    if let Some(old_path) = old_cover {
      if cover_ref_release(conn, old_path)? {
        orphaned.push(old_path.to_string());
      }
    }
    if let Some(p) = cover_path {
      cover_ref_add(conn, p)?;
    }
  }
  // 版本封面集合转移（内容不变时跳过，避免重复增减）
  if old_editions != editions {
    orphaned.extend(cover_ref_release_editions(conn, old_editions)?);
    cover_ref_add_editions(conn, editions)?;
  }
  Ok(orphaned)
}

/// 删除单个来源项目及其抓取的短评
///
/// 返回是否存在；(被释放且已无引用的封面/版本封面路径) 一并返回。
pub fn delete_source(
  conn: &Connection,
  book_id: i64,
  kind: &str,
  ref_key: &str,
) -> SqlResult<(bool, Vec<String>)> {
  let row: Option<(Option<String>, Option<String>)> = conn
    .query_row(
      "SELECT cover_path, editions FROM sources WHERE book_id = ?1 AND kind = ?2 AND ref = ?3",
      rusqlite::params![book_id, kind, ref_key],
      |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .ok();
  let n = conn.execute(
    "DELETE FROM sources WHERE book_id = ?1 AND kind = ?2 AND ref = ?3",
    rusqlite::params![book_id, kind, ref_key],
  )?;
  let mut orphaned: Vec<String> = Vec::new();
  if let Some((cover, editions)) = row {
    if let Some(p) = cover {
      if cover_ref_release(conn, &p)? {
        orphaned.push(p);
      }
    }
    orphaned.extend(cover_ref_release_editions(conn, editions.as_deref())?);
  }
  delete_comments_for_source(conn, book_id, ref_key)?;
  Ok((n > 0, orphaned))
}

/// 清空某一类来源项目及其抓取的短评
///
/// 返回被释放且已无引用的封面路径（调用方删文件）。
pub fn clear_sources(conn: &Connection, book_id: i64, kind: &str) -> SqlResult<Vec<String>> {
  let rows = list_sources(conn, book_id)?;
  conn.execute(
    "DELETE FROM sources WHERE book_id = ?1 AND kind = ?2",
    rusqlite::params![book_id, kind],
  )?;
  let mut orphaned: Vec<String> = Vec::new();
  for s in rows.iter().filter(|s| s.kind == kind) {
    if let Some(p) = s.cover_path.as_deref() {
      if cover_ref_release(conn, p)? {
        orphaned.push(p.to_string());
      }
    }
    orphaned.extend(cover_ref_release_editions(conn, s.editions.as_deref())?);
    delete_comments_for_source(conn, book_id, &s.ref_key)?;
  }
  Ok(orphaned)
}

/// 删除某个来源项目抓取的短评（保留个人/AI 书评）
pub fn delete_comments_for_source(
  conn: &Connection,
  book_id: i64,
  source_ref: &str,
) -> SqlResult<usize> {
  conn.execute(
    "DELETE FROM comments WHERE book_id = ?1 AND source_ref = ?2 AND COALESCE(is_mine, 0) = 0",
    rusqlite::params![book_id, source_ref],
  )
}

/// 用来源项目回写 books.clasp_ids / douban_urls（保持旧字段与来源模块一致）
pub fn sync_book_source_columns(conn: &Connection, book_id: i64) -> SqlResult<()> {
  let rows = list_sources(conn, book_id)?;
  let clasp: Vec<String> = rows
    .iter()
    .filter(|s| s.kind == "clasp")
    .map(|s| s.ref_key.clone())
    .collect();
  let douban: Vec<String> = rows
    .iter()
    .filter(|s| s.kind == "douban")
    .map(|s| s.ref_key.clone())
    .collect();
  conn.execute(
    "UPDATE books SET clasp_ids = ?1, douban_urls = ?2 WHERE id = ?3",
    rusqlite::params![
      serde_json::to_string(&clasp).unwrap_or_default(),
      serde_json::to_string(&douban).unwrap_or_default(),
      book_id
    ],
  )?;
  Ok(())
}

// ============================= //
//  元数据编辑 / LLM 用量统计
// ============================= //

/// 更新书籍元数据（GUI 详情页编辑：书名/作者/标签/简介）
pub fn update_book_meta(
  conn: &Connection,
  book_id: i64,
  title: &str,
  author: &str,
  tags: &str,
  description: Option<&str>,
) -> SqlResult<()> {
  conn.execute(
    "UPDATE books SET title = ?1, author = ?2, tags = ?3, description = ?4 WHERE id = ?5",
    rusqlite::params![title, author, tags, description, book_id],
  )?;
  Ok(())
}

/// 更新书籍状态（想读/在读/已读）；标记已读时记录完成时间
pub fn set_book_status(conn: &Connection, book_id: i64, status: &str) -> SqlResult<()> {
  conn.execute(
    "UPDATE books SET
       status = ?1,
       finished_date = CASE WHEN ?1 = '已读' THEN datetime('now','localtime') ELSE finished_date END
     WHERE id = ?2",
    rusqlite::params![status, book_id],
  )?;
  Ok(())
}

/// 累计一次 LLM 调用的 token 用量（按模型 UPSERT 累加）
pub fn record_llm_usage(
  conn: &Connection,
  model: &str,
  prompt_tokens: u64,
  completion_tokens: u64,
  total_tokens: u64,
) -> SqlResult<()> {
  conn.execute(
    "INSERT INTO llm_usage (model, calls, prompt_tokens, completion_tokens, total_tokens, last_used)
     VALUES (?1, 1, ?2, ?3, ?4, datetime('now','localtime'))
     ON CONFLICT(model) DO UPDATE SET
       calls             = calls + 1,
       prompt_tokens     = prompt_tokens + ?2,
       completion_tokens = completion_tokens + ?3,
       total_tokens      = total_tokens + ?4,
       last_used         = datetime('now','localtime')",
    rusqlite::params![model, prompt_tokens as i64, completion_tokens as i64, total_tokens as i64],
  )?;
  Ok(())
}

/// 单个模型的 token 用量统计行
#[derive(Serialize, Debug)]
pub struct LlmUsageRow {
  pub model: String,
  pub calls: i64,
  pub prompt_tokens: i64,
  pub completion_tokens: i64,
  pub total_tokens: i64,
  pub last_used: Option<String>,
}

/// 查询各模型的 token 用量（按总用量降序）
pub fn get_llm_usage(conn: &Connection) -> SqlResult<Vec<LlmUsageRow>> {
  let mut stmt = conn.prepare(
    "SELECT model, COALESCE(calls,0), COALESCE(prompt_tokens,0), COALESCE(completion_tokens,0),
            COALESCE(total_tokens,0), CAST(last_used AS TEXT)
     FROM llm_usage ORDER BY total_tokens DESC",
  )?;
  let rows = stmt.query_map([], |row| {
    Ok(LlmUsageRow {
      model: row.get(0)?,
      calls: row.get(1)?,
      prompt_tokens: row.get(2)?,
      completion_tokens: row.get(3)?,
      total_tokens: row.get(4)?,
      last_used: row.get(5)?,
    })
  })?;
  rows.collect()
}

/// 全部模型的累计 token 总量（预算检查 / 设置页预算进度条用）
pub fn llm_usage_total(conn: &Connection) -> SqlResult<i64> {
  conn.query_row("SELECT COALESCE(SUM(total_tokens), 0) FROM llm_usage", [], |r| {
    r.get(0)
  })
}

/// 清零用量统计（设置页「清零用量」按钮；预算周期重置用）
pub fn reset_llm_usage(conn: &Connection) -> SqlResult<()> {
  conn.execute("DELETE FROM llm_usage", [])?;
  Ok(())
}

/// 按数据库路径读取累计 token 总量（agent 预算检查用：无 Connection 上下文）
///
/// 打开只读连接做短查询；文件不存在 / 读失败返回 0（统计不可用不应阻断调用）。
pub fn llm_usage_total_at(path: &Path) -> i64 {
  let Ok(conn) = Connection::open(path) else {
    return 0;
  };
  conn
    .query_row("SELECT COALESCE(SUM(total_tokens), 0) FROM llm_usage", [], |r| {
      r.get(0)
    })
    .unwrap_or(0)
}

#[cfg(test)]
mod tests {
  use super::*;

  fn test_conn() -> Connection {
    open_db(":memory:").unwrap()
  }

  /// 状态切换：已读记录 finished_date，其他状态不覆盖
  #[test]
  fn test_set_book_status() {
    let conn = test_conn();
    let id = insert_book(&conn, "白夜行", "东野圭吾", "推理小说", "E:\\b.epub", "[]").unwrap();

    set_book_status(&conn, id, "在读").unwrap();
    let d = get_book_detail(&conn, id).unwrap().unwrap();
    assert_eq!(d.status, "在读");
    assert!(d.finished_date.is_none());

    set_book_status(&conn, id, "已读").unwrap();
    let d = get_book_detail(&conn, id).unwrap().unwrap();
    assert_eq!(d.status, "已读");
    assert!(d.finished_date.is_some());
  }

  /// LLM 用量按模型累加
  #[test]
  fn test_record_llm_usage() {
    let conn = test_conn();
    record_llm_usage(&conn, "OpenAI/gpt-4o", 100, 50, 150).unwrap();
    record_llm_usage(&conn, "OpenAI/gpt-4o", 30, 20, 50).unwrap();
    record_llm_usage(&conn, "DeepSeek/deepseek-chat", 10, 5, 15).unwrap();

    let rows = get_llm_usage(&conn).unwrap();
    assert_eq!(rows.len(), 2);
    let gpt = rows.iter().find(|r| r.model == "OpenAI/gpt-4o").unwrap();
    assert_eq!(gpt.calls, 2);
    assert_eq!(gpt.prompt_tokens, 130);
    assert_eq!(gpt.total_tokens, 200);
  }

  /// 匹配/豆瓣链接更新与豆瓣短评清理：个人/AI 书评保留
  #[test]
  fn test_match_urls_and_douban_comments() {
    let conn = test_conn();
    let id = insert_book(&conn, "白夜行", "东野圭吾", "推理小说", "E:\\b.epub", "[]").unwrap();

    // 豆瓣链接入库 + clasp_ids 保持原值（None）
    set_book_match_urls(
      &conn,
      id,
      None,
      r#"["https://book.douban.com/subject/1/"]"#,
    )
    .unwrap();
    let d = get_book_detail(&conn, id).unwrap().unwrap();
    assert_eq!(d.clasp_ids.as_deref(), Some("[]"));
    assert_eq!(
      d.douban_urls.as_deref(),
      Some(r#"["https://book.douban.com/subject/1/"]"#)
    );

    // 插入豆瓣短评与个人书评
    insert_comment(&conn, id, Some(4), "豆瓣短评内容足够长，用于测试过滤逻辑。", 10, "豆瓣", None).unwrap();
    insert_comment(&conn, id, None, "我的个人书评。", 0, "AI助手生成", None).unwrap();
    // is_mine 由 insert_comment 的 source 参数之外的字段控制：手动置位个人评论
    conn
      .execute(
        "UPDATE comments SET is_mine = 1 WHERE source != '豆瓣'",
        [],
      )
      .unwrap();

    // 清理：仅删除豆瓣短评，个人书评保留
    let deleted = delete_douban_comments(&conn, id).unwrap();
    assert_eq!(deleted, 1);
    let rows = get_comments_for_book(&conn, id).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].is_mine, 1);
  }

  /// 封面引用计数：多引用递减、归零删除语义（文件删除由调用方执行）
  #[test]
  fn test_cover_ref_lifecycle() {
    let conn = test_conn();
    let id = insert_book(&conn, "白夜行", "东野圭吾", "推理小说", "E:\\b.epub", "[]").unwrap();
    let p = "C:\\covers\\a.jpg";

    // 两个引用（如：两个来源共用同一封面文件）
    cover_ref_add(&conn, p).unwrap();
    cover_ref_add(&conn, p).unwrap();

    // 释放一次 → 仍有引用，不删文件
    assert!(!cover_ref_release(&conn, p).unwrap());
    // 释放第二次 → 归零，调用方应删文件
    assert!(cover_ref_release(&conn, p).unwrap());

    // 未登记路径（旧数据）：无存活引用 → 释放即归零
    assert!(cover_ref_release(&conn, "C:\\covers\\legacy.jpg").unwrap());

    // 未登记路径但有存活引用（书籍仍指向该文件）→ 双保险不归零
    let p2 = "C:\\covers\\legacy2.jpg";
    conn
      .execute(
        "UPDATE books SET cover_path = ?1 WHERE id = ?2",
        rusqlite::params![p2, id],
      )
      .unwrap();
    assert!(!cover_ref_release(&conn, p2).unwrap());
  }

  /// 打开（或创建）本地 SQLite 数据库的 VACUUM INTO 快照：
  /// 绑定参数路径可用，产出的快照文件可直接独立打开（推送数据库快照的机制）
  #[test]
  fn test_vacuum_into_snapshot() {    let conn = test_conn();
    insert_book(&conn, "白夜行", "东野圭吾", "推理小说", "E:\\b.epub", "[]").unwrap();
    let tmp = std::env::temp_dir().join(format!("mna-vacuum-test-{}.db", std::process::id()));
    let _ = std::fs::remove_file(&tmp);
    conn
      .execute(
        "VACUUM INTO ?1",
        rusqlite::params![tmp.to_string_lossy().as_ref()],
      )
      .unwrap();
    let snap = Connection::open(tmp.to_string_lossy().as_ref()).unwrap();
    let n: i64 = snap
      .query_row("SELECT COUNT(*) FROM books", [], |r| r.get(0))
      .unwrap();
    assert_eq!(n, 1, "快照包含全部数据");
    let _ = std::fs::remove_file(&tmp);
  }

  /// 封面路径重定位：跨平台分隔符（Windows 反斜杠 / Unix 斜杠 / 裸文件名）
  ///
  /// 回归：移动端（Unix）同步桌面端（Windows）数据库快照时，
  /// `Path::file_name` 不识别反斜杠 → 重定位全部失败 → 封面不显示。
  #[test]
  fn test_rebase_cover_path_cross_platform() {
    let dir = std::env::temp_dir().join(format!("mna-rebase-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("abc.jpg"), b"x").unwrap();
    let expected = dir.join("abc.jpg").to_string_lossy().into_owned();

    assert_eq!(
      rebase_cover_path(r"E:\书库\covers\abc.jpg", &dir).as_deref(),
      Some(expected.as_str()),
      "Windows 反斜杠路径"
    );
    assert_eq!(
      rebase_cover_path("/srv/covers/abc.jpg", &dir).as_deref(),
      Some(expected.as_str()),
      "Unix 斜杠路径"
    );
    assert_eq!(
      rebase_cover_path("abc.jpg", &dir).as_deref(),
      Some(expected.as_str()),
      "裸文件名"
    );
    assert_eq!(
      rebase_cover_path(r"E:\covers\missing.jpg", &dir),
      None,
      "本地不存在的文件 → None"
    );
    assert_eq!(rebase_cover_path("", &dir), None);

    let _ = std::fs::remove_dir_all(&dir);
  }

  /// 从云端覆盖同步：整行覆盖 / 并入 / 移除，其他书库与无 library_file 书籍不受影响
  #[test]
  fn test_replace_library_from_remote() {
    let local = test_conn();
    let remote = test_conn();

    // 本地书库 L：a.epub（含旧短评）、b.epub；另有无 library_file 的历史数据与书库 M 的书
    let a = insert_book(&local, "旧标题A", "旧作者", "推理小说", "E:\\a.epub", "[]").unwrap();
    set_book_library(&local, a, "L").unwrap();
    update_book_enrichment(&local, a, None, None, None, None, Some("a.epub"), Some("[]")).unwrap();
    insert_comment(&local, a, Some(3), "本地旧短评，长度足够。", 5, "豆瓣", None).unwrap();

    let b = insert_book(&local, "标题B", "作者B", "推理小说", "E:\\b.epub", "[]").unwrap();
    set_book_library(&local, b, "L").unwrap();
    update_book_enrichment(&local, b, None, None, None, None, Some("b.epub"), Some("[]")).unwrap();

    let legacy = insert_book(&local, "历史遗留", "佚名", "推理小说", "", "[]").unwrap();
    set_book_library(&local, legacy, "L").unwrap();

    let m = insert_book(&local, "他库书籍", "作者M", "推理小说", "E:\\m.epub", "[]").unwrap();
    set_book_library(&local, m, "M").unwrap();

    // 云端：a.epub（覆盖，含两条新短评）、c.epub（新增）；无 b.epub
    let ra = insert_book(&remote, "云端标题A", "云端作者A", "推理小说,日系", "S:\\a.epub", "[]").unwrap();
    remote
      .execute(
        "UPDATE books SET library_file = 'a.epub' WHERE id = ?1",
        rusqlite::params![ra],
      )
      .unwrap();
    insert_comment(&remote, ra, Some(5), "云端短评一，长度足够。", 9, "豆瓣", None).unwrap();
    insert_comment(&remote, ra, Some(4), "云端短评二，长度足够。", 3, "豆瓣", None).unwrap();
    let rc = insert_book(&remote, "云端新书C", "作者C", "推理小说", "", "[]").unwrap();
    remote
      .execute(
        "UPDATE books SET library_file = 'c.epub' WHERE id = ?1",
        rusqlite::params![rc],
      )
      .unwrap();

    // 本地封面缓存（重定位验证：云端路径仅文件名有效）
    let covers = std::env::temp_dir().join(format!("mna-test-covers-{}", std::process::id()));
    std::fs::create_dir_all(&covers).unwrap();
    std::fs::write(covers.join("cover-a.jpg"), b"x").unwrap();
    remote
      .execute(
        "UPDATE books SET cover_path = '/srv/covers/cover-a.jpg' WHERE id = ?1",
        rusqlite::params![ra],
      )
      .unwrap();

    let files: std::collections::HashSet<String> =
      ["a.epub".to_string(), "c.epub".to_string()].into();
    let stats = replace_library_from_remote(&local, &remote, "L", &covers, &files).unwrap();
    assert_eq!(stats.updated, 1, "a.epub 整行覆盖");
    assert_eq!(stats.inserted, 1, "c.epub 并入");
    assert_eq!(stats.deleted, 1, "b.epub 远端已无 → 删除");

    // a.epub：云端数据覆盖，file_path 保留本地溯源值，短评全量替换
    let cards = get_book_cards(&local, None, None, Some("L")).unwrap();
    let ca = cards.iter().find(|c| c.id == a).unwrap();
    assert_eq!(ca.title, "云端标题A");
    assert_eq!(ca.author, "云端作者A");
    let detail = get_book_detail(&local, a).unwrap().unwrap();
    assert_eq!(detail.file_path.as_deref(), Some("E:\\a.epub"));
    assert!(detail
      .cover_path
      .as_deref()
      .unwrap_or("")
      .ends_with("cover-a.jpg"));
    let comments = get_comments_for_book(&local, a).unwrap();
    assert_eq!(comments.len(), 2, "本地旧短评被云端两条替换");
    assert!(!comments.iter().any(|c| c.content.contains("本地旧短评")));

    // b.epub 已删除；c.epub 入库且归属本地书库 L
    assert!(!cards.iter().any(|c| c.id == b));
    assert!(cards.iter().any(|c| c.title == "云端新书C"));

    // 无 library_file 的历史数据与其他书库书籍保持不动
    let legacy_detail = get_book_detail(&local, legacy).unwrap().unwrap();
    assert_eq!(legacy_detail.title, "历史遗留");
    let m_cards = get_book_cards(&local, None, None, Some("M")).unwrap();
    assert_eq!(m_cards.len(), 1);

    let _ = std::fs::remove_dir_all(&covers);
  }
}
