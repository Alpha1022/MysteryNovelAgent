//! 「所有文件访问」（MANAGE_EXTERNAL_STORAGE）权限支撑：
//! 移动端检测授权状态并跳转系统设置页。
//!
//! Android 11+ 的特殊权限无法以运行时弹窗请求，需引导用户手动开关；
//! 桌面端无此概念，命令恒返回已授权。
//!
//! 模式参照 tauri-plugin-opener：Rust 内联插件注册 Android Kotlin 插件类，
//! 命令经 `PluginHandle::run_mobile_plugin_async` 调用 Kotlin `@Command` 方法。

use tauri::plugin::{Builder, TauriPlugin};
use tauri::Manager;

#[cfg(target_os = "android")]
use tauri::plugin::PluginHandle;
#[cfg(target_os = "android")]
const PLUGIN_IDENTIFIER: &str = "com.mystery_novel_agent";

/// 插件状态：持有移动端插件句柄（桌面端为空标记）
pub struct StoragePermissionState {
  #[cfg(target_os = "android")]
  handle: PluginHandle<tauri::Wry>,
}

pub fn init() -> TauriPlugin<tauri::Wry> {
  Builder::<tauri::Wry>::new("storage-permission")
    .setup(|app, api| {
      #[cfg(target_os = "android")]
      {
        let handle =
          api.register_android_plugin(PLUGIN_IDENTIFIER, "StoragePermissionPlugin")?;
        app.manage(StoragePermissionState { handle });
      }
      #[cfg(not(target_os = "android"))]
      {
        let _ = api;
        app.manage(StoragePermissionState {});
      }
      Ok(())
    })
    .build()
}

/// 是否已授予「所有文件访问」权限（Android 11+ 检测；桌面端/旧版本恒 true）
#[tauri::command]
pub async fn has_all_files_access(app: tauri::AppHandle<tauri::Wry>) -> Result<bool, String> {
  #[cfg(target_os = "android")]
  {
    let state = app.state::<StoragePermissionState>();
    let result: serde_json::Value = state
      .handle
      .run_mobile_plugin_async("hasAllFilesAccess", serde_json::json!({}))
      .await
      .map_err(|e| e.to_string())?;
    Ok(result
      .get("granted")
      .and_then(|v| v.as_bool())
      .unwrap_or(false))
  }
  #[cfg(not(target_os = "android"))]
  {
    let _ = app;
    Ok(true)
  }
}

/// 打开本应用的「所有文件访问」系统授权页（仅移动端有效；用户开关后返回重试同步）
#[tauri::command]
pub async fn open_all_files_access_settings(
  app: tauri::AppHandle<tauri::Wry>,
) -> Result<(), String> {
  #[cfg(target_os = "android")]
  {
    let state = app.state::<StoragePermissionState>();
    state
      .handle
      .run_mobile_plugin_async::<serde_json::Value>(
        "openAllFilesAccessSettings",
        serde_json::json!({}),
      )
      .await
      .map_err(|e| e.to_string())?;
    Ok(())
  }
  #[cfg(not(target_os = "android"))]
  {
    let _ = app;
    Ok(())
  }
}
