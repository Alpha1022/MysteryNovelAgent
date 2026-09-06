# 架构文档 — 推理小说阅读管理助手

> 最后更新：2026-09 · Rust 2024 edition · 单二进制 CLI

## 1. 项目概述

本地优先的推理小说 EPUB 管理工具，核心能力：

| 能力 | 命令 | 说明 |
| :--- | :--- | :--- |
| 导入增强 | `add <epub\|目录>` | claspclub 匹配 → 元数据/简介/封面/系列增强 → 拼音命名入书库 |
| 阅读管理 | `finish` | 标记读完 + 苏格拉底式 AI 对话生成个人书评 |
| 书库查询 | `list` / `delete` | 表格展示 / 删除 |
| 设备同步 | `sync` | 检测 USB 阅读器（Kindle/Kobo/通用 U 盘）并复制书籍 |
| 系列合并 | `merge --series\|--ids` | 多本 EPUB 合一 + LLM 融合简介 |

设计原则：**本地优先 · 原文件不动 · 优雅降级**。

## 2. 模块架构

```text
src/
├── main.rs        (250)  clap CLI 入口：命令路由 + list/library 配置 + 批量 add 循环
├── ingestion.rs  (1500+)  【核心】导入流水线（搜索→选择→增强→写库副本→入库）
│                         CLI 交互式入口：ingest_book；GUI 非交互式入口：
│                         analyze_epub / search_clasp_page / prepare_import（可打断 + 进度）/ persist_import
│                         inherit_series：系列继承规则（CLI/GUI/来源重设共用）
├── spider.rs      (550)  claspclub API 客户端（建议/分页搜索/详情）+ 豆瓣短评解析/过滤（top5）
├── agent.rs       (331)  LLM 客户端（多 Provider 解析）+ combine_descriptions + TokenUsage
├── finish.rs      (227)  标记读完 + 多轮 AI 书评对话（含无 LLM 手动降级）
├── db.rs          (1790) SQLite 建表/迁移 + 全部查询函数 + llm_usage 统计 + 状态切换 + 远端快照整库覆盖
├── config.rs      (200)  AppConfig（config.toml 读写，多 Provider llm 段 + 多书库 + WebDav）
├── library.rs      (98)  拼音命名 slugify + 复制入库 + 冲突处理
├── device.rs      (150)  sysinfo 磁盘枚举 + 设备识别 + sync 交互
├── webdav.rs      (860)  WebDav 手动同步（MKCOL/PROPFIND/PUT/DELETE；推送=本地覆盖云端，拉取=云端覆盖本地）
├── reader.rs      (250)  内置阅读器提取（spine 章节 + 消毒 + 图片 data URI 内联）
├── merge.rs       (269)  EPUB 系列合并（资源前缀 + 引用重写 + LLM 简介）
├── utils.rs        (65)  汉字计数 / 标签拆分合并 / 文件名提取 / <title> 提取
└── commands/mod.rs (1)   子命令模块占位
```

依赖分层（无环）：`main → ingestion → {spider, agent, db, config, library}`；
`merge → {agent, db, config, library}`；`device → {db, config}`。

## 3. add 导入流水线（核心数据流）

```text
EPUB 文件（原文件全程只读）
  │  1. 提取 <dc:title> / 文件名 → 关键词候选
  ▼
claspclub 搜索建议 API（逐候选尝试）
  │
  ├─ 多结果 ──▶ 交互菜单（选择 / 手动输入 / 合并本多选 / 取消）
  ├─ 无结果 ──▶ 手动录入书名+作者
  └─ 唯一结果 + batch 模式 ──▶ 自动静默（跳过全部确认与编辑）
  ▼
claspclub 详情 API（逐条目）
  ├─ summaryNoSpoiler → 简介（多条走 LLM 融合，无 LLM 降级拼接）
  ├─ coverUrl        → 封面（OSS 带 Referer 伪装 → 豆瓣 og:image 兜底）
  ├─ series{name,order} → 系列（所有条目系列一致才自动沿用，缺卷号仅继承系列名）
  └─ editions[].doubanUrl → 豆瓣精准链接（优先于封面推断）
  ▼
书名规范化：繁体自动转简体；非中文标题弹框确认（批量也不例外）
  ▼
标签兜底：无"推理小说"标签则追加
  ▼
library::copy_into_library：复制原文件 → 拼音命名
  格式：[系列拼音-N] 作者拼音-书名拼音.epub（冲突加 -2）
  ▼
元数据写入【书库副本】（标题/作者/标签/简介/系列/封面，无确认无备份）
  ▼
SQLite 入库：books + update_book_enrichment（clasp_ids 存 JSON 数组）
  ▼
豆瓣短评（逐豆瓣链接）→ 过滤（≥15 汉字，按有用数 top5）→ comments 表
```

## 4. 数据库 Schema（SQLite `mystery_novel.db`）

```sql
books (
  id INTEGER PK,
  title TEXT, author TEXT, tags TEXT,          -- 标签逗号分隔
  file_path TEXT,        -- 原始 EPUB 路径（溯源）
  clasp_id TEXT,         -- 遗留列，不再使用
  status TEXT DEFAULT '想读',                   -- 想读/在读/已读
  my_review TEXT, finished_date DATETIME,
  description TEXT, cover_path TEXT,
  series_name TEXT, series_order INTEGER,
  library_file TEXT,     -- 书库内规范文件名（sync/merge 的实际数据源）
  clasp_ids TEXT,        -- 来源 clasp 条目 ID 的 JSON 数组（合并本多条）
  merged_from TEXT       -- 合并书的来源本地书籍 ID JSON 数组
);
comments (
  id INTEGER PK, book_id FK→books,
  rating INTEGER,             -- 1-5，null 允许
  content TEXT, usefulness INTEGER,
  source TEXT DEFAULT '豆瓣',  -- 豆瓣 / AI助手生成
  is_mine INTEGER DEFAULT 0
);
embeddings (id, book_id, chunk_text, vector BLOB)  -- 预留 RAG，未使用
llm_usage (
  model TEXT PK, calls INTEGER,
  prompt_tokens, completion_tokens, total_tokens INTEGER,
  last_used DATETIME     -- 每次 LLM 调用按模型 UPSERT 累加
)
```

迁移方式：启动时幂等 `ALTER TABLE ADD COLUMN`（列已存在静默忽略）。

## 5. 外部服务

| 服务 | 端点 | 用途 | 容错 |
| :--- | :--- | :--- | :--- |
| claspclub 搜索建议 | `GET /api/v1/search/suggestions?keyword=` | CLI 交互式匹配 | 失败→手动录入 |
| claspclub 分页搜索 | `GET /api/v1/books?keyword=&sort=doubanRating&page=&pageSize=5` | GUI 搜索/analyze（条目自带简介/封面；不含系列，系列在导入时逐来源调详情获取） | 失败→降级/手动录入 |
| claspclub 详情 | `GET /api/v1/books/{id}` | 简介/封面/系列（`series{name,order}`，单行本为 null）/豆瓣链接/版本封面（导入时逐来源调用） | 失败→跳过该字段 |
| claspclub 封面 OSS | `clasp-book-images.oss-cn-hangzhou.aliyuncs.com` | 封面下载 | 带 `Referer: https://claspclub.com/` 防盗链伪装 |
| 豆瓣书籍页 | `book.douban.com/subject/{id}/` | 封面兜底（og:image） | 随机 bid cookie + 浏览器 UA |
| 豆瓣短评页 | `.../comments/` | 短评知识库 | 失败→跳过 |
| LLM | OpenAI 兼容 `/chat/completions` | 简介融合 / 书评对话 | 未配置/失败→降级（拼接 / 手动输入），原因回传 GUI 提示 |

LLM 配置优先级：GUI 设置页（config.toml `llm` 段）→ 环境变量 `OPENAI_API_KEY`（必需）、`OPENAI_BASE_URL`（默认 openai）、`MODEL_NAME`（默认 gpt-3.5-turbo）。

## 6. 关键行为不变量（修改代码前必读）

1. **原文件不动**：`add` 只复制，元数据只写入书库副本；豆瓣/网络失败不 panic。
2. **静默规则**：仅 `batch && 唯一结果` 全静默；多结果、手动录入、合并本、非中文书名必须交互。
3. **降级链**：LLM→拼接/手动；封面 OSS→豆瓣；搜索→手动录入；每级都有日志。
4. **数据质量**：短评 ≥15 汉字 + 按有用数排序取 5（不足时放宽取最长 5 条）。
5. **合并本**：`clasp_ids` JSON 数组；系列继承要求全部条目同系列同卷号。
6. **Rusqlite 同步调用**在 async 上下文中直接使用（CLI 单任务场景，可接受）。

## 7. merge 合并算法（merge.rs）

1. 解析书籍（按系列名 or 显式 ID 列表），确认后解析 EPUB 路径（`library_file` 优先，回退 `file_path`）。
2. 简介来源：DB `description` → 回退 EPUB `dc:description` → LLM `combine_descriptions` 融合。
3. 逐本读取：manifest 图片/CSS 以 `b{i}/` href 前缀写入新 EPUB；章节经
   `EpubRewriteOptions::rewrite_paths(PathRewrite::prefix)` 读取，引用路径自动重写。
4. 结构：每本原书为一卷（`EpubChapter` children），子章节标题取内容 `<title>` 标签。
5. 封面取第一本；生成后入书库 + `merged_from` 记录来源。
6. 已知限制：CSS 样式可能退化（设计取舍，文字+图片尽量保留）。

## 8. sync 设备同步（device.rs）

- `sysinfo::Disks` 枚举可移动磁盘（跨平台：Win 盘符 / Linux `/media|/run/media` / macOS `/Volumes`）。
- 识别：Kindle（`documents/`+`system/` → 目标 `documents/`）、Kobo（`.kobo/`）、通用 USB（根目录）。
- MTP 协议设备（新款 Kindle）不受支持，提示手动复制。
- 流程：选设备 → MultiSelect 选书 → 确认 → 逐本复制 `library_file`。

## 9. GUI（Tauri 2 + React，frontend/ + src-tauri/）

业务逻辑与终端交互解耦（仅依赖 `Path` + `Connection`），GUI 通过命令层复用：

| 命令 | 说明 | 交互/降级 |
| :--- | :--- | :--- |
| `analyze_epub` | 提取内嵌书名/作者（繁→简）+ claspclub 静默匹配 | 唯一/精确同名才采用，歧义降级本地元数据 |
| `search_clasp` | 搜索页分页搜索（pageSize=5，按豆瓣评分排序；条目自带简介/封面，导入省去详情调用） | 翻页按钮；失败返回错误由前端展示 |
| `fetch_cover_image` | 代理下载远程封面（伪装 Referer 过 OSS/豆瓣防盗链），返回原始字节 | 前端 blob URL 缓存渲染 |
| `read_cover_file` | 读取本地封面缓存文件字节（书库卡片/详情/来源/悬停封面统一经此渲染） | 全平台一致、移动端不依赖 asset 协议（Android WebView 下 asset 对绝对路径不可靠）；路径白名单校验（各书库 covers 目录） |
| `collect_epubs` | 递归收集文件夹下所有 EPUB（批量加书） | 按路径排序返回 |
| `import_epub` | 确认后导入；`matched` 多条 = 合并本（标签并集 + LLM 融合简介 + 第一本封面）；结果回传 `fusion_model` / `fusion_error`（融合降级/失败原因，GUI 显式提示） | 携带 `task_id`：进度经 `task-progress` 事件实时推送，`cancel_task` 可打断（详情/融合/封面/短评各阶段 select! 中断；"写入书库"检查点后不可打断） |
| `cancel_task` | 打断进行中的长任务（TaskRegistry 按 task_id 置位取消令牌） | 返回是否存在该任务 |
| `delete_book` | 删除 DB 记录 + 书库 EPUB 副本 + 封面缓存（原始导入文件不动） | 前端原生确认框 |
| `update_book_meta` | 详情页编辑书名/作者/标签/简介，同步 EPUB 副本 | EPUB 写入失败仅降级更新 DB |
| `upload_cover` | 手动封面替换：复制入 covers → 同步 EPUB 副本嵌入封面 → 更新 DB | EPUB 写入失败仅降级更新 DB |
| `get_settings` / `save_settings` | LLM 多 Provider（各含 Endpoint / API Key / 模型）+ 默认服务商/模型 + token 用量 | 旧版单 provider 平铺配置自动迁移；名称去重校验 |
| `llm_status` | LLM 可用性预检（合并本简介导入前提示） | 未配置返回 configured=false |
| `set_book_status` | 切换阅读状态（想读/在读/已读，已读自动记录完成时间） | 状态值白名单校验 |
| `llm_chat` | 通用 LLM 对话（GUI AI 书评会话）；用量按 "provider/model" 入库 | 消息序列必须 system 开头 |
| `save_book_review` | 保存书评（仅写 `books.my_review`，不插个人短评，避免豆瓣短评区重复展示；详情页提供编辑入口） | 空书评拒绝 |
| `get_reader_content` | 内置阅读器：按 spine 提取章节正文（脚本/事件属性剥离 + 图片 data URI 内联，单图 10MB/全书 64MB 预算上限） | 大书解析走 spawn_blocking；无章节时报错 |
| `reset_book_series` | 按各 clasp 来源系列信息重设书籍系列（inherit_series 规则；旧数据缺系列自动回拉详情回填来源行）并同步 EPUB 副本 | 来源系列不一致时报错拒绝 |
| `webdav_test` / `webdav_push` / `webdav_pull` | WebDav 连接测试（按书库名建远程子目录 + PROPFIND）/ **同步到云端**：本地完整覆盖云端（EPUB/covers 同名同大小跳过、云端多余文件 DELETE、DB 以 VACUUM INTO 一致性快照整份上传，失败回退直读文件）/ **从云端同步**：云端完整覆盖本地（EPUB/covers 覆盖下载、本地多余文件删除、DB 快照按"远端目录实际存在的 EPUB 集合"过滤后**整库覆盖**目标书库 —— 命中 library_file 整行覆盖、缺失并入、远端已无的删除，短评/来源随远端全量替换；云端无快照时降级为按 EPUB 登记骨架行）。PROPFIND 跳过目录条目；covers 目录写 `.nomedia`（移动端防相册收录） | 仅手动触发、**仅当前书库**可同步（后端校验 current_library），无自动同步；进度经 task-progress 实时推送（phase 前缀 `webdav-`），task_id 注册 SyncTaskRegistry 供「打断」；凭据存 config.toml；远端布局 `{remote_dir}/{书库名}/` |
| `is_mobile` / `fs_roots` / `list_fs_dir` / `create_fs_dir` | 内置文件浏览器支撑（移动端选书/选目录/新建文件夹；桌面走系统原生对话框） | 列目录失败时 UI 提示授予"所有文件访问" |
| `write_text_file` / `read_text_file` | 会话导出/导入 JSON（路径经系统保存/打开对话框取得） | 本地文件 IO |
| `open_book_file` / `get_reader_content` | 按**书籍归属书库**解析 EPUB 路径（归属书库 → 当前书库 → 遗留 library_path；多书库/移动端必需），书库副本优先、原始文件兜底 | 多书库下旧逻辑用遗留字段会定位错目录 |
| `chatbot_chat` | 书虫 Agent：工具调用循环（检索本地书库），回复携带 `steps` 工具调用轨迹（工具名/实参/结果摘要） | 前端以可折叠记录展示，非黑盒 |

GUI 加书弹窗承担交互确认职责：书名/作者/标签可编辑、展示封面；无/多结果时进入搜索页（可改关键词、分页浏览、按顺序多选合并本）；**非中文书名必须确认后**才可导入。「批量加书」为分裂按钮（点击选文件夹递归扫描，箭头/悬浮展开菜单），「唯一结果自动导入」开关置于其下拉菜单内（主题色圆点标识开合，与 CLI batch 一致），可随时打断（停止剩余 + 中断当前导入）。

长任务（>3s：导入全流程、LLM 融合、批量导入）通过 `task-progress` 事件实时渲染进度，`cancel_task` + `CancelToken`（select! 中断网络调用）支持打断。

阅读状态徽章可点击切换（想读/在读/已读，书架卡片与详情页均可）；切到"已读"后弹窗询问短评 —— 可与 AI 多轮对话讨论后生成（`llm_chat`，前端维护会话，system 提示词与 CLI finish 一致）、手动填写或跳过；"我的书评"仅展示于详情页专区（不进豆瓣短评列表）且可随时编辑。书架页支持拖拽导入：EPUB 文件走单本确认流程、文件夹走批量流程（拖拽中显示遮罩提示）。LLM 用量按 "provider/model" 累计入 `llm_usage` 表。弹窗不启用"点击外部关闭"（避免拖拽选择文本误触）。文件/图片选择与删除确认使用 `tauri-plugin-dialog`。

**书虫 Chatbot**（`Chatbot.tsx`）：回复经轻量 Markdown 渲染（`utils/markdown.ts`，先转义再转换防注入，支持标题/列表/引用/代码块/粗斜体/链接）；assistant 消息携带工具调用轨迹（可折叠查看实参与结果摘要）；多会话管理（localStorage `mna-chat-sessions`）：历史任务查看/切换/删除，会话可导出/导入完整上下文 JSON（`save`/`open` 对话框 + `write_text_file`/`read_text_file`）。

详情页提供「阅读」按钮（保留「打开本地文件」）：`ReaderModal` 全屏弹窗按章节渲染 `get_reader_content` 提取的正文（iframe srcdoc + sandbox 禁脚本，统排样式；章节下拉/字号调节/进度记忆于 localStorage `mna-reader:{bookId}`；「排版」弹层可选阅读模式 —— **上下滚动** 或 **自动分页**（左右翻页 = CSS 多列、上下翻页 = 行栅格切片，页高对齐 1.9 倍行高整数倍避免切割文字），分页模式滑动/点按两侧/方向键翻页并在章边界自动跨章；触摸手势挂 iframe 内容文档——触摸事件不跨 iframe 冒泡）。首页书架显示方式可切换（网格 / 列表=封面左信息右 / 仅封面）并可调封面大小（大/中/小，窄屏未设置时默认小，持久化于 localStorage）。来源管理弹窗逐条展示 clasp 来源的系列信息（导入与"更新数据"时爬取，不在打开页面时回拉），工具栏提供「重设系列」（按来源继承规则）与「重设标签」。书库配置含**书库名**（slug，仅 `[A-Za-z0-9_]`，本地唯一，旧配置由 id 派生自动迁移）与标题两字段。设置页为三页签：书库 / LLM / **WebDav** —— WebDav 页签按书库分子选项卡，含拨动开关（启用同步）、服务器地址、账号、应用专用密码、远程目录、远端位置预览（`{remote_dir}/{书库名}`，同名书库跨设备自动对齐）与「测试连接 / 保存 / 同步到云端 / 从云端同步」（同步前自动保存配置；推送/拉取均为整库覆盖语义，仅在点击时执行，非当前书库禁用）。首页工具栏的「同步」按钮为下拉菜单（同步到云端 / 从云端同步两项），同步过程显示"已处理/总数 + 百分比"进度条并可「打断」；「显 示」下拉菜单切换书架视图与封面大小。导入确认弹窗的系列字段经 `commit_import` 落库。

## 9.1 移动端（Android，Tauri 2 Mobile）— 已接入

应用构建采用标准双目标结构：`src-tauri/src/lib.rs`（`#[cfg_attr(mobile, tauri::mobile_entry_point)] run()`，桌面与移动共用）+ `src-tauri/src/main.rs`（桌面薄壳）；`[lib] crate-type = ["staticlib", "cdylib", "rlib"]` 供 Android 以 `libmystery_gui_lib.so` 加载。Android 上 `dirs::` 系列（HOME 等）失效，启动时经 `config::set_data_dir_override(app_data_dir)` 重定向 config/数据库路径，临时文件走 `config::temp_dir()`（app cache 子目录）。

构建/调试步骤（Windows）：
1. 安装 Android Studio（含 SDK 34+ / Platform-Tools / NDK）与 JDK 17；`JAVA_HOME` 指向 JDK（或 Android Studio jbr）。
2. `rustup target add aarch64-linux-android armv7-linux-androideabi i686-linux-android x86_64-linux-android`。
3. **开启 Windows 开发人员模式**（设置 → 系统 → 开发者选项）——tauri 需符号链接把 `.so` 放入 jniLibs，否则报 "Creation symbolic link is not allowed"（或以管理员身份运行终端）。
4. `cargo tauri android init`（已生成 `gen/android`）→ 启动模拟器或连接真机。
5. 调试：`cargo tauri android dev`（vite devUrl 主机由 CLI 自动替换为局域网 IP）；出包：`cargo tauri android build`（APK/AAB）。
6. **已知问题与对策**：
   - `android dev` 的文件监听会检测到 CLI 自己写入 gen 资产的 devUrl 变更而反复 Rebuild + 重装（表现类似反复闪退）→ 加 `--no-watch`（手动重启迭代），或用 `cargo tauri android build --debug` 出自包含 APK 直接安装测试。
   - 内置文件浏览器需要「所有文件访问」权限：`AndroidManifest.xml` 已声明 `MANAGE_EXTERNAL_STORAGE` 等，首次使用需在系统设置（应用 → 特殊应用权限 → 所有文件访问）中手动授予，否则列目录报权限错误（UI 已有提示）。
7. 移动端选择器：`is_mobile` / `fs_roots` / `list_fs_dir` 命令支撑内置 `PathPickerHost` 浏览器（选 EPUB 文件 / 选目录）；桌面端走 tauri-plugin-dialog 原生对话框（`api/picker.ts` 统一分发）。主页在无书库时隐藏工具栏/搜索/筛选并显示创建引导；创建书库必须先经选择器选定目录。
8. 已知限制：USB 设备同步（device.rs）在 Android 无意义；`open_book_file`（系统应用打开）在 Android 上退化；桌面用户数据布局不受影响（override 仅移动端注入）。

## 10. 技术栈

| 类别 | Crate |
| :--- | :--- |
| 异步运行时 | `tokio` (full) |
| CLI | `clap` (derive)、`dialoguer`、`comfy-table` |
| HTTP / 解析 | `reqwest` (rustls-tls)、`scraper`、`regex`、`percent-encoding` |
| EPUB | `rbook`（读写、封面、章节、路径重写） |
| 数据库 | `rusqlite`（bundled + functions，自定义 SQLite 函数） |
| 中文处理 | `pinyin`（拼音命名）、`character_converter`（繁→简） |
| 系统 | `sysinfo`（磁盘）、`dirs`、`dunce`、`toml` |
| 序列化/日志/错误 | `serde`/`serde_json`、`tracing`/`tracing-subscriber`、`thiserror`/`anyhow` |

## 11. 配置文件

`~/.config/mystery-novel-agent/config.toml`（Linux/macOS）或 `%APPDATA%\mystery-novel-agent\config.toml`（Windows）：

```toml
library_path = "/path/to/library"   # 旧版单书库字段（已迁移进 libraries）
covers_path  = "..."                # 可选，默认 library_path/covers
database_path = "..."               # 可选，默认 {数据目录}/mystery-novel-agent/mystery_novel.db

[[libraries]]                       # 书库列表（多书库）
id           = "default"
name         = "default"            # 书库名（slug，仅 [A-Za-z0-9_]，本地唯一；WebDav 远程子目录名）
title        = "默认书库"
path         = "/path/to/library"
default_tags = ["推理小说"]

[libraries.webdav]                  # WebDav 同步（每书库独立开关与凭据；远端 {remote_dir}/{书库名}/）
enabled    = false
url        = "https://dav.jianguoyun.com/dav/"
username   = "…"
password   = "…"                    # 应用专用密码（本地明文，与 LLM API Key 同级）
remote_dir = "mystery-novel-agent"

[llm]                               # GUI 设置页写入；优先于 OPENAI_* 环境变量（多 Provider）
default_provider = "DeepSeek"       # 默认服务商
default_model    = "deepseek-chat"  # 默认模型（须属于默认服务商）

[[llm.providers]]
name     = "OpenAI"
base_url = "https://api.openai.com/v1"
api_key  = "sk-…"
models   = ["gpt-4o-mini", "gpt-4o"]

[[llm.providers]]
name     = "DeepSeek"
base_url = "https://api.deepseek.com/v1"
api_key  = "sk-…"
models   = ["deepseek-chat"]
```

数据库默认位置（`AppConfig::database_file`，CLI 与 GUI 共用）：Linux `~/.local/share/mystery-novel-agent/mystery_novel.db`；Windows `%APPDATA%\mystery-novel-agent\mystery_novel.db`。

GUI（Tauri）：`src-tauri/` 子 crate 复用本库（`mystery-novel-agent` path 依赖），React 前端在 `frontend/`；`cargo tauri dev` 启动开发模式，命令层见 `src-tauri/src/commands.rs`（加书/删书/元数据编辑/封面替换/搜索/设置）。
