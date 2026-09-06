use std::path::{Path, PathBuf};

use dialoguer::{Confirm, MultiSelect, Select};
use rusqlite::Connection;
use tracing::{info, warn};

use crate::config::AppConfig;
use crate::db;

/// 检测到的设备
pub struct Device {
  /// 设备类型标签（Kindle / Kobo / USB）
  pub kind_label: String,
  /// 挂载点
  pub mount_point: PathBuf,
  /// EPUB 目标目录
  pub target_dir: PathBuf,
}

impl Device {
  fn display(&self) -> String {
    format!("[{}] {}", self.kind_label, self.mount_point.display())
  }
}

/// 枚举可移动磁盘并识别设备类型（跨平台）
///
/// - Linux:  /media/$USER/* 或 /run/media/$USER/*
/// - Windows: 可移动盘符
/// - macOS:  /Volumes/*
///
/// 识别规则：
/// - Kindle: 存在 `documents/` + `system/` 目录 → 目标 `documents/`
/// - Kobo:   存在 `.kobo/` 目录 → 目标根目录
/// - 其他可移动磁盘 → 目标根目录
pub fn detect_devices() -> Vec<Device> {
  let disks = sysinfo::Disks::new_with_refreshed_list();
  let mut out = Vec::new();

  for d in disks.list() {
    let mp = d.mount_point();

    // 跳过根分区与系统盘
    if mp == Path::new("/") {
      continue;
    }

    let is_kindle = mp.join("documents").is_dir() && mp.join("system").is_dir();
    let is_kobo = mp.join(".kobo").is_dir();

    if is_kindle {
      out.push(Device {
        kind_label: "Kindle".into(),
        mount_point: mp.to_path_buf(),
        target_dir: mp.join("documents"),
      });
    } else if is_kobo {
      out.push(Device {
        kind_label: "Kobo".into(),
        mount_point: mp.to_path_buf(),
        target_dir: mp.to_path_buf(),
      });
    } else if d.is_removable() {
      out.push(Device {
        kind_label: "USB".into(),
        mount_point: mp.to_path_buf(),
        target_dir: mp.to_path_buf(),
      });
    }
  }

  info!("检测到 {} 个可同步设备", out.len());
  out
}

/// sync 命令入口：检测设备 → 选设备 → 选书 → 复制
pub async fn run_sync(conn: &Connection) -> anyhow::Result<()> {
  // 1. 设备检测
  let devices = detect_devices();
  if devices.is_empty() {
    anyhow::bail!(
      "未检测到可同步的 USB 设备。\n\
       提示：MTP 协议设备（新款 Kindle 等）不受支持，\
       请将设备切换为 USB 大容量存储模式，或手动复制书库文件。"
    );
  }

  let device_items: Vec<String> = devices.iter().map(|d| d.display()).collect();
  let sel = Select::new()
    .with_prompt("选择目标设备")
    .items(&device_items)
    .default(0)
    .interact()?;
  let device = &devices[sel];

  // 2. 书库书籍（有 library_file 的）
  let cfg = AppConfig::load();
  let lib = cfg.require_library_path()?;
  let books = db::get_all_books(conn)?;
  let with_files: Vec<_> = books
    .into_iter()
    .filter(|b| !b.library_file.is_empty())
    .collect();

  if with_files.is_empty() {
    anyhow::bail!("书库中没有可同步的书籍（需先通过 add 导入）。");
  }

  // 3. 选择要同步的书
  let prompts: Vec<String> = with_files
    .iter()
    .map(|b| format!("《{}》- {}", b.title, b.author))
    .collect();
  let picks = MultiSelect::new()
    .with_prompt("选择要同步的书籍（空格勾选，回车确认）")
    .items(&prompts)
    .interact()?;

  if picks.is_empty() {
    println!("未选择任何书籍，已退出。");
    return Ok(());
  }

  // 4. 确认目标
  let total = picks.len();
  if !Confirm::new()
    .with_prompt(format!(
      "将 {} 本书复制到 {} ？",
      total,
      device.target_dir.display()
    ))
    .default(true)
    .interact()?
  {
    println!("已取消。");
    return Ok(());
  }

  // 5. 逐本复制
  let mut ok_cnt = 0usize;
  let mut err_cnt = 0usize;
  for i in picks {
    let b = &with_files[i];
    let src = lib.join(&b.library_file);
    if !src.exists() {
      warn!("文件缺失: {}", src.display());
      println!("  《{}》... 文件缺失，跳过", b.title);
      err_cnt += 1;
      continue;
    }
    let dest = device.target_dir.join(&b.library_file);
    print!("  《{}》... ", b.title);
    match std::fs::copy(&src, &dest) {
      Ok(_) => {
        println!("OK");
        ok_cnt += 1;
      }
      Err(e) => {
        println!("失败: {e}");
        warn!("同步失败 [{}]: {e}", b.title);
        err_cnt += 1;
      }
    }
  }

  println!("\n同步完成：成功 {ok_cnt} 本，失败 {err_cnt} 本");
  println!("提示：安全弹出设备前请等待文件写入完成。");
  Ok(())
}
