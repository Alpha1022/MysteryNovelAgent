# 架构文档 — 推理小说阅读管理助手

> 最后更新：2026-09 · Rust 2024 edition · 核心库 ≈ 8.7k 行 / GUI(Rust) ≈ 3.4k 行 / 前端 ≈ 7.3k 行
>
> 本文档的目标：把项目的**每一个部分**讲清楚到可以直接回答深挖提问的程度。
> 每个设计都尽量回答两个问题：**是什么**、**为什么这么做**。

## 目录

1. [项目概述](#1-项目概述)
2. [总体架构与分层](#2-总体架构与分层)
3. [模块清单与职责](#3-模块清单与职责)
4. [数据库 Schema](#4-数据库-schema-sqlite-mystery_novel_db)
5. [导入流水线详解（核心）](#5-导入流水线详解核心)
6. [外部服务与反爬细节](#6-外部服务与反爬细节)
7. [LLM 集成设计（agent.rs）](#7-llm-集成设计agentrs)
8. [书虫 Agent（GUI）](#8-书虫-agentgui)
9. [finish 苏格拉底书评（CLI）](#9-finish-苏格拉底书评cli)
10. [内置阅读器（reader.rs）](#10-内置阅读器readerrs)
11. [merge 系列合并（merge.rs）](#11-merge-系列合并mergers)
12. [WebDav 整库同步（webdav.rs）](#12-webdav-整库同步webdavrs)
13. [设备同步（device.rs）与静态书架（shelf.rs）](#13-设备同步devicers与静态书架shelfrs)
14. [GUI（Tauri 2 + React）](#14-gui-tauri-2--react)
15. [移动端（Android）](#15-移动端android)
16. [关键设计决策 FAQ（防深挖）](#16-关键设计决策-faq防深挖)
17. [测试策略](#17-测试策略)
18. [已知限制与未来方向](#18-已知限制与未来方向)
19. [技术栈与配置文件](#19-技术栈与配置文件)
20. [构建与调试（重要坑）](#20-构建与调试重要坑)

---

## 1. 项目概述

**一句话定位**：给推理小说重度读者的本地优先书库管理器 —— 扔进来的每个 EPUB 自动变成一条带完整元数据、简介、封面、豆瓣优质短评的书架记录，且原始文件一个字节不动。

**解决什么痛点**：网购 / 网盘 / 论坛收集来的 EPUB 元数据脏乱（书名乱码或繁体、无作者、无简介无封面）；同一本书多版本、系列卷号混乱；想看优质评价要人肉上豆瓣。现成工具（Calibre）全能但整理全靠手动。

| 能力 | CLI 命令 | 说明 |
| :--- | :--- | :--- |
| 导入增强 | `add <epub\|目录>` | claspclub 匹配 → 元数据/简介/封面/系列增强 → 拼音命名入书库（目录则批量） |
| 阅读管理 | `finish` | 标记读完 + 苏格拉底式 AI 对话生成个人书评 |
| 书库查询 | `list [--limit --search]` / `delete <id>` | 表格展示 / 删除（原始文件不动） |
| 设备同步 | `sync` | 检测 USB 阅读器（Kindle/Kobo/通用 U 盘）并复制书籍 |
| 系列合并 | `merge --series <名>\|--ids <a,b>` | 多本 EPUB 合一 + LLM 融合简介 |
| 静态书架 | `shelf` | 渲染单文件 `shelf.html`（封面 + 链接 + 扫码提示），方便发给手机 |
| 书库配置 | `library config set/show` | 书库路径配置（GUI 内为多书库管理） |

GUI 另有：拖拽/批量导入、书虫 Agent（检索本地书库的对话助手）、内置阅读器、来源管理（多版本封面/简介切换、重抓短评）、多书库、WebDav 同步、Android 端。

**三大设计原则**（贯穿全部代码，被 44 个单元测试守卫）：

1. **本地优先**：无账号无云端；数据全部在本地 SQLite + 文件系统；WebDav 仅手动触发的整库同步。
2. **原文件不动**：`add` 只复制入书库，元数据只写书库副本（副本可随时重建，因此写入无需确认）。
3. **优雅降级**：每个外部依赖（claspclub/豆瓣/LLM/封面 OSS）都有失败路径，失败 `warn!` 日志后继续，**禁止 panic/unwrap**（测试代码除外）。

## 2. 总体架构与分层

```text
┌─────────────────────────── 入口层（薄壳，无业务逻辑）───────────────────────────┐
│  CLI（src/main.rs，clap 路由 + dialoguer 交互）                                  │
│  桌面 GUI / Android（src-tauri/，Tauri 2 命令层，commands.rs 全部 #[tauri::command]）│
└──────────────────────────────────┬────────────────────────────────────────────┘
                                   │ 仅依赖 Path + &Connection
┌──────────────────────────────────▼────────────────────────────────────────────┐
│  核心库 mystery-novel-agent（src/，纯 Rust）                                    │
│  ingestion.rs 导入流水线 │ spider.rs 反爬抓取 │ agent.rs LLM 客户端              │
│  db.rs SQLite 全量读写  │ config.rs 配置      │ library.rs 拼音命名/复制            │
│  merge.rs 合并          │ reader.rs 阅读提取  │ finish.rs 书评 │ webdav.rs 同步    │
│  device.rs 设备         │ shelf.rs 静态页     │ utils.rs 中文工具                  │
└──────────────────────────────────┬────────────────────────────────────────────┘
                                   ▼
        SQLite（books/comments/sources/cover_refs/llm_usage）
        本地书库目录（拼音规范命名 + covers/ 内容去重缓存）
        外部服务：claspclub API × 3 · 豆瓣书籍页/短评页 · OpenAI 兼容 LLM
```

**分层规则**（写入 AGENTS.md 的硬约束）：`main → ingestion → {spider, agent, db, config, library}`；`merge → {agent, db, config, library}`；`device → {db, config}`；依赖无环、底层不反向依赖上层。

**为什么这样分**：业务逻辑（爬取、合并、命名、同步）只认 `Path` 和 `Connection`，不认"终端"或"窗口"——所以同一套核心库被 CLI 和 GUI 复用，GUI 的 `prepare_import`/`persist_import` 就是 CLI `ingest_book` 的非交互式拆解版（见 §5）。

## 3. 模块清单与职责

| 模块 | 行数 | 关键函数 / 职责 |
| :--- | :--- | :--- |
| `main.rs` | 265 | clap 命令路由；`run_add`（单本/文件夹批量循环）；`auto_render_shelf`（导入后自动重渲染静态书架）；`collect_epubs` 递归收集 |
| `ingestion.rs` | 2645 | 【核心】导入流水线。CLI 入口 `ingest_book`（交互式）；GUI 入口 `analyze_epub`（静默匹配）/ `search_clasp_page`（分页搜索）/ `prepare_import`（可打断预处理）/ `finalize_import`（落盘）/ `persist_import`（入库）；`sanitize_epub_if_needed`（损坏 EPUB 修复：zip 重复条目走中央目录重建、OPF manifest 去重/坏引用剔除）；`inherit_series`（系列继承规则，CLI/GUI/来源重设共用）；`crawled_title_author`（爬取优先回填）；`pick_silent_match`（静默匹配：唯一结果或繁简归一后精确同名） |
| `spider.rs` | 878 | claspclub 三接口客户端（建议/分页/详情，结构体集中定义）；豆瓣短评解析（`li.comment-item`，评分 allstarXX/10、内容、有用数，全部有兜底选择器）；`filter_comments`（≥15 汉字 + 有用数降序 top5，不足放宽取最长 5 条）；`parse_douban_description`（`#link-report` 内 `span.all` 完整版优先 / `span.short` 兜底，限定内容简介区避免串到作者简介区）；`parse_douban_book_meta`（og:title / #info 作者 / og:image）；`fetch_clasp_reviews`（claspclub 评论接口，防御式字段兼容） |
| `agent.rs` | 476 | LLM 客户端：`resolve_config`（多 Provider 优先级解析，见 §7）；`chat` / `chat_with_tools`（重试 + 超时）；`combine_descriptions`（多来源简介融合，带降级原因返回）；`TokenUsage`/`ChatOutcome` |
| `finish.rs` | 227 | 苏格拉底式书评：选书 → 立即标记已读 → 组装上下文（元数据 + top5 短评）→ 多轮对话（`/done` 生成书评、`/skip` 手动）→ 书评入库（`my_review` + 个人短评） |
| `db.rs` | 1737 | 全部表结构与幂等迁移；封面引用计数（`cover_ref_add/release/seed`，归零才允许删文件）；`replace_library_from_remote`（WebDav 拉取的整库覆盖，含 VACUUM INTO 快照）；`get_book_cards`（书架分页查询，含搜索/多书库过滤）；sources 表全套 CRUD 与 `sync_book_source_columns`（books 冗余列回写） |
| `config.rs` | 438 | `AppConfig`（config.toml 读写）：多书库（含主题/默认标签/WebDav 凭据）、多 Provider LLM 段（旧单 Provider 平铺配置自动迁移）、`covers_dir`、`temp_dir`（移动端经 `set_data_dir_override` 重定向到应用私有目录） |
| `library.rs` | 111 | `slugify` 拼音命名（`[系列拼音-N] 作者拼音-书名拼音.epub`，仅 `[a-z0-9-]`，冲突 `-2`）；`copy_into_library`（复制 + resolve_collision）；`to_ascii_filename` |
| `device.rs` | 150 | `sysinfo::Disks` 枚举可移动盘（Win 盘符 / Linux `/media` / macOS `/Volumes`）；识别 Kindle（`documents/`+`system/`）、Kobo（`.kobo/`）、通用 USB；MultiSelect 选书复制 `library_file` |
| `webdav.rs` | 974 | WebDav 客户端（reqwest 手发 MKCOL/PROPFIND/PUT/DELETE）；`push`（本地覆盖云端）与 `pull`（云端覆盖本地）的整库算法，见 §12 |
| `reader.rs` | 195 | 内置阅读器提取：spine 章节正文，脚本/事件属性剥离，图片 base64 内联（单图 10MB / 全书 64MB 预算），`resolve_ref` 相对路径归一 |
| `merge.rs` | 527 | `plan_merge`（解析/校验/展示计划）+ `apply_merge`（资源 `b{i}/` 前缀重写、`EpubRewriteOptions::rewrite_paths`、每书一卷、来源行合并、`merged_from` 记录） |
| `shelf.rs` | 159 | 把书库渲染成单文件 `shelf.html`（`shelf_template.html` + 封面相对路径 + 转义），供手机浏览 |
| `utils.rs` | 63 | `count_han`（汉字计数，短评质量过滤用）、`split_tags`/`join_tags`、`filename_stem` |

## 4. 数据库 Schema（SQLite `mystery_novel.db`）

```sql
books (
  id INTEGER PK,
  title TEXT, author TEXT, tags TEXT,            -- 标签逗号分隔；每本必含"推理小说"
  file_path TEXT,        -- 原始 EPUB 路径（溯源，不可靠：用户可能移动）
  clasp_id TEXT,         -- 遗留列（已由 clasp_ids 取代，保留兼容）
  status TEXT DEFAULT '想读',                    -- 想读/在读/已读（白名单校验）
  my_review TEXT, finished_date DATETIME,        -- finish 产物
  description TEXT, cover_path TEXT,             -- 增强元数据
  series_name TEXT, series_order INTEGER,        -- calibre:series(_index) 同步写入
  library_file TEXT,     -- ★ 书库内规范文件名：sync/merge/阅读器的实际数据源
  clasp_ids TEXT,        -- 来源 clasp 条目 ID 的 JSON 数组（合并本多条）
  merged_from TEXT,       -- 合并书的来源本地书籍 ID JSON 数组
  douban_urls TEXT,       -- 豆瓣书籍页链接 JSON 数组（重抓短评/封面兜底）
  library_id TEXT        -- 多书库归属（旧数据 NULL，启动迁移归入当前书库）
);
comments (
  id PK, book_id FK→books,
  rating INTEGER,             -- 1-5（allstarXX / 10 折算），允许 null
  content TEXT, usefulness INTEGER,
  source TEXT DEFAULT '豆瓣',  -- 豆瓣 / claspclub / AI助手生成
  is_mine INTEGER DEFAULT 0,  -- 个人书评标记（详情页专区展示，不混入短评列表）
  source_ref TEXT             -- 来源定位：clasp ID 或豆瓣链接（按来源重抓用）
);
sources (                    -- 来源管理：每本书的有序 clasp/豆瓣项目
  id PK, book_id FK,
  kind TEXT,                 -- 'clasp'（ref=条目ID）| 'douban'（ref=书籍页URL）
  ref TEXT, position INTEGER,-- 多来源有序（合并本按选择顺序）
  title, author, cover_url, cover_path, summary, tags,   -- 冗余缓存
  editions TEXT,             -- clasp 版本封面 JSON（更换封面弹窗）
  series_name, series_order  -- 仅 clasp 来源有（重设系列用）
);
cover_refs (path TEXT PK, refs INTEGER);  -- 封面内容去重后的引用计数
llm_usage (model TEXT PK, calls, prompt_tokens, completion_tokens, total_tokens, last_used);
embeddings (id, book_id, chunk_text, vector BLOB);  -- 预留 RAG，未使用
```

**关键机制**：

- **迁移**：启动时幂等 `ALTER TABLE ADD COLUMN`（列已存在静默忽略），无版本号表 —— 老库零迁移成本直接升级；`assign_legacy_books` 把无书库归属的旧数据归入当前书库。
- **封面引用计数**：同一张封面字节内容只落盘一次（64 位内容哈希 + 长度命名）；书籍/来源/悬停副本各记一引用；删除书籍时 `cover_ref_release`，**归零才允许删文件** —— 修复"两本书共用同一封面、删一本把另一本的封面也删了"。
- **`library_file` vs `file_path`**：一切消费方（设备同步、合并、阅读器、WebDav）一律用前者；后者仅溯源展示。
- **`sync_book_source_columns`**：sources 表插入/删除后回写 books 的冗余列（标题/作者/封面等），保证书架列表查询不用 JOIN。

## 5. 导入流水线详解（核心）

### 5.1 CLI 交互式（`ingest_book`）

```text
EPUB（原文件只读）
 1. sanitize_epub_if_needed：zip 重复条目（中央目录手动重建）/ OPF 坏 manifest（去重 + 删坏引用 + spine 联动）→ 得到干净副本
 2. 关键词候选 = <dc:title> → 文件名（去扩展名）
 3. claspclub 建议搜索（逐候选尝试）
    ├ 多结果 → 交互菜单（选择/手动输入/合并本多选/取消）
    ├ 无结果 → 手动录入书名+作者（降级链）
    └ 唯一 + batch → 自动静默（跳过全部确认）
 4. 逐 clasp 条目调详情 API：summaryNoSpoiler / coverUrl / series{name,order} / editions[].doubanUrl（isPrimary 优先）
 5. 系列继承：所有条目同名才沿用；卷号全部一致才沿用，冲突取首条（上下册合并本）
 6. 书名规范化：繁→简（character_converter）；非中文标题必须人工确认（批量也不例外）
 7. 豆瓣链接 = 手动输入 + 版本表精准链接 + 封面 URL 推断（douban-{id}.jpg 正则）去重合并
 8. 逐豆瓣链接预爬：页面元数据（og:*/#info）、简介（§6）、短评（过滤 top5）
 9. 简介定稿：clasp 优先于豆瓣；多条 → LLM 融合（确认）/降级拼接
10. 封面：OSS（Referer 伪装）→ 豆瓣 og:image（bid cookie + UA）→ EPUB 内嵌封面；内容去重落盘
11. copy_into_library（拼音命名）→ write_epub_metadata（dc:title/creator/subject/description/calibre:series/封面嵌入书库副本）
12. 入库：books + enrichment + douban_urls + 来源行 + 短评（带 source_ref）+ 封面引用 + llm_usage
```

### 5.2 GUI 三段式（prepare / confirm / commit）

GUI 把同一条流水线拆成**可打断的三段**，全部网络工作在 `prepare_import`，落盘在 `finalize_import`，数据库在 `persist_import`：

```text
拖入/选书 → analyze_epub（静默匹配：唯一结果或繁简归一后精确同名才采用）
        ↓
搜索页（可改关键词/翻页/按序多选=合并本）
        ↓
prepare_import（网络+封面全部就绪；task-progress 事件实时推送；
              CancelToken 在各网络调用间检查 + tokio::select! 打断进行中的请求）
        ↓  ImportCache 按 task_id 暂存 PreparedImport
确认弹窗（书名/作者/标签/简介/系列/封面全部可编辑，用户编辑优先级最高）
        ↓
commit_import（应用编辑 → finalize_import 落盘 → persist_import 入库）
   或 discard_import（丢弃会话）
```

**打断机制**：前端 `task_id` → Rust `TaskRegistry(Mutex<HashMap<CancelToken>>)`；`cancel_task` 置位令牌；流水线在每阶段间 `cancel.check()?`、每个网络调用 `tokio::select! { r = fut, _ = cancel.wait_cancelled() }`；**"写入书库"检查点之后不可打断**（避免半写状态）。

**静默边界**（AGENTS.md 硬约束）：仅"批量 + 唯一结果"全静默；多结果、手动录入、合并本多选、**非中文书名确认**必须交互 —— GUI 上对应"唯一结果自动导入"开关。

**元数据优先级**（爬取 > EPUB 原值，用户编辑最终生效）：
- `analyze_epub`：静默匹配命中时直接返回爬取到的书名（繁→简）/作者 —— 前端预填与批量快速路径都用它；
- `prepare_import`：`crawled_title_author` 按 **clasp 详情 → 搜索条目 → 豆瓣页面** 回填书名（繁→简），作者取 clasp 来源并集（合并本顿号拼接）；确认弹窗中用户编辑在 `commit_import` 最终覆盖。

### 5.3 各阶段失败路径（降级链）

| 失败点 | 降级 | 用户感知 |
| :--- | :--- | :--- |
| claspclub 搜索/详情 | 手动录入书名+作者 / 跳过该字段 | CLI 提示；GUI 搜索页显示错误 |
| 豆瓣页面/短评 | warn 日志，跳过 | 书无短评 |
| LLM 融合 | `\n\n` 拼接 | GUI 提示条（`fusion_error`） |
| 封面 OSS | 豆瓣 og:image → EPUB 内嵌封面 → 无封面 | 书架占位图 |
| zip/OPF 损坏 | 自动重建临时副本（原文件不动） | 无感 |

## 6. 外部服务与反爬细节

| 服务 | 端点 | 伪装/技巧 |
| :--- | :--- | :--- |
| claspclub 建议 | `GET /api/v1/search/suggestions?keyword=` | CLI 交互匹配 |
| claspclub 分页 | `GET /api/v1/books?keyword=&sort=doubanRating&page=&pageSize=5` | GUI 搜索页（条目自带简介/封面/推断的豆瓣链接，省详情调用；**不含系列**） |
| claspclub 详情 | `GET /api/v1/books/{id}` | `series{name,order}`（单行本为 null）、`editions[].doubanUrl`（isPrimary 优先）、版本封面 |
| claspclub 评论 | `GET /api/v1/books/{id}/comments?area=all&sort=popular` | 未文档化接口，防御式字段兼容（content/body、likeCount/likes…） |
| 封面 OSS | `clasp-book-images.oss-cn-hangzhou.aliyuncs.com` | **Referer: `https://claspclub.com/`** 防盗链伪装 |
| 豆瓣书籍页 | `book.douban.com/subject/{id}/` | 随机 `bid` cookie + 浏览器 UA；`og:image` 封面；`#link-report` 简介（`span.all` 完整版优先，防串作者简介区）；图片下载带 `Referer: book.douban.com` |
| 豆瓣短评页 | `.../comments/` | `li.comment-item` 解析（评分/内容/有用数全有兜底选择器） |
| LLM | OpenAI 兼容 `/chat/completions` | 显式 `stream:false`；消息序列必须 system→user 开头（部分 API 拒绝 system→assistant） |

**为什么选 claspclub 而不是直接爬豆瓣**：豆瓣无书籍搜索接口（反爬），claspclub 是第三方聚合的书目库，恰好提供精确搜索 + 无剧透简介 + 版本信息 + 精准的豆瓣链接（`editions[].doubanUrl`），等于替我们完成了"搜索"这一步；豆瓣只用于兜底抓取页面数据与短评。

## 7. LLM 集成设计（agent.rs）

**多 Provider 配置解析**（`resolve_config`，优先级）：
1. config.toml `[llm]` 段（GUI 设置页写入）：`default_provider` + `default_model` → 从 `[[llm.providers]]`（各含 name/base_url/api_key/models）定位 base_url 与 key；**旧版单 Provider 平铺配置在 `AppConfig::load` 自动迁移**；
2. 环境变量 `OPENAI_API_KEY`（必需）+ `OPENAI_BASE_URL`（默认 openai）+ `MODEL_NAME`（默认 gpt-3.5-turbo）；
3. 都没有 → 返回 `None`，调用方走降级路径（**调用前先探测，绝不因未配置而失败**）。

**调用策略**：
- 超时 30s；重试次数取 `llm.retry_count`（默认 2，最大 10），**只对可重试错误**（网络错误 / HTTP 429 / 5xx）指数退避 —— 参数错误、鉴权失败、预算超限重试无意义；
- **预算咽喉**：`chat_with_tools_timeout` 是所有 LLM 调用（chat/融合/翻译/书虫循环）的唯一入口，每次调用前读 `llm.budget_tokens` 与 `llm_usage` 累计用量，达到预算立即返回 `LlmError::Budget`（不可重试；用量按次入库，多轮 agent 每轮都会重新检查 → 达到预算自动中断）。DB 读失败按 0 用量放行（统计故障不阻断功能）；
- 每次调用产出 `TokenUsage`（输入/输出/总量取自 API 响应的 `usage` 字段），按 `"provider/model"` 主键 UPSERT 累计入 `llm_usage`（GUI 设置页可见用量与成本）；
- **成本换算**：`LlmSettings::price_for` 按 "显式配置（`[[llm.pricing]]`，元/百万 tokens）精确匹配 → 内置预设表 `PRESET_PRICING` 最长前缀匹配" 解析价格；成本 = 输入量×输入价 + 输出量×输出价，设置页逐模型与合计展示；`reset_llm_usage` 清零用量（预算周期重置）；
- 带工具调用（`chat_with_tools`，书虫用）与不带工具两个入口。

**LLM 只在三个业务点出现**（其余全部确定性代码）：
1. `combine_descriptions`：导入/合并时的多来源简介融合 —— 输出 `DescriptionFusion{text, llm, degraded}`，失败降级拼接并**把原因回传 GUI**（提示条显示）；
2. `finish` 苏格拉底书评（CLI）/ `llm_chat`（GUI ReviewModal，同一套 system 提示词）；
3. 书虫 Agent 的工具调用循环（§8）。

## 8. 书虫 Agent（GUI）

`chatbot_chat` 命令 —— 一个小型 function-calling agent：

- **系统提示按数据库实时构建**：注入当前书库标题 + 前 40 本书的元数据摘要（含豆瓣短评精选）+ 工具使用指引，其余让模型用工具按需检索（提示词明确要求"不要凭空猜测"）；
- **读工具**（`chatbot_tools()`，OpenAI function calling 格式）：
  - `search_books(keyword)`：标题/作者/标签模糊检索；
  - `filter_books(status/author/tag)`：按条件过滤；
  - `get_book_detail(book_id)`：完整简介/个人书评/系列/状态；
  - `get_book_comments(book_id)`：这本书的短评列表；
  - `search_claspclub(keyword, page?)`：**在线书目库检索**（网络调用）—— 复用 `ingestion::search_clasp_page`（同款反爬 HTTP 客户端），返回书名/作者/标签/无剧透简介；结果显式标注"在线书目库（非本地书架）"并指引模型改用 search_books 查本地，避免线上信息与本地藏书混淆。**不持数据库锁**（工具循环逐工具短暂持锁，网络 IO 在锁外执行）。
  工具执行即普通 SQL 查询（`db::search_chatbot_books`），结果以 JSON 字符串回给模型；
- **写工具**（用户确认后才生效，见下）：
  - `update_book_meta(book_id, title?, author?, tags?, description?, series_name?, series_order?)`：修改元数据；
  - `set_book_status(book_id, status, review?, generate_review?)`：切换阅读状态；已读可携带交流总结出的短评或请求打开 AI 引导式短评窗口（`review` 与 `generate_review` 互斥，仅已读有效）；
  - `merge_books(book_ids[])`：按顺序合并为合集（≥2 本）。
  **后端不直接执行写操作**：`execute_chatbot_tool` 仅做参数校验（书存在 / 状态白名单 / ≥2 本）并返回 `pending_confirmation` 标记；前端 `Chatbot.tsx` 从最终回复的 `steps` 轨迹中解析出待确认动作（`collectActions`，按动作去重排队），逐个弹出与主界面**同一套组件**的确认/编辑窗口 —— `EditMetaModal`（书虫补丁预填到当前值上）、`ConfirmDialog`（状态/短评/合并确认，合并后沿用"是否 LLM 融合简介"固定流程）、`ReviewModal`（苏格拉底短评）—— 用户确认后走既有命令（`update_book_meta`/`set_book_status`/`save_book_review`/`merge_books_gui`）生效并广播 `mna:library-refresh` 事件刷新书架。系统提示要求模型调用写工具后告知用户"请在弹出窗口中确认"，不得声称已完成；
- **循环**：最多 `CHATBOT_MAX_ROUNDS=5` 轮工具调用；LLM 超时 120s（长推理链）；预算耗尽会在下一轮调用前被预算咽喉拦截；
- **可审计**：回复携带 `steps`（工具名 + 实参 + 结果摘要），前端以可折叠记录展示 —— **每执行完一个工具就推送 `chat-progress` 事件**，生成期间实时展示调用轨迹而非只放动画；
- 降级：未配置 LLM → 命令直接报错（GUI 不显示入口）。

## 9. finish 苏格拉底书评（CLI）

流程：`get_unfinished_books` 选择 → **立即** `mark_book_finished`（标记不依赖后续成功）→ 组装上下文（元数据 + top5 短评）→ `resolve_config` 探测 LLM：

- 有 LLM：多轮对话（system 提示注入书名/作者/标签/参考短评，引导式提问而非直接生成）；`/done` 结束并生成书评，`/skip` 跳过；
- 无 LLM / 对话失败：`manual_review` 手动多行输入（空行结束）—— 完整降级路径；
- 书评写 `books.my_review` + 插入 `is_mine=1, source='AI助手生成'` 的个人短评（详情页专区展示，不混入豆瓣短评列表）。

## 10. 内置阅读器（reader.rs）

- `extract_content`：按 spine 顺序取章节；正文经 `sanitize_and_inline` —— 剥离脚本/事件属性（iframe srcdoc + sandbox 双保险）、图片 base64 内联（单图 10MB / 全书 64MB 预算，超限跳过该图）；
- `resolve_ref` + `normalize_path`：处理 EPUB 内相对路径（`..`/`./` 归一），保证图片定位跨书可靠；
- 前端 `ReaderModal`（GUI）：iframe srcdoc 渲染；章节下拉 / 字号调节 / 进度记忆（localStorage `mna-reader:{bookId}`）；排版模式 **上下滚动** 或 **自动分页**（左右翻页 = CSS 多列、上下翻页 = 行栅格切片，页高对齐 1.9 倍行高整数倍避免切字）；触摸手势挂 iframe 内容文档（触摸事件不跨 iframe 冒泡）。

## 11. merge 系列合并（merge.rs）

1. `plan_merge`：按系列名（`get_books_by_series`）或显式 ID 列表解析；展示合并计划（书名/数量）确认；
2. 简介来源：DB `description` → EPUB `dc:description` → LLM `combine_descriptions` 融合；
3. 逐本读取：manifest 图片/CSS 以 `b{i}/` 前缀写入新 EPUB；章节经 `EpubRewriteOptions::rewrite_paths(PathRewrite::prefix)` —— rbook 在读取时自动重写资源引用路径；每本原书为一卷（`EpubChapter` children），子章节标题取正文 `<title>`；
4. 封面取第一本；来源行（sources）与短评按顺序合并；入书库 + `merged_from` 记录来源本地 ID；
5. 已知限制：CSS 样式可能退化（取舍：保文字与图片完整性）。

## 12. WebDav 整库同步（webdav.rs）

**语义**：推送 = 本地完整覆盖云端；拉取 = 云端完整覆盖本地。**仅手动触发**、仅当前书库（后端校验 `current_library`）。远端布局 `{remote_dir}/{书库名}/`（同名书库跨设备自动对齐）。

**推送**：EPUB 与封面逐个 PUT（**同名同大小跳过**）；云端多余文件 DELETE；数据库以 **`VACUUM INTO` 一致性快照**整份上传（保证库文件在写入中途不损坏；快照失败回退直读文件上传）。

**拉取**：EPUB/封面覆盖下载、本地多余文件删除；**DB 快照按"远端目录实际存在的 EPUB 集合"过滤后整库覆盖目标书库**（`db::replace_library_from_remote`）—— 命中 `library_file` 整行覆盖、新文件并入、远端已无的删除；短评/来源随远端全量替换；**云端无快照时降级为按 EPUB 登记骨架行**（首次从纯文件目录恢复）。

**其他**：PROPFIND 跳过目录条目；covers 目录写 `.nomedia`（Android 防相册收录）；进度经 `task-progress`（phase 前缀 `webdav-`）实时推送 + `SyncTaskRegistry` 支持打断；URL 路径段 percent-encode。

**为什么是整库覆盖而不是双向增量同步**：个人书库的设备拓扑通常是"一台主力机 + 临时设备"，整库语义简单、无冲突解决；避免了三态合并的复杂度与数据丢失风险（取舍明确）。

## 13. 设备同步（device.rs）与静态书架（shelf.rs）

- **sync**：`sysinfo::Disks` 枚举可移动盘；识别 Kindle（`documents/`+`system/` 存在 → 目标 `documents/`）、Kobo（`.kobo/`）、通用 USB（根目录）；MultiSelect 选书 → 确认 → 逐本复制 `library_file`。MTP 协议设备（新款 Kindle）不受支持，提示手动复制。
- **shelf**：`shelf render` 把书库渲染成单文件 `shelf.html`（`include_str!` 模板 + 封面相对路径 + HTML 转义）；CLI 每次导入后自动重渲染。用途：发给手机 / 挂内网快速浏览。

## 14. GUI（Tauri 2 + React）

### 14.1 命令层（src-tauri/src/commands.rs，3514 行，全部 #[tauri::command]）

分组速览：查询（`get_books`/`get_book_detail`/`get_comments`/`get_config`/`get_settings`/`llm_status`）、导入（`analyze_epub`/`search_clasp`/`import_epub`/`prepare_import`/`commit_import`/`set_pending_cover`/`discard_import`/`cancel_task`/`collect_epubs`/`save_dropped_file`/`debug_log`）、书籍维护（`delete_book`/`update_book_meta`/`upload_cover`/`set_book_status`/`save_book_review`/`reset_book_tags`/`reset_book_series`）、来源管理（`list_sources`/`add_clasp_sources`/`add_douban_sources`/`save_source_order`/`delete_source`/`clear_sources`/`refresh_source_comments`/`refresh_source_meta`/`set_source_as_cover`/`set_source_as_description`/`merge_source_descriptions`）、阅读（`get_reader_content`/`open_book_file`）、封面（`fetch_cover_image`/`read_cover_file`/`fetch_epub_cover`）、AI（`llm_chat`/`chatbot_chat`/`translate_text`）、WebDav 四命令、书库 CRUD 五命令、移动端文件浏览器（`is_mobile`/`fs_roots`/`list_fs_dir`/`create_fs_dir`）、`write_text_file`/`read_text_file`。

逐命令交互/降级细节：

| 命令 | 说明 | 交互/降级 |
| :--- | :--- | :--- |
| `analyze_epub` | 提取内嵌书名/作者（繁→简）+ claspclub 静默匹配 | 唯一/精确同名才采用，歧义降级本地元数据；**命中时书名/作者优先采用匹配结果**（EPUB 原值仅兜底） |
| `search_clasp` | 搜索页分页搜索（pageSize=5，按豆瓣评分排序；条目自带简介/封面，导入省去详情调用） | 翻页按钮；失败返回错误由前端展示 |
| `fetch_cover_image` | 代理下载远程封面（伪装 Referer 过 OSS/豆瓣防盗链），返回原始字节 | 前端 blob URL 缓存渲染 |
| `read_cover_file` | 读取本地封面缓存文件字节 | 全平台一致、移动端不依赖 asset 协议（Android WebView 下 asset 对绝对路径不可靠）；路径白名单校验（各书库 covers 目录 + 应用私有 covers 目录） |
| `has_all_files_access` / `open_all_files_access_settings` | Android 11+ 「所有文件访问」特殊权限检测与授权页跳转（内联 Tauri 插件 + Kotlin `StoragePermissionPlugin`）；同步可写性预检失败时前端弹窗引导 | 桌面端恒返回已授权；特殊权限无法弹窗请求，只能引导用户到系统设置开关 |
| `collect_epubs` | 递归收集文件夹下所有 EPUB（批量加书） | 按路径排序返回 |
| `save_dropped_file` | 保存 HTML5 拖入的文件字节到临时目录（拖拽导入专用；字节走 IPC 原始请求体、文件名走 `filename` 请求头） | 返回临时文件路径；文件名取 basename 并剔除非法字符；启动清扫遗留文件 |
| `debug_log` | 前端诊断日志转发到 app.log（排查滚动/拖拽等问题时前端加一行即可） | 日志随 stdout + 数据目录 app.log 双写 |
| `import_epub` | 确认后导入；`matched` 多条 = 合并本（标签并集 + LLM 融合简介 + 第一本封面）；结果回传 `fusion_model` / `fusion_error`（融合降级/失败原因，GUI 显式提示） | 携带 `task_id`：进度经 `task-progress` 事件实时推送，`cancel_task` 可打断（详情/融合/封面/短评各阶段 select! 中断；"写入书库"检查点后不可打断）；prepare 阶段书名/作者按 **clasp 详情 → 搜索条目 → 豆瓣页面** 优先回填（合并本作者取并集），确认弹窗中的用户编辑在 commit 时最终覆盖 |
| `cancel_task` | 打断进行中的长任务（TaskRegistry 按 task_id 置位取消令牌） | 返回是否存在该任务 |
| `delete_book` | 删除 DB 记录 + 书库 EPUB 副本 + 封面缓存（封面按引用计数，归零才删文件；原始导入文件不动） | 前端原生确认框 |
| `update_book_meta` | 详情页编辑书名/作者/标签/简介，同步 EPUB 副本 | EPUB 写入失败仅降级更新 DB |
| `upload_cover` | 手动封面替换：复制入 covers → 同步 EPUB 副本嵌入封面 → 更新 DB（`read_image_file` 供前端裁剪预览读取原图字节） | EPUB 写入失败仅降级更新 DB；EditMetaModal 关闭时 `onClose(changed=true)` 触发详情刷新（封面经内容去重换新路径，不刷新页面会停留旧图） |
| `get_settings` / `save_settings` | LLM 多 Provider（各含 Endpoint / API Key / 模型）+ 默认服务商/模型 + token 预算 + 模型价格表 + 用量与预估成本（逐模型 + 合计；`pricing_effective` 为"显式配置→内置预设"解析后的展示值） | 旧版单 provider 平铺配置自动迁移；名称去重校验；价格为非负数、按模型去重 |
| `reset_llm_usage` | 清零 `llm_usage` 用量统计（预算周期重置；不影响书籍数据） | 前端确认框 |
| `llm_status` | LLM 可用性预检（合并本简介导入前提示） | 未配置返回 configured=false |
| `set_book_status` | 切换阅读状态（想读/在读/已读，已读自动记录完成时间） | 状态值白名单校验 |
| `llm_chat` | 通用 LLM 对话（GUI AI 书评会话）；用量按 "provider/model" 入库 | 消息序列必须 system 开头 |
| `save_book_review` | 保存书评（仅写 `books.my_review`，不插个人短评，避免豆瓣短评区重复展示；详情页提供编辑入口） | 空书评拒绝 |
| `get_reader_content` | 内置阅读器：按 spine 提取章节正文（脚本/事件属性剥离 + 图片 data URI 内联，单图 10MB/全书 64MB 预算上限） | 大书解析走 spawn_blocking；无章节时报错 |
| `reset_book_series` | 按各 clasp 来源系列信息重设书籍系列（inherit_series 规则；旧数据缺系列自动回拉详情回填来源行）并同步 EPUB 副本 | 来源系列不一致时报错拒绝 |
| `webdav_test` / `webdav_push` / `webdav_pull` | WebDav 连接测试（按书库名建远程子目录 + PROPFIND）/ **同步到云端**：本地完整覆盖云端 / **从云端同步**：云端完整覆盖本地（详见 §12） | 仅手动触发、**仅当前书库**可同步（后端校验 current_library）；进度经 task-progress 实时推送（phase 前缀 `webdav-`），task_id 注册 SyncTaskRegistry 供「打断」；凭据存 config.toml |
| `is_mobile` / `fs_roots` / `list_fs_dir` / `create_fs_dir` | 内置文件浏览器支撑（移动端选书/选目录/新建文件夹；桌面走系统原生对话框） | 列目录失败时 UI 提示授予"所有文件访问" |
| `write_text_file` / `read_text_file` | 会话导出/导入 JSON（路径经系统保存/打开对话框取得） | 本地文件 IO |
| `open_book_file` / `get_reader_content` | 按**书籍归属书库**解析 EPUB 路径（归属书库 → 当前书库 → 遗留 library_path；多书库/移动端必需），书库副本优先、原始文件兜底 | 多书库下旧逻辑用遗留字段会定位错目录 |
| `chatbot_chat` | 书虫 Agent：工具调用循环（检索本地书库），回复携带 `steps` 工具调用轨迹（工具名/实参/结果摘要） | 前端以可折叠记录展示，非黑盒 |

### 14.2 前端结构（frontend/src/）

- 页面：`LibraryPage`（书架 + 全部导入入口）、`BookDetailPage`（详情/短评/来源管理/阅读器）；
- 组件：`AddBookModal`（搜索→豆瓣→确认三步弹窗）、`ReviewModal`（已读 AI 书评会话）、`ReaderModal`、`SettingsModal`（书库/LLM/WebDav 三页签）、`Chatbot`（书虫，多会话 localStorage）、`FilterSidebar`/`SearchBar`/`BookCard`/`SourcesModal`/`EditMetaModal`/`PathPickerHost`（移动端文件浏览器）等；
- 图片一律经命令通道取字节（`read_cover_file`/`fetch_cover_image`）→ blob URL + 会话级 Map 缓存 —— 不用 convertFileSrc/asset 协议（Android WebView 下对绝对路径不可靠）；
- 长任务进度统一监听 `task-progress` 事件；批量条可打断（停止剩余 + `cancel_task` 中断当前）。

**导入交互细节**：加书弹窗承担交互确认职责 —— 书名/作者/标签可编辑（每字段带"翻译为中文"按钮，走 LLM）、展示封面、系列字段在确认步落库；无/多结果时进入搜索页（可改关键词、分页浏览、**按顺序多选 = 合并本**）；非中文书名必须确认后才可导入。「批量加书」为分裂按钮（点击选文件夹递归扫描，箭头/悬浮展开菜单），「唯一结果自动导入」开关在下拉菜单内（与 CLI batch 语义一致）。弹窗不启用"点击外部关闭"（避免拖拽选择文本时鼠标释放到蒙层误关）。

**封面上传与截取**（`EditMetaModal`）：「上传封面…」选图后进入截取模式（`CoverCrop`）—— 固定 **2:3 视口**（与全应用封面展示比例一致）cover 式取景，拖动平移、滚轮/滑杆缩放（锚点缩放，`offset/scale` 反演到源图像素坐标，canvas 按原图分辨率抠取不放大）；PNG 源输出 PNG（保留透明）、其余输出 JPEG 0.92；可「使用原图」跳过截取。字节经 `save_dropped_file` 落临时文件后走既有 `upload_cover` 链（内容去重 + EPUB 同步 + DB 更新），成功后 `onClose(true)` 通知调用方刷新（详情页 `setRefresh` / 书虫 `mna:library-refresh`）。

**书架交互细节**：视图三态（网格/列表=封面左信息右/仅封面）+ 封面大小三档（大/中/小，窄屏默认小），localStorage 持久化；排序（添加顺序/最近/书名/作者/系列，中文经 `Intl.Collator` 拼音序）；侧栏筛选（阅读状态/作者/标签，多作者按顿号拆分 facet）；多选批量操作（设状态/删除/合并模式=有序选择后合并）；阅读状态徽章可点击切换，切到"已读"弹短评询问（可与 AI 多轮讨论后生成）；**返回顶部浮动按钮**（滚离顶部显示 ↑，点击平滑回顶后变 ↓ 可返回原位，用户再次下滑自动重置 —— 下滑判定用滚动增量方向而非绝对位置，避免回顶动画误触发）；监听 `mna:library-refresh` CustomEvent 刷新书架（书虫写操作完成后广播）。

**设置页**：书库（多书库 CRUD/切换/主题色/默认标签/路径迁移）/ LLM（多 Provider + 默认服务商/模型 + 重试次数 + **token 预算与进度条 + 模型价格表编辑 + 用量/成本统计（逐模型与合计，"清零用量"重置预算周期）**）/ WebDav（按书库独立开关与凭据、远端位置预览 `{remote_dir}/{书库名}`、测试连接/保存/推送/拉取，同步前自动保存配置）。

**书虫 Chatbot**：回复经轻量 Markdown 渲染（`utils/markdown.ts`，先转义再转换防注入）；assistant 消息携带工具调用轨迹（可折叠查看实参与结果摘要）；多会话管理（localStorage `mna-chat-sessions`），会话可导出/导入完整上下文 JSON。

**来源管理弹窗**（`SourcesModal`）：逐条展示 clasp 来源的系列信息（导入与"更新数据"时爬取，打开页面不回拉）；工具栏「重设系列」（按来源继承规则）「重设标签」；版本封面切换（editions JSON）、设为封面/设为简介、按来源重抓短评。

### 14.3 拖拽导入（HTML5 通道）

**背景**（实证结论）：wry 的原生 `tauri://drag-*` 通道在窗口化托管下收不到事件 —— 拖拽被 WebView2 内部输入窗口（"Chrome Legacy Window"）接管，wry 在我们窗口子树上注册的 OLE drop target 永远不会被调用；同时 wry 的 `AllowExternalDrop(false)` 屏蔽了 HTML5 兜底 → 拖拽完全无响应。

**方案**：`tauri.conf.json` 关闭 `dragDropEnabled` → WebView2 保持默认 `AllowExternalDrop=true` → Chromium 把外部拖拽转成标准 DragEvent。页面拿不到本地路径（浏览器安全限制，只有 File 内容）→ `webkitGetAsEntry` 递归展开文件/文件夹（readEntries 每批 ≤100，循环读到空）→ 逐个 `File.arrayBuffer()` → `save_dropped_file` 命令（**字节走 IPC 原始请求体**避免 JSON 序列化大数组；文件名 encodeURIComponent 后放 `filename` 请求头）落临时文件（`temp/drag-import/`，启动清扫）→ 拿到路径后复用路径式导入流程。`dragenter/leave` 计数驱动遮罩；`App.tsx` 全局拦截文件类拖放防止页面被导航替换。

**进度条跨阶段连续显示**：读取（`batchProgress` 真进度条）→ 分析（单本接管文案"分析书籍信息"，含数秒网络匹配；「＋ 加书」按钮流程同样显示）→ 批量导入（`runBatch` 文案），全部结束后统一清理——分阶段清空会造成"闪一下就消失"的空窗。两个实证踩过的坑：① 读取/分析/批量是同一流程的续接段，**续接函数不得带 `addBusy` 守卫**（读取阶段 batchStatus 已置位 → addBusy 恒 true → 早退 → 只完成落盘、导入从未启动；并发防护只放在 HTML5 drop 入口）；② 临时文件名带 `{时间戳}-{序号}-` 前缀（保唯一），展示时经 `dropDisplayName` 剥离还原原始文件名。

### 14.4 书架滚动位置持久化

localStorage `library.scrollY`；防抖 200ms 保存 + 卸载/pagehide 立即保存。三个必须用 ref 兜底的坑（全部实测踩过）：
1. 卸载时 DOM 已被路由替换、文档骤降把 `window.scrollY` 钳制成 0 → 以 `scrollYRef` 记录的最后真实位置为准；
2. 返回书架瞬间文档短暂变矮派发钳制滚动事件 → 恢复流程完成前（`scrollReadyRef`）忽略一切滚动事件；
3. StrictMode（开发模式）挂载即清理会把全新 ref 的 0 写进存储 → `hasScrolledRef`：记录过真实滚动才允许写入。
恢复侧：书籍加载完成后 `scrollTo` + 短暂重试（封面延迟加载导致文档高度暂时不足会被钳制），用户主动滚动立即停止重试。

## 15. 移动端（Android）

- 双目标结构：`src-tauri/src/lib.rs`（`#[cfg_attr(mobile, tauri::mobile_entry_point)] run()`，桌面/移动共用）+ `main.rs` 薄壳；`crate-type` 含 cdylib 供 `libmystery_gui_lib.so` 加载；
- `dirs::` 系列在 Android 失效 → 启动 `set_data_dir_override(app_data_dir)` 重定向 config/数据库/临时目录（`temp_dir()` → cache 子目录）；
- 权限：`MANAGE_EXTERNAL_STORAGE`（所有文件访问）—— 首次使用引导跳系统设置（`storage_permission.rs` 内联插件 + Kotlin `StoragePermissionPlugin`）；
- 内置文件浏览器（`PathPickerHost`）替代系统选择器；`android dev` 的文件监听会因 CLI 自己写入 gen 资产反复 Rebuild → 用 `--no-watch` 或 `android build --debug` 出自包含 APK；
- 已知限制：USB 设备同步无意义；`open_book_file` 退化。

Android 构建（Windows）：① Android Studio（SDK 34+ / Platform-Tools / NDK）+ JDK 17（`JAVA_HOME`）；② `rustup target add aarch64-linux-android …` 四目标；③ **开启 Windows 开发人员模式**（tauri 需符号链接放 `.so`，否则报 "Creation symbolic link is not allowed"）；④ `cargo tauri android init`（已生成 gen/android）；⑤ `cargo tauri android dev`（vite devUrl 主机由 CLI 自动替换为局域网 IP）/ `cargo tauri android build`（APK/AAB）。

## 16. 关键设计决策 FAQ（防深挖）

**Q：为什么复制文件而不是直接改原文件？**
书库是可重建的资产：原始下载物保持字节不动（用户可能还想原样分享/重新整理）；副本随时可以从原文件重建，所以写副本无需确认弹窗。这也是数据不变量 #1，删除书籍时也只删副本。

**Q：为什么用拼音命名书库文件？**
跨平台可移植（无中文编码/文件系统差异问题）、可排序、避免 NFD/NFC 规范化差异（macOS 与 Windows 对同名中文文件判定不一致）；格式 `[系列拼音-N] 作者拼音-书名拼音.epub` 自带排序语义。数据库中的 `library_file` 才是权威引用。

**Q：为什么 LLM 只做"叶子"不做"大脑"？**
三个理由：(a) 可靠性 —— 简介融合、书评、检索问答是"锦上添花"，挂了走降级不影响主流程；把抓取/命名/入库交给 LLM 会引入幻觉与不可重试性；(b) 成本 —— 按 provider/model 记账可见，全库走对话式整理在 token 上不可行；(c) 可审计性 —— 书虫的工具调用轨迹逐步留痕，纯生成式回答无法验证。

**Q：为什么 rusqlite 同步调用在 async 上下文里可以接受？**
CLI 是单任务顺序流程；GUI 把 DB 调用放进 `#[tauri::command] async fn`（tauri 命令默认跑在专用线程池，不阻塞窗口线程），锁持有时间是毫秒级本地查询。WebDav 拉取后的整库覆盖是长事务，同样在命令线程内独占执行。真正的长网络调用全部不持锁。

**Q：为什么拖拽不走 Tauri 官方的 onDragDropEvent？**
实测（注入探针 + 合成事件 + 临时日志实证）：WebView2 窗口化托管下，拖拽被其内部输入窗口接管，wry 注册的 OLE drop target 收不到任何回调 —— 这是通道级失效而非时序问题，所以官方事件在 Windows 上对此应用不可用。HTML5 通道是浏览器原生行为，跨平台一致。（详见 §14.3）

**Q：为什么封面要内容去重 + 引用计数？**
同一封面 URL 下载的字节相同（多来源/重抓/合并），去重避免缓存膨胀；不同书籍记录可能引用同一文件，引用计数防止"删 A 书把 B 书封面删了"。启动时 `seed_cover_refs` 重算全部引用并清扫无主文件。

**Q：为什么豆瓣简介要限定 `#link-report` 区？**
豆瓣页面里内容简介和作者简介用同款 `span.short/span.all hidden` 结构；全文档取第一个 `div.intro` 在长简介（截短结构）或特定版式下会串到作者简介。限定内容简介容器 + `span.all` 优先是实测两个真实页面（26771719 短/30354903 长）得出的选择器，配了三个单元测试防回归。

**Q：为什么短评要 ≥15 汉字 + 有用数 top5？**
数据质量优于数量（AGENTS.md 原则 4）：太短的评论（"好看""一般"）对知识库没有价值；有用数是豆瓣社区的质量信号。不足 5 条时放宽取最长 5 条 —— 保证冷门书也有内容。

**Q：为什么自己手写 WebDav 而不用库？**
需求只有四个动词（MKCOL/PROPFIND/PUT/DELETE）+ 特殊的"整库覆盖"语义；reqwest 手发 XML 即可，避免引入带额外运行时/依赖树的 WebDAV 客户端（AGENTS.md：不随意引入新依赖）。

**Q：为什么用 Tauri 而不是 Electron？**
Rust 核心库直接被复用（同一 crate 作为 path 依赖）；体积与内存远小于 Electron；还免费得到 Android 目标。代价是 WebView2 的一些平台怪癖（如上述拖拽），都有解法。

**Q：CLI 和 GUI 怎么保证行为一致？**
共用核心库（ingest 的匹配规则、系列继承、降级链是同一份代码）；差异只在交互层 —— GUI 的静默规则对齐 CLI batch 语义（唯一结果自动导入开关）；两边遵循同一份 AGENTS.md 约束。

## 17. 测试策略

`cargo test` 49 个单元测试全绿（核心库 47 + GUI crate 2），**按"不变量"而非"覆盖率"组织**：

| 领域 | 测试 | 守住的不变量 |
| :--- | :--- | :--- |
| 爬虫解析 | `parse_douban_description(_long/_short)`、`filter_comments_top5`、`extract_douban_id`、`book_item_to_suggestion`、`search_mode_fuzzy`、`clasp_series_real_format` | 选择器防改版回归；长简介取完整版不串作者简介；短评质量规则 |
| 导入核心 | `pick_silent_match_unique/exact_title`、`inherit_series`、`crawled_title_author`、`persist_import`、`write_epub_metadata_with_cover`、`edition_label` | 静默匹配规则；系列继承规则；爬取优先回填；端到端入库 |
| EPUB 修复 | `fix_opf_broken_href`、`dedupe_opf_manifest`、`epub_cover_fallback` | 损坏 zip/OPF 自动重建 |
| 命名 | `slugify`、`filename` | 拼音命名规范与冲突处理 |
| 数据库 | `cover_ref_lifecycle`、`replace_library_from_remote`、`vacuum_into_snapshot`、`match_urls_and_douban_comments`、`record_llm_usage`、`set_book_status`、`rebase_cover_path_cross_platform` | 封面引用计数；WebDav 整库覆盖；用量记账；状态白名单 |
| LLM 配置 | `resolve_settings_priority`、`resolve_env_fallback`、`resolve_model_fallback`、`resolve_skip_empty_key`、`budget_error`、`budget_error_not_retryable`、`price_for` | 多 Provider 解析优先级与环境变量兜底；预算边界（达到/超过/未配置）；价格解析（显式覆盖 > 预设最长前缀） |
| 书虫写工具 | `chatbot_action_tools_pending_confirmation`、`format_clasp_results`（GUI crate） | 写工具仅校验并返回待确认标记，不直接执行；状态白名单 / review 互斥 / 合并 ≥2 本等参数校验；在线检索回包的来源标注 / fuzzy 标记 / 简介截断 |
| 其他 | `reader::sanitize_and_inline`、`resolve_ref`、`base64_encode`、`shelf::render_shelf`、`webdav` 四个、`utils` 三个 | 阅读器消毒/路径归一；WebDav URL 编码；中文工具 |

验证链：`cargo build`（零 error）→ `cargo test`（全绿）→ `cargo run -- --help`（CLI 结构）→ `npm run build`（tsc + vite）→ 手动冒烟（拖拽导入/书架/详情）。GUI 侧新增逻辑（拖拽字节流、滚动持久化）通过注入探针（`debug_log` 命令 + app.log）在真实运行的应用中实证过端到端路径。

## 18. 已知限制与未来方向

- **RAG 全书问答**：`embeddings` 表已预留（chunk_text + vector BLOB），后续接 sqlite-vec / candle —— 书虫 Agent 目前靠元数据+短评而非全文检索；
- MTP 设备同步（新款 Kindle）需 libmtp/Windows MTP API，暂不支持；
- 多 renditions EPUB 不支持（rbook 限制）；
- WebDav 是整库覆盖语义，无三方合并；
- merge 的 CSS 样式可能退化（保文字与图片优先）；
- 阅读器对超预算图片跳过内联（64MB 上限）。

## 19. 技术栈与配置文件

| 类别 | Crate |
| :--- | :--- |
| 异步 | `tokio`（full；GUI 侧精简 features） |
| HTTP/解析 | `reqwest`（rustls-tls）、`scraper`、`regex`、`percent-encoding` |
| EPUB | `rbook`（读/写/封面/章节/路径重写）、`zip`/`flate2`（损坏修复） |
| 数据库 | `rusqlite`（bundled + functions：注册 SQLite 自定义函数） |
| 中文 | `pinyin`、`character_converter`（繁→简） |
| CLI | `clap`（derive）、`dialoguer`、`comfy-table` |
| GUI | `tauri 2`、`tauri-plugin-dialog/opener`、React 18 + Vite + TypeScript |
| 系统/其他 | `sysinfo`、`dirs`、`dunce`、`toml`、`serde`/`serde_json`、`tracing` |

配置文件 `%APPDATA%\mystery-novel-agent\config.toml`（Linux `~/.config/`），结构：

```toml
[[libraries]]                     # 多书库（id 唯一；name 为 slug，限 [A-Za-z0-9_]，即 WebDav 远程子目录名）
id = "default"; name = "default"; title = "默认书库"
path = "/path/to/library"; default_tags = ["推理小说"]
[libraries.webdav]               # 每书库独立开关与凭据
enabled = false; url = "https://dav.jianguoyun.com/dav/"
username = "…"; password = "…"    # 应用专用密码；remote_dir = "mystery-novel-agent"

[llm]                            # GUI 设置页写入；优先于 OPENAI_* 环境变量
default_provider = "DeepSeek"; default_model = "deepseek-chat"; retry_count = 2
budget_tokens = 5000000          # Token 预算：累计用量达到后拒绝新的 LLM 调用（缺省不限）
[[llm.providers]]
name = "DeepSeek"; base_url = "https://api.deepseek.com/v1"
api_key = "sk-…"; models = ["deepseek-chat"]
[[llm.pricing]]                  # 模型价格（元 / 百万 tokens；未配置的模型按内置预设表估算）
model = "deepseek-chat"; input_per_m = 2.0; output_per_m = 8.0
```

数据库默认 `%APPDATA%\mystery-novel-agent\mystery_novel.db`（CLI/GUI 共用）；`library_path`/`covers_path`/`database_path` 为旧版单书库字段（已迁移进 libraries）。GUI 日志双写 stdout 与数据目录 `app.log`（release 下 stdout 不可见，文件用于事后诊断）。

## 20. 构建与调试（重要坑）

- **CLI**：仓库根 `cargo build` / `cargo test`；WSL 环境 `wsl -d Ubuntu -e bash -lc "cd ~/mystery-novel-agent && cargo ..."`。
- **GUI 开发**：`npm run tauri dev`（自动起 vite dev server + cargo）。
- **GUI 出包**：`npm run tauri build`。
- ⚠️ **裸 `cargo build` 出的 debug 二进制加载 devUrl**（需要 5173 端口 dev server 在跑），**不嵌入前端** —— 必须经 tauri CLI 构建（CLI 内部以 `--features tauri/custom-protocol` 构建：该特性使 `generate_context!` 改为内嵌 frontendDist 并忽略 devUrl）；如需不经 CLI 的独立构建：`cargo build --manifest-path src-tauri/Cargo.toml --features tauri/custom-protocol`。
- **开发模式自检**（`check_dev_server`，`#[cfg(all(dev, desktop))]`）：dev 构建且 devUrl TCP 探测不可达时，启动即弹原生错误弹窗给出三条出路（tauri dev / 独立构建加特性 / tauri build）并退出 —— 替代原来的"白屏打不开"；`tauri dev` 场景 CLI 先等 dev server 就绪再拉起应用，探测正常通过不误弹。
- **前端改动与嵌入**：dist 资产经 `generate_context!` 宏嵌入，修改前端后要经 tauri CLI 重新构建；怀疑嵌入过期时 `cargo clean -p mystery-gui` 强制重编译。
- **Android**：见 §15。
- **诊断**：GUI 的 Rust 侧 `tracing` 日志 → `app.log`；前端可 `invoke('debug_log', {msg})` 转发；`RUST_LOG` 覆盖级别。

### 20.1 CI 自动构建与发布（双远端）

**GitLab**（origin = git.tsinghua.edu.cn，`.gitlab-ci.yml`）—— Linux Docker runner，只构建两类产物（Android/macOS/iOS 移至 GitHub，GitLab 无对应 runner）：
- `build:linux-cli`：Linux 原生 CLI；`build:windows`：**cargo-xwin 交叉编译**（`cargo tauri build --runner cargo-xwin --target x86_64-pc-windows-msvc --bundles nsis`）→ NSIS 安装包 + Windows CLI；
- `publish:continuous` / `publish:release`：产物上传 generic package registry（continuous 同名覆盖 / `v*` 标签版本化），并维护**滚动 Release**（tag 固定 `continuous`：每次构建 DELETE 旧 Release 与 tag、再以当前提交经 Releases API 重建；tag 规则限定 `^v/` 防 CI 自建标签循环触发）。

**GitHub**（github remote = Alpha1022/MysteryNovelAgent，`.github/workflows/release.yml`）—— 托管 runner 原生环境五平台矩阵（推送映射 `git push github master:main`）：
- `build-windows`（windows-latest）：NSIS 安装包 + CLI；
- `build-linux`（ubuntu-24.04）：webkit2gtk-4.1 依赖 + **deb/AppImage** + CLI（`APPIMAGE_EXTRACT_AND_RUN=1` 免 FUSE）；
- `build-android`（ubuntu-latest）：runner 自带 Android SDK，补装 SDK 36/NDK 26 + JDK 17，`cargo tauri android build --apk --target aarch64 --target armv7` → 通用 APK；**签名**：gen/android 的 gradle 读环境变量 `KEYSTORE_FILE/KEYSTORE_PASSWORD/KEY_ALIAS/KEY_PASSWORD`，CI 从 Secrets `KEYSTORE_BASE64`（解码为文件）等四项注入 —— 构建前 keytool 预检口令与别名（快速失败，不等全量编译后签名才炸），缺 Secrets 时明确报错；
- `build-macos`（macos-latest，arm64）：`--target universal-apple-darwin` DMG（tauri 双架构 lipo 合并）+ CLI（手动 lipo 为 universal）；
- `build-ios`（macos-latest）：**Rust 编译检查**（`cargo build --target aarch64-apple-ios,aarch64-apple-ios-sim -p mystery-gui`，无产物）—— tauri 生成的 Xcode 工程的 Build Rust Code 阶段必须由 `tauri ios build/dev` 的服务进程编排（经临时目录 `{identifier}-server-addr` 文件通信），绕过 CLI 直接 `xcodebuild` 会在 read_options 处 panic；`ios build` 又面向真机且需签名（CLI 不支持模拟器目标）——故退化为编译验证（真机分发需 Apple 开发者证书经 Xcode 归档签名）；
- `publish`：download-artifact 汇总（merge-multiple）→ `gh release create -R $GITHUB_REPOSITORY`：main push → 滚动 Release `continuous`（正式版非预发布；`--cleanup-tag` 删旧重建、`--target $GITHUB_SHA`；gh 不经本地 git 解析仓库故必须 -R —— publish 无 checkout）；`v*` 标签 → 正式 Release（需单独 `git push github v0.1.0`）；
- 共性：tauri-cli 走 taiki-e/install-action 预编译（失败回退源码）、swatinem/rust-cache 缓存 Rust、setup-node 缓存 npm、并发去重（cancel-in-progress）；**gen/android 入库**（Manifest 权限/Kotlin 插件/签名配置，`gradlew` 在 git index 标记 755 —— Windows 提交默认丢执行位）。私有仓库注意 macOS runner 按 10 倍计费分钟数。
