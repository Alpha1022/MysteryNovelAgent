# AGENTS.md

本项目是一个 **"推理小说阅读管理助手"**，使用 **Rust**（edition 2024）编写的本地优先书籍管理工具。核心能力：**EPUB 元数据智能增强**（claspclub API 匹配 + LLM 简介融合）、**豆瓣短评知识库**、**拼音统一书库**、**设备同步** 与 **系列合并**。

> **入口状态**：GUI（Tauri，`src-tauri/` + `frontend/`）是唯一积极维护的入口；**CLI（`src/main.rs`）已弃用** —— 源码保留、行为不再演进、CI 不构建分发，新功能只做 GUI。CLI 与 GUI 共用 `src/` 核心库（改核心逻辑时两边行为保持一致仍是被测不变量）。

作为参与此项目的 AI 编程 Agent，请严格遵循以下规范。

---

## 1. 核心原则（按优先级）

1. **原文件不动**：`add` 导入时先复制入书库，元数据（标题/作者/标签/简介/系列/封面）**只写入书库副本**。任何代码不得修改用户的原始 EPUB。
2. **优雅降级，禁止 panic**：网络爬取极易失效（豆瓣反爬、claspclub 改版、LLM 不可用）。每条外部依赖都必须有降级路径（详见 §5），失败时 `warn!` 日志 + 降级继续，**禁止 unwrap()/expect()**（测试代码除外）。
3. **静默与交互的边界**（必须精确遵守）：
   - **全静默**：批量模式（`batch=true`）下搜索唯一结果 —— 自动选中、跳过编辑、直接写库。
   - **必须交互**：多结果选择、合并本多选、手动录入、**非中文书名确认**（批量模式也不例外）。
   - 写入书库副本**无需确认**（副本可随时重建）。
4. **数据质量优于数量**：豆瓣短评必须 ≥15 汉字、按有用数降序取 top5（不足时放宽取最长 5 条）。
5. **代码规范**：**2 空格缩进**；`snake_case` 函数 / `PascalCase` 类型；公共 API 写 `///` 文档；scraper 选择器必须加 HTML 结构来源注释；关键逻辑写单元测试。

---

## 2. 模块架构（与代码同步，详见 ARCHITECTURE.md）

```text
src/
├── main.rs        # clap CLI 路由：add/finish/list/delete/sync/merge/library
├── ingestion.rs   # 【核心】导入流水线，公共入口：
│                  #   pub async fn ingest_book(path, conn, opts: IngestOptions)
│                  #   IngestOptions { merged: bool, batch: bool }
├── spider.rs      # claspclub API 结构体与请求 + 豆瓣短评解析/过滤
├── agent.rs       # LLM 客户端 + combine_descriptions（简介融合）
├── finish.rs      # 标记读完 + 苏格拉底式 AI 书评（FinishError）
├── db.rs          # SQLite：建表/幂等迁移/查询函数（books/comments/embeddings）
├── config.rs      # AppConfig：config.toml 读写
├── library.rs     # slugify 拼音命名 / copy_into_library / resolve_collision
├── device.rs      # 设备检测 + sync 命令
├── merge.rs       # EPUB 系列合并（merge.rs::run_merge）
└── utils.rs       # count_han / 标签拆分合并 / filename_stem
```

依赖分层无环：`ingestion → {spider, agent, db, config, library}`；不要让底层模块反向依赖上层。

---

## 3. 技术栈约束

| 类别 | Crate | 备注 |
| :--- | :--- | :--- |
| 异步 | `tokio` (full) | CLI 场景 rusqlite 同步调用可接受 |
| HTTP | `reqwest` (rustls-tls, json) | 详情/搜索 15s，LLM 30s 超时 |
| EPUB | `rbook` | `edit().clear_meta().title()...`、`cover_image`、`EpubRewriteOptions` |
| 数据库 | `rusqlite` (bundled, functions) | 需 functions feature 注册自定义函数 |
| 中文 | `pinyin`、`character_converter` | 拼音命名 / 繁→简 |
| CLI/展示 | `clap` (derive)、`dialoguer`、`comfy-table` | |
| 系统 | `sysinfo`、`dirs`、`dunce`、`toml` | |

不要随意引入新依赖；等效能力优先用现有 crate。

---

## 4. 外部 API 规则

### claspclub
- **搜索建议**：`GET https://claspclub.com/api/v1/search/suggestions?keyword={urlencode}` → `ClaspSuggestionResp.books[]`（title/authorName/id/tags/coverUrl），CLI 交互式流程使用
- **分页搜索**：`GET https://claspclub.com/api/v1/books?keyword={}&sort=doubanRating&page={}&pageSize=5` → `ClaspBooksResp{ items[]（id/title/authors[].name/coverUrl/summaryNoSpoiler/tags[].name/doubanRating）, pagination.totalPages }`；条目自带无剧透简介 → 导入时省去详情调用（注意：**不含系列信息**）；GUI 搜索页/analyze 使用
- **详情**：`GET https://claspclub.com/api/v1/books/{id}` → `summaryNoSpoiler`（简介）、`coverUrl`、`series{name,order}`、`editions[].doubanUrl`（isPrimary 优先），仅当搜索结果未携带简介时回退调用
- 结构体定义集中在 `spider.rs`，字段变化只改那里。

### 豆瓣
- 短评页：`{book_url}/comments/`，解析 `li.comment-item`（评分 allstarXX/10、`span.short`、`span.vote-count`），每套选择器必须有兜底。
- 封面兜底：书籍页 `og:image`（随机 `bid` cookie + 浏览器 UA），图片下载带 `Referer: https://book.douban.com/`。
- 阿里云 OSS 封面必须带 `Referer: https://claspclub.com/`。

### LLM
- OpenAI 兼容 `/chat/completions`；配置来源优先级：GUI 设置页（config.toml `llm` 段，**多 Provider**（各含 base_url/api_key/models）+ 默认服务商/模型）→ `OPENAI_API_KEY` / `OPENAI_BASE_URL` / `MODEL_NAME` 环境变量；未配置返回 `None` 并降级。每次 LLM 调用的 token 用量按 "provider/model" 累计入 `llm_usage` 表。旧版单 provider 平铺 llm 配置在 `AppConfig::load` 自动迁移。
- **预算与成本**：`llm.budget_rmb`（元）设定后，`agent::chat_with_tools_timeout`（所有 LLM 调用的统一咽喉）在每次调用前把累计用量按价格表换算成成本并与预算比较，达到即返回 `LlmError::Budget`（不可重试）——融合/翻译降级、书虫报错；未定价模型计 0 成本（预算不含其用量）；`reset_llm_usage` 清零用量（预算周期重置）。成本按 `llm.pricing`（元/百万 tokens，精确匹配）或内置预设价（前缀匹配）换算，设置页展示。
- 请求体显式 `stream: false`；消息序列必须 system→user 开头（部分 API 拒绝 system→assistant）。

---

## 5. 降级链（每条外部依赖的失败路径）

| 依赖 | 失败时 |
| :--- | :--- |
| claspclub 搜索 | → 手动录入书名+作者 |
| claspclub 详情 API | → 跳过该字段，继续流程 |
| LLM 简介融合 | → 多条简介 `\n\n` 拼接 |
| 封面 OSS | → 豆瓣 og:image → 放弃封面 |
| 豆瓣短评 | → warn 日志，跳过 |
| finish 中 LLM | → `/skip` 手动多行输入书评 |

---

## 6. 书库与命名规范

- 书库路径：`config.toml` 的 `library_path`；封面缓存 `{library}/covers`。
- EPUB 文件名（`library.rs::to_ascii_filename`）：
  `[系列拼音-N] 作者拼音-书名拼音.epub`，音节间连字符，仅 `[a-z0-9-]`，冲突追加 `-2`。
- DB 中 `library_file` 是书库内规范文件名，是 sync/merge 的实际数据源；`file_path` 仅溯源。
- 合并本：`clasp_ids` 存 JSON 数组；合并书 `merged_from` 存来源本地 ID JSON 数组。

---

## 7. 数据不变量

- `books.tags` 逗号分隔字符串；每本书必须含 **"推理小说"** 标签（无则追加）。
- 书名必须为简体中文（`character_converter::traditional_to_simplified` 自动转换）；非中文标题必须交互确认。
- 系列（`calibre:series`/`calibre:series_index`）仅当所有匹配条目同系列同卷号时继承。
- `comments.is_mine=1` 表示用户/AI 生成的个人书评，`source='AI助手生成'`。

---

## 8. 构建与验证

```bash
cargo build          # 必须零 error
cargo test           # 现有 8 个测试必须全绿（新增逻辑须带测试）
cargo run -- --help  # 验证 CLI 结构
```

- WSL 环境：命令用 `wsl -d Ubuntu -e bash -lc "cd ~/mystery-novel-agent && cargo ..."`。
- 涉及 rusqlite 自定义函数（如 Calibre 触发器所需的 `title_sort`）需 `functions` feature。

---

## 9. 未来扩展预留

- **RAG 全书问答**：`embeddings` 表已预留（`chunk_text TEXT, vector BLOB`），后续可接 sqlite-vec / candle。
- **GUI**：业务逻辑（spider/merge/library/device）均不依赖终端交互，仅依赖 `Path` + `Connection`，可直接被 Tauri 等前端复用；仅需替换 config 读取与交互确认层。
- 多 renditions EPUB、MTP 设备同步（需 libmtp/Windows MTP API）暂不支持。
