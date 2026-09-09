# 推理小说阅读管理助手

本地优先的书籍管理工具：**Rust CLI + Tauri GUI（Windows 桌面 / Android）**。
导入 EPUB 时自动完成元数据增强（claspclub 匹配、LLM 简介融合、封面抓取、豆瓣短评精选），
提供统一拼音命名书库、豆瓣短评知识库、系列合并、内置阅读器、WebDav 多设备同步与"书虫"AI 阅读助手。
以推理小说为主要场景，但**同样适用于其他类型书籍**（手动提供豆瓣链接即可，见[下文](#不止推理小说)）。

**核心承诺：你自己的原始 EPUB 文件永远不被修改** —— 所有元数据只写入书库副本与本地数据库。

---

## 功能总览

| 模块 | 说明 |
| :--- | :--- |
| 书库管理 | 多书库（书库名 slug 唯一、独立主题色/固定标签），EPUB 以 `[系列拼音-N] 作者拼音-书名拼音.epub` 统一命名 |
| 智能导入 | 单本 / 批量（递归扫描）/ 拖拽；自动匹配 claspclub 条目 → LLM 融合无剧透简介 → 封面下载（OSS/豆瓣防盗链兜底）→ 豆瓣短评按有用数取 top5（≥15 汉字过滤）；无匹配时可手动粘贴豆瓣书籍页链接（任意类型书籍） |
| 书架 | 阅读状态（想读/在读/已读）、多维度筛选、拼音排序、实时搜索 |
| 内置阅读器 | 按章节渲染 EPUB 正文（统排样式），字号调节、进度记忆、移动端滑动手势；「排版」可选上下滚动或自动分页（左右/上下翻页，页高对齐行栅格不切割文字） |
| 书架视图 | 网格 / 列表（封面左、信息右）/ 仅封面三种显示方式，封面大小可调（窄屏默认小） |
| 系列合并 | 多选同系列书籍合并为单个 EPUB，资源重写 + LLM 融合简介，保留各卷来源与短评 |
| 书虫 AI | 右下角悬浮助手：可问答"我读过什么"、按书架推荐，回复携带工具调用轨迹；会话可导出/导入 |
| WebDav 同步 | 每书库独立配置（坚果云 / NextCloud 等），**推拉双向整库覆盖**，移动端与 PC 互通 |
| 数据统计 | LLM token 用量按服务商/模型累计 |

### WebDav 同步

- **同步到云端**：本地书库完整覆盖云端 —— 上传全部 EPUB/封面/数据库快照，删除云端多出的文件。
- **从云端同步**：云端完整覆盖本地 —— 下载 EPUB/封面，数据库按目标书库整库覆盖（含短评、来源、书评），
  删除本地多出的文件。
- 仅手动触发、**仅对当前书库生效**；内容一致的文件自动跳过；同步过程有进度条，可随时打断。
- ⚠️ 覆盖语义：任一方向的同步都会以源端为准，**目标端多出的书籍（含书评）会被移除**，请先想清楚方向。
- 推荐坚果云：在坚果云网页版「账户信息 → 安全选项」添加应用密码，填入 WebDav 页签即可。
- 凭据明文保存在本机 config.toml；远端布局 `{远程目录}/{书库名}/`，同名书库跨设备自动对齐。

---

## 使用方法

### GUI（推荐）

```bash
# 开发调试（桌面）：自动拉起 vite 与应用窗口
cargo tauri dev

# 或手动分别构建
cd frontend && npm install && npm run build && cd ..
cargo build --workspace
# 运行 target/debug/mystery-gui.exe
```

首次启动会引导**创建书库**（选择本地目录）。

- **加书**：工具栏「＋ 加书」选择单个 EPUB；「批量加书」选择文件夹递归导入
  （下拉菜单中可开启"唯一结果自动导入"——claspclub 唯一匹配时跳过确认直接入库）。
- **书架**：「显 示」菜单切换网格 / 列表 / 仅封面视图并调整封面大小；「多选」进入批量模式，
  可批量改状态/删除，或开启「合并模式」合并系列。
- **阅读**：详情页「阅读」打开内置阅读器；滚动模式下 `←/→` 键或点按左右边缘翻章，
  分页模式下翻页；移动端支持左右滑动手势；「排 版」可切换滚动 / 自动分页（左右或上下翻页）。
- **设置**：左上角「设 置」—— 书库 / LLM / WebDav 三页签。
  LLM 未配置时一切功能照常（简介直接拼接、书评手动输入），只是少了 AI 增强。

### CLI

```bash
cargo run -- add <EPUB 或文件夹>            # 导入并增强元数据（文件夹 = 批量）
cargo run -- add <EPUB> --merged            # 合并本模式（多选 claspclub 条目）
cargo run -- list [--limit N] [--search 关键词]
cargo run -- finish                         # 标记读完 + AI 书评对话
cargo run -- delete <ID>                    # 删除书籍（原始文件不动）
cargo run -- sync                           # 检测已连接的阅读设备并复制书籍
cargo run -- merge --series <系列名>        # 按系列名合并；或 --ids 1,2,3 按书籍 ID 合并
cargo run -- library set <PATH>             # 配置书库路径
cargo run -- shelf                          # 渲染 HTML 书架到书库目录
```

CLI 与 GUI 共用同一份 config.toml 与数据库。

### 数据位置

- 桌面：配置 `%APPDATA%/mystery-novel-agent/config.toml`（Windows）或
  `~/.config/mystery-novel-agent/config.toml`（Linux/macOS）；
  数据库同目录下 `mystery_novel.db`。
- Android：应用私有目录（`app_data_dir`），书库目录首次创建时由内置文件浏览器选择
  （建议选择应用私有目录，避免系统"所有文件访问"授权问题）。

### 试用指南

仓库 `sample/` 目录下提供了两本可供试用的 EPUB（详见 [sample/README.md](sample/README.md)）：

- 绫辻行人《钟表馆事件》—— 长篇小说代表；
- 大山诚一郎《密室收藏家》—— 短篇小说集代表。

两本书在推理小说数据库[暗扣 (claspclub)](https://claspclub.com/) 与豆瓣上均有资料，
可以完整体验"匹配 → 元数据增强 → 短评抓取"的导入流水线。快速上手：

1. 克隆仓库后先安装前端依赖：`cd frontend && npm install && cd ..`；
   然后 `cargo tauri dev` 启动应用，创建一个书库（目录随意，书会被复制进去，原始文件不动）；
2. 「＋ 加书」选择 `sample/` 下的 EPUB → 在搜索结果中确认匹配（可编辑书名/作者/标签）；
3. 导入后在详情页查看封面、无剧透简介、豆瓣短评；「阅读」试读内置阅读器；
4. 想体验 AI 增强（LLM 简介融合、书虫助手、AI 书评），先在「设置 → LLM」填入任意
   OpenAI 兼容服务的 Endpoint 与 API Key —— 未配置时一切功能照常，只是少了 AI 部分。

### 不止推理小说

目前的自动化数据源以推理小说为主（claspclub 为推理小说专门站），但**本工具同样适合管理其他类型的书籍**：

- **豆瓣是通用数据源**：导入时若 claspclub 无匹配，可以跳过搜索、直接粘贴
  `book.douban.com/subject/...` 书籍页链接（每行一条，支持同一本书的多个版本），
  即可回填书名/作者/简介/封面并抓取豆瓣短评 —— 文学、社科、漫画、轻小说均可如此入库；
- **导入后随时补挂**：详情页「来源」管理弹窗可随时粘贴豆瓣链接添加来源、重抓短评、
  将某来源的封面/简介设为书籍主数据；
- **标签体系自定义**：每个书库可设置自己的固定标签（默认"推理小说"，可改为
  "文学"、"轻小说"等），标签亦可逐本编辑，因此可以为不同类型的书分别建库。

---

## 构建

### 环境要求

- Rust 1.85+（edition 2024）
- Node.js 18+ 与 npm（前端为 React + Vite）
- Tauri CLI：`cargo install tauri-cli --version ^2`
- Android 构建（可选）：Android Studio（SDK + NDK r27+）、JDK 17、
  环境变量 `ANDROID_HOME` 与 `NDK_HOME`

### 桌面

```bash
cargo install tauri-cli --version "^2"
cd frontend && npm install && cd ..
cargo tauri dev        # 开发调试
cargo tauri build      # 产出安装包（target/release/bundle/）
```

### Android

```bash
cargo tauri android init        # 仅首次
cargo tauri android dev         # 真机/模拟器调试
cargo tauri android build       # 产出 APK/AAB
```

> **移动端存储说明**：
> - 封面缓存固定存放于**应用私有目录**（`{应用数据目录}/covers`），不会被系统图库收录，也不受书库目录权限影响；
> - EPUB 仍保存在创建书库时选择的目录。若选择公共存储（如 `/storage/emulated/0/...`），
>   需在系统设置中授予应用「所有文件访问」权限 —— 同步遇写入失败时会弹窗引导一键跳转授权页；
> - 不想授予权限的话，把书库目录设为应用私有目录即可（`fs_roots` 中的应用数据目录），无需任何授权。

### 仅 CLI

```bash
cargo build --release
# 产物：target/release/mystery-novel-agent(.exe)
```

### 测试与静态检查

```bash
cargo test --workspace          # 单元测试（数据库/同步/爬虫解析等）
cd frontend && npm run build    # tsc 类型检查 + 前端打包
cargo run -- --help             # 验证 CLI 结构
```

---

## 调试

- **日志**：`tracing` 全局日志，`RUST_LOG=debug cargo tauri dev` 提升级别；
  WebDav 同步、导入降级链的关键节点均有 info/warn 日志。
- **桌面 DevTools**：debug 构建下右键 → 检查（Inspect），Network 页可看到全部 `invoke` IPC 调用。
- **Android**：`adb logcat -s tauri` 查看原生日志；debug 构建可在 chrome://inspect 调试 WebView。
- **同步排查**：
  - 连接失败 → 检查地址是否以 `http(s)://` 开头、账号/应用专用密码；
  - 云端目录为空 → 需先在另一台设备「同步到云端」；
  - 封面不显示 → 封面文件名为内容哈希，跨设备自动重定位到本地书库 `covers/` 目录，
    确认推送端已执行过一次「同步到云端」。

## 架构速览

```text
src/            Rust 核心库（CLI 与 GUI 共用）
├── main.rs       clap CLI 路由
├── ingestion.rs  导入流水线（搜索→选择→增强→写库副本→入库）
├── spider.rs     claspclub API + 豆瓣短评解析
├── agent.rs      LLM 客户端（多 Provider）
├── webdav.rs     WebDav 推送/拉取（整库覆盖）
├── db.rs         SQLite（建表/迁移/查询/远端快照覆盖）
├── config.rs     config.toml（多书库 + 多 Provider LLM + WebDav）
└── ...

src-tauri/      Tauri GUI 壳（桌面 + Android）
frontend/       React + Vite 前端
```

依赖分层无环：`ingestion → {spider, agent, db, config, library}`。详见 [ARCHITECTURE.md](ARCHITECTURE.md)。

## 开发方向

以下为规划中的方向（**尚未实现**），按优先级大致排序：

1. **轻小说数据源**：基于[轻小说机翻机器人](https://n.novelia.cc/)设计新的数据源，
   支持 web 小说与文库本轻小说的元数据匹配与内容管理，与现有 claspclub / 豆瓣来源并列。
2. **论文支持**：在 EPUB 之外增加对论文的管理 —— 提取摘要与正文内容供 agent 理解，
   并基于内容自动生成标签。
3. **数据源插件化**：将 claspclub、豆瓣、轻小说机翻机器人这些数据源重构为插件形式，
   提供统一的数据源接口，支持用户自行编写扩展接入新的站点。

## 许可

仅供个人学习与阅读管理使用；claspclub / 豆瓣数据版权归原站所有。
