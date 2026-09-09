package com.mystery_novel_agent

import android.app.Activity
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.Environment
import android.provider.Settings
import app.tauri.annotation.Command
import app.tauri.annotation.TauriPlugin
import app.tauri.plugin.Invoke
import app.tauri.plugin.JSObject
import app.tauri.plugin.Plugin

/**
 * 「所有文件访问」（MANAGE_EXTERNAL_STORAGE，Android 11+ 特殊权限）：
 * 无法弹窗请求，只能引导用户到系统设置开关。
 * - hasAllFilesAccess：检测是否已授权
 * - openAllFilesAccessSettings：跳转本应用的授权设置页
 */
@TauriPlugin
class StoragePermissionPlugin(private val activity: Activity) : Plugin(activity) {

    @Command
    fun hasAllFilesAccess(invoke: Invoke) {
        val granted = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            Environment.isExternalStorageManager()
        } else {
            // Android 10 及以下走清单中声明的传统存储权限；
            // 未授权时由同步的可写性预检给出指引
            true
        }
        val ret = JSObject()
        ret.put("granted", granted)
        invoke.resolve(ret)
    }

    @Command
    fun openAllFilesAccessSettings(invoke: Invoke) {
        try {
            val intent = Intent(Settings.ACTION_MANAGE_APP_ALL_FILES_ACCESS_PERMISSION)
            intent.data = Uri.parse("package:${activity.packageName}")
            intent.flags = Intent.FLAG_ACTIVITY_NEW_TASK
            activity.startActivity(intent)
            invoke.resolve()
        } catch (ex: Exception) {
            invoke.reject(ex.message)
        }
    }
}
