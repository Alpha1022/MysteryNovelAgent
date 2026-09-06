use std::sync::Mutex;

use mystery_novel_agent::db;
use tauri::Manager;

mod commands;

/// 应用入口（桌面二进制与 Android/iOS 移动端共用）
#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
  tauri::Builder::default()
    .plugin(tauri_plugin_opener::init())
    .plugin(tauri_plugin_dialog::init())
    .setup(|app| {
      // Android 等无 HOME 环境：dirs:: 系列失效，用 Tauri 应用私有目录重定向
      // config/database/临时文件路径（桌面端不注入，维持原布局）
      #[cfg(mobile)]
      if let Some(dir) = app.path().app_data_dir().ok() {
        mystery_novel_agent::config::set_data_dir_override(dir);
      }

      let cfg = mystery_novel_agent::config::AppConfig::load();
      let db_path = cfg.database_file();
      let conn = db::open_db(&db_path.to_string_lossy())
        .map_err(|e| format!("无法打开数据库: {e}"))?;

      // 启动清扫：封面引用计数已重算，删除缓存目录中无主的历史文件
      if let Ok(dir) = cfg.covers_dir() {
        commands::cleanup_covers_dir(&conn, &dir);
      }
      // 旧数据归入当前书库（多书库迁移）
      if let Some(lib_id) = cfg.current_library_id() {
        let _ = db::assign_legacy_books(&conn, lib_id);
      }

      app.manage(Mutex::new(conn));
      app.manage(commands::TaskRegistry::default());
      app.manage(commands::SyncTaskRegistry::default());
      app.manage(commands::ImportCache::default());
      Ok(())
    })
    .invoke_handler(tauri::generate_handler![
      commands::get_books,
      commands::get_book_detail,
      commands::get_comments,
      commands::get_config,
      commands::get_reader_content,
      commands::analyze_epub,
      commands::import_epub,
      commands::prepare_import,
      commands::commit_import,
      commands::set_pending_cover,
      commands::discard_import,
      commands::translate_text,
      commands::delete_book,
      commands::open_book_file,
      commands::upload_cover,
      commands::fetch_epub_cover,
      commands::list_sources,
      commands::add_clasp_sources,
      commands::add_douban_sources,
      commands::save_source_order,
      commands::delete_source,
      commands::clear_sources,
      commands::refresh_source_comments,
      commands::refresh_source_meta,
      commands::reset_book_tags,
      commands::reset_book_series,
      commands::set_source_as_cover,
      commands::set_source_as_description,
      commands::merge_source_descriptions,
      commands::merge_books_gui,
      commands::chatbot_chat,
      commands::add_library,
      commands::save_library,
      commands::delete_library,
      commands::switch_library,
      commands::change_library_path,
      commands::search_clasp,
      commands::fetch_cover_image,
      commands::read_cover_file,
      commands::collect_epubs,
      commands::update_book_meta,
      commands::get_settings,
      commands::save_settings,
      commands::llm_status,
      commands::cancel_task,
      commands::cancel_sync_task,
      commands::set_book_status,
      commands::llm_chat,
      commands::save_book_review,
      commands::webdav_test,
      commands::webdav_push,
      commands::webdav_pull,
      commands::is_mobile,
      commands::fs_roots,
      commands::list_fs_dir,
      commands::create_fs_dir,
      commands::write_text_file,
      commands::read_text_file,
    ])
    .build(tauri::generate_context!())
    .expect("error while building tauri application")
    .run(|_app_handle, event| {
      // 退出时再清扫一次无主封面（覆盖运行期间产生的孤儿文件）
      if let tauri::RunEvent::Exit = event {
        // 退出清扫（独立连接，避免与 managed 状态的生命周期纠缠）
        let cfg = mystery_novel_agent::config::AppConfig::load();
        let dir = cfg.covers_dir().ok();
        let db_path = cfg.database_file();
        if let (Some(dir), Some(conn)) = (
          dir,
          mystery_novel_agent::db::open_db(&db_path.to_string_lossy()).ok(),
        ) {
          commands::cleanup_covers_dir(&conn, &dir);
        }
      }
    });
}
