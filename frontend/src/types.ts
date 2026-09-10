export interface BookCard {
  id: number;
  title: string;
  author: string;
  tags: string;
  status: string;
  cover_path: string | null;
  series_name: string;
  series_order: number | null;
  clasp_ids: string;
}

export interface BookDetail {
  id: number;
  title: string;
  author: string;
  tags: string;
  status: string;
  description: string | null;
  cover_path: string | null;
  my_review: string | null;
  finished_date: string | null;
  series_name: string | null;
  series_order: number | null;
  clasp_ids: string | null;
  merged_from: string | null;
  /** 豆瓣书籍页链接（JSON 数组字符串） */
  douban_urls: string | null;
  library_file: string | null;
  file_path: string | null;
}

export interface CommentRow {
  id: number;
  rating: number | null;
  content: string;
  usefulness: number;
  source: string;
  is_mine: number;
  /** 来源项目（clasp ID / 豆瓣链接）；旧数据与个人书评为 null */
  source_ref: string | null;
}

/** claspclub 版本封面（预爬本地的 editions JSON 元素） */
export interface EditionCoverMeta {
  label: string;
  url: string;
  path: string | null;
}

/** 来源项目（来源管理界面） */
export interface SourceItem {
  kind: "clasp" | "douban";
  ref_key: string;
  position: number;
  title: string | null;
  author: string | null;
  cover_url: string | null;
  cover_path: string | null;
  summary: string | null;
  /** 系列信息（仅 claspclub 来源有；旧数据可能缺失） */
  series_name: string | null;
  series_order: number | null;
  /** 标签（仅 claspclub 来源有） */
  tags: string[];
  editions: EditionCoverMeta[];
}

/** 简介操作结果（翻译 / 合并） */
export interface DescriptionResult {
  description: string;
  model: string | null;
  degraded: string | null;
}

/** merge_books_gui 返回 */
export interface MergeResult {
  id: number;
  title: string;
}

export interface ConfigInfo {
  library_path: string | null;
  covers_dir: string | null;
  /** 书库列表（多书库管理） */
  libraries: LibraryProfile[];
  /** 当前书库 ID */
  current_library: string | null;
  /** 当前书库的固定标签（导入时强制添加） */
  default_tags: string[];
}

/** 书库主题色（未设置的项回退内置默认） */
export interface LibraryThemeCfg {
  accent?: string | null;
  bg?: string | null;
  panel?: string | null;
  panel2?: string | null;
  ink?: string | null;
  muted?: string | null;
  read?: string | null;
  reading?: string | null;
  wish?: string | null;
}

/** 书库的 WebDav 同步配置 */
export interface WebDavCfg {
  enabled: boolean;
  url: string;
  username: string;
  password: string;
  remote_dir: string;
}

/** WebDav 同步结果（推送 / 拉取共用） */
export interface WebDavReport {
  total: number;
  uploaded: number;
  downloaded: number;
  /** 完整覆盖语义：本次删除的云端/本地多余文件数 */
  deleted: number;
  skipped: number;
  failed: number;
  /** 数据库覆盖：云端有而本地无 → 并入 */
  books_imported: number;
  /** 数据库覆盖：本地有 → 用云端数据覆盖 */
  books_updated: number;
  /** 数据库覆盖：本地有而云端无 → 删除 */
  books_deleted: number;
  errors: string[];
}

/** 书库配置项 */
export interface LibraryProfile {
  id: string;
  /** 书库名（slug）：仅 [A-Za-z0-9_]，本地唯一；WebDav 远程子目录名 */
  name: string;
  title: string;
  path: string;
  theme: LibraryThemeCfg;
  default_tags: string[];
  webdav: WebDavCfg;
}

/** 内置阅读器内容 */
export interface ReaderContent {
  title: string;
  chapters: { title: string; html: string }[];
}

/** 内置主题预设 */
export interface ThemePreset {
  name: string;
  colors: LibraryThemeCfg;
}

export interface ClaspMatch {
  id: string;
  title: string;
  author: string;
  tags: string[];
  cover_url: string | null;
  /** 无剧透简介（分页搜索接口携带，导入时省去详情调用） */
  summary: string | null;
  /** 豆瓣书籍页链接 */
  douban_url: string | null;
}

/** search_clasp 返回的一页结果 */
export interface ClaspSearchPage {
  items: ClaspMatch[];
  page: number;
  total_pages: number;
  total: number;
  /** true = claspclub 无精确匹配，正在返回相近结果 */
  fuzzy: boolean;
}

/** analyze_epub 返回的导入预览 */
export interface EpubPreview {
  path: string;
  title: string;
  author: string;
  is_chinese: boolean;
  /** 是否有内嵌封面（未匹配时默认采用 EPUB 封面） */
  has_epub_cover: boolean;
  /** zip 重建副本路径（原 EPUB 含重复条目时存在，导入应使用该路径） */
  effective_path: string | null;
  search_results: number;
  matched: ClaspMatch | null;
}

/** import_epub 返回的导入结果 */
export interface ImportResult {
  id: number;
  title: string;
  author: string;
  library_file: string;
  cover_path: string | null;
  comments: number;
  /** 简介融合所用模型（未融合/降级时为 null） */
  fusion_model: string | null;
  /** 简介融合降级/失败原因（null 表示融合正常） */
  fusion_error: string | null;
}

/** prepare_import 返回（导入确认弹窗数据源） */
export interface PreparedPreview {
  task_id: string;
  title: string;
  author: string;
  tags: string[];
  description: string;
  fusion_model: string | null;
  fusion_error: string | null;
  cover_path: string | null;
  comments: number;
  series_name: string | null;
  series_order: number | null;
  douban_preview: {
    title: string | null;
    author: string | null;
    cover_path: string | null;
    cover_url: string | null;
  } | null;
}

/** llm_status 返回（LLM 可用性预检） */
export interface LlmStatus {
  configured: boolean;
  model: string | null;
}

/** LLM 服务商（OpenAI 兼容，各自独立的 Endpoint / Key / 模型） */
export interface LlmProvider {
  name: string;
  base_url: string;
  api_key: string;
  models: string[];
}

/** 模型价格（元 / 百万 tokens；输入与输出分开计价） */
export interface ModelPricing {
  model: string;
  input_per_m: number;
  output_per_m: number;
}

/** LLM 设置（config.toml llm 段，多 Provider） */
export interface LlmSettings {
  providers: LlmProvider[];
  default_provider: string | null;
  default_model: string | null;
  /** 调用失败重试次数（null = 默认 2；0 = 不重试） */
  retry_count: number | null;
  /** 花费预算（元）：累计成本按价格表换算，达到后拒绝新的 LLM 调用；null = 不限 */
  budget_rmb: number | null;
  /** 模型价格表（元 / 百万 tokens） */
  pricing: ModelPricing[];
}

/** task-progress 事件负载（长任务实时进度） */
export interface TaskProgressEvent {
  phase: string;
  current: number;
  total: number;
  message: string;
}

/** 通用 LLM 对话消息 */
export interface ChatMessage {
  role: string;
  content: string;
  /** 本次回复附带的工具调用轨迹（仅 assistant、书虫 agent 有） */
  steps?: ChatStep[];
}

/** 一次工具调用记录（Agent 轨迹展示） */
export interface ChatStep {
  tool: string;
  /** 实参 JSON 文本 */
  args: string;
  /** 结果摘要（后端已截断） */
  result: string;
}

/** LLM 回复（含所用模型与工具调用轨迹） */
export interface ChatReply {
  content: string;
  model: string;
  steps?: ChatStep[];
}

/** 书虫会话（多轮对话历史；可导出/导入 JSON） */
export interface ChatSession {
  id: string;
  /** 标题（取首条用户消息） */
  title: string;
  savedAt: number;
  messages: ChatMessage[];
}

/** 单个模型的 token 用量统计（含按价格表换算的预估成本） */
export interface LlmUsageRow {
  model: string;
  calls: number;
  prompt_tokens: number;
  completion_tokens: number;
  total_tokens: number;
  last_used: string | null;
  /** 预估成本（元；模型无价格配置时为 null） */
  cost: number | null;
}

/** get_settings 返回 */
export interface SettingsInfo {
  llm: LlmSettings;
  usage: LlmUsageRow[];
  /** 全模型累计 token 总量 */
  usage_total_tokens: number;
  /** 全模型累计预估成本（元；至少一个模型有价格时为已知部分之和，全部未知为 null） */
  usage_total_cost: number | null;
  /** 未定价模型的用量行数（成本/预算不含其用量） */
  usage_unpriced: number;
  /** 生效价格表（显式配置 → 内置预设；编辑器预填展示用） */
  pricing_effective: ModelPricing[];
}
