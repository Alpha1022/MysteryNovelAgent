// 桌面入口薄壳：实际应用构建在 lib.rs（Android/iOS 以 cdylib 加载同一入口）
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
  mystery_gui_lib::run()
}
