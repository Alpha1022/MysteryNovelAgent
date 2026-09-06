use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// 运行时数据目录覆盖（Android 等无 HOME 环境下 dirs:: 系列全部失效）：
/// 由 GUI 启动时用 Tauri `app_data_dir` 注入，config 与 database 路径随之重定向。
/// 桌面端不设置，维持原布局不变。
static DATA_DIR_OVERRIDE: OnceLock<PathBuf> = OnceLock::new();

/// 注入数据目录覆盖（仅移动端应调用；重复调用以首次为准）
pub fn set_data_dir_override(dir: PathBuf) {
  let _ = DATA_DIR_OVERRIDE.set(dir);
}

/// 读取运行时数据目录覆盖（移动端文件浏览器根目录用）
pub fn data_dir_override() -> Option<&'static PathBuf> {
  DATA_DIR_OVERRIDE.get()
}

/// 运行时临时目录（覆盖生效时使用其 cache 子目录；Android 上 /tmp 不可写）
pub fn temp_dir() -> PathBuf {
  match DATA_DIR_OVERRIDE.get() {
    Some(base) => {
      let dir = base.join("cache");
      let _ = std::fs::create_dir_all(&dir);
      dir
    }
    None => std::env::temp_dir(),
  }
}

/// 在目录内放置 `.nomedia` 标记（移动端防止封面图片被相册扫描收录）
pub fn ensure_nomedia(dir: &Path) {
  if matches!(std::env::consts::OS, "android" | "ios") {
    let _ = std::fs::write(dir.join(".nomedia"), b"");
  }
}

/// 数据目录：优先运行时覆盖，回退系统数据目录
fn base_data_dir() -> Option<PathBuf> {
  DATA_DIR_OVERRIDE.get().cloned().or_else(dirs::data_dir)
}

/// 配置目录：优先运行时覆盖，回退系统配置目录
fn base_config_dir() -> Option<PathBuf> {
  DATA_DIR_OVERRIDE.get().cloned().or_else(dirs::config_dir)
}

/// 单个 LLM 服务商配置（OpenAI 兼容）
#[derive(Serialize, Deserialize, Default, Debug, Clone, PartialEq)]
pub struct LlmProvider {
  /// 显示名（默认选择的键，不可重复）
  pub name: String,
  /// API Endpoint（如 https://api.openai.com/v1）
  #[serde(default)]
  pub base_url: String,
  /// API Key（本地明文存储）
  #[serde(default)]
  pub api_key: String,
  /// 该服务商下的模型列表
  #[serde(default)]
  pub models: Vec<String>,
}

/// LLM 调用失败默认重试次数
pub const DEFAULT_LLM_RETRY: u32 = 2;

/// LLM 设置（GUI 设置页配置，持久化到 config.toml 的 `llm` 段，优先于环境变量）
#[derive(Serialize, Deserialize, Default, Debug, Clone)]
pub struct LlmSettings {
  /// 服务商列表（每个 provider 有独立的 Endpoint / API Key / 模型）
  #[serde(default)]
  pub providers: Vec<LlmProvider>,
  /// 默认服务商名
  #[serde(default)]
  pub default_provider: Option<String>,
  /// 默认模型（须属于默认服务商）
  #[serde(default)]
  pub default_model: Option<String>,
  /// LLM 调用失败重试次数（None = 默认 2 次；0 = 不重试，最大 10）
  #[serde(default)]
  pub retry_count: Option<u32>,

  // ---- 旧版单 provider 平铺字段：仅用于读取旧 config.toml 迁移，保存时清除 ----
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub base_url: Option<String>,
  #[serde(default, skip_serializing_if = "Option::is_none")]
  pub api_key: Option<String>,
  #[serde(default, skip_serializing_if = "Vec::is_empty")]
  pub models: Vec<String>,
}

/// LLM 设置是否已配置任何服务商
impl LlmSettings {
  pub fn has_providers(&self) -> bool {
    self.providers.iter().any(|p| !p.api_key.trim().is_empty())
  }
}

/// 应用配置（持久化到 config.toml）
#[derive(Serialize, Deserialize, Default, Debug)]
pub struct AppConfig {
  /// 本地书库根目录（EPUB 统一存放处）—— 旧版单书库字段，加载时迁移进 libraries
  #[serde(default)]
  pub library_path: Option<PathBuf>,
  /// 封面图片缓存目录（默认 library_path/covers）
  #[serde(default)]
  pub covers_path: Option<PathBuf>,
  #[serde(default)]
  pub database_path: Option<PathBuf>,
  /// LLM 设置（GUI 设置页）
  #[serde(default)]
  pub llm: LlmSettings,
  /// 书库列表（多书库管理）
  #[serde(default)]
  pub libraries: Vec<LibraryConfig>,
  /// 当前书库 ID
  #[serde(default)]
  pub current_library: Option<String>,
}

/// 书库主题色（未设置的项回退内置默认值）
#[derive(Serialize, Deserialize, Default, Debug, Clone, PartialEq)]
pub struct LibraryTheme {
  #[serde(default)]
  pub accent: Option<String>,
  #[serde(default)]
  pub bg: Option<String>,
  #[serde(default)]
  pub panel: Option<String>,
  #[serde(rename = "panel2", default)]
  pub panel_2: Option<String>,
  #[serde(default)]
  pub ink: Option<String>,
  #[serde(default)]
  pub muted: Option<String>,
  #[serde(default)]
  pub read: Option<String>,
  #[serde(default)]
  pub reading: Option<String>,
  #[serde(default)]
  pub wish: Option<String>,
}

/// 单个书库的 WebDav 同步配置（凭据本地明文存储，与 LLM API Key 同级）
#[derive(Serialize, Deserialize, Default, Debug, Clone, PartialEq)]
pub struct WebDavConfig {
  /// 是否启用该书的 WebDav 同步
  #[serde(default)]
  pub enabled: bool,
  /// 服务器地址（如 https://dav.jianguoyun.com/dav/）
  #[serde(default)]
  pub url: String,
  #[serde(default)]
  pub username: String,
  #[serde(default)]
  pub password: String,
  /// 远程目录（默认 mystery-novel-agent；多级目录以 / 分隔，自动逐级创建）
  #[serde(default)]
  pub remote_dir: String,
}

impl WebDavConfig {
  /// 配置是否完整可用（启用且地址非空）
  pub fn is_ready(&self) -> bool {
    self.enabled && !self.url.trim().is_empty()
  }
}

/// 书库名合法性：仅大小写字母 / 数字 / 下划线，非空且 ≤ 64 字符
pub fn is_valid_library_name(s: &str) -> bool {
  !s.is_empty()
    && s.len() <= 64
    && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// 单个书库配置
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct LibraryConfig {
  pub id: String,
  /// 书库名（slug）：仅 [A-Za-z0-9_]，本地唯一；WebDav 远程目录按其命名
  #[serde(default)]
  pub name: String,
  #[serde(default)]
  pub title: String,
  pub path: PathBuf,
  #[serde(default)]
  pub theme: LibraryTheme,
  /// 导入时固定添加的标签（空则回退"推理小说"）
  #[serde(default)]
  pub default_tags: Vec<String>,
  /// WebDav 同步（每书库独立开关与凭据）
  #[serde(default)]
  pub webdav: WebDavConfig,
}

impl AppConfig {
  /// 当前书库（current_library 精确匹配 → 第一个；均无则 None）
  pub fn current_library(&self) -> Option<&LibraryConfig> {
    self
      .libraries
      .iter()
      .find(|l| Some(&l.id) == self.current_library.as_ref())
      .or_else(|| self.libraries.first())
  }

  /// 当前书库 ID（无书库时 None）
  pub fn current_library_id(&self) -> Option<&str> {
    self.current_library().map(|l| l.id.as_str())
  }

  /// 当前书库的固定标签（空则回退"推理小说"不变量）
  pub fn default_tags(&self) -> Vec<String> {
    let tags: Vec<String> = self
      .current_library()
      .map(|l| l.default_tags.iter().map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect())
      .unwrap_or_default();
    if tags.is_empty() {
      vec!["推理小说".to_string()]
    } else {
      tags
    }
  }

  /// 返回配置文件路径
  /// Linux/macOS: ~/.config/mystery-novel-agent/config.toml
  /// Windows:      %APPDATA%\mystery-novel-agent\config.toml
  /// 兜底:          ./config.toml
  fn config_file_path() -> PathBuf {
    base_config_dir()
      .map(|d| d.join("mystery-novel-agent").join("config.toml"))
      .unwrap_or_else(|| PathBuf::from("config.toml"))
  }

  /// 从磁盘加载配置；文件不存在时返回默认值
  /// 旧版 llm 段（单 provider 平铺字段）自动迁移为 providers 列表；
  /// 旧版单书库 library_path 自动迁移为 libraries[0]
  pub fn load() -> Self {
    let path = Self::config_file_path();
    let mut cfg: Self = match std::fs::read_to_string(&path) {
      Ok(content) => toml::from_str(&content).unwrap_or_default(),
      Err(_) => Self::default(),
    };
    cfg.migrate_llm();
    cfg.migrate_libraries();
    cfg.migrate_library_names();
    cfg
  }

  /// 旧版单书库迁移：library_path → libraries[0]（id "default"，默认标签"推理小说"）
  fn migrate_libraries(&mut self) {
    if self.libraries.is_empty() {
      if let Some(p) = self.library_path.clone() {
        self.libraries.push(LibraryConfig {
          id: "default".to_string(),
          name: String::new(),
          title: "默认书库".to_string(),
          path: p,
          theme: LibraryTheme::default(),
          default_tags: vec!["推理小说".to_string()],
          webdav: WebDavConfig::default(),
        });
        self.current_library = Some("default".to_string());
      }
    }
    // 确保当前书库有效
    if self.current_library.is_none() {
      self.current_library = self.libraries.first().map(|l| l.id.clone());
    }
  }

  /// 书库名迁移：旧配置缺书库名（或非法/重复）时自动补齐唯一合法名。
  /// 候选名由 id 派生（非法字符替换为 _），冲突追加 _2/_3…（幂等，id 稳定）。
  fn migrate_library_names(&mut self) {
    let mut used: Vec<String> = Vec::new();
    for lib in &mut self.libraries {
      let name = lib.name.trim().to_string();
      let candidate = if is_valid_library_name(&name) {
        name
      } else {
        let derived: String = lib
          .id
          .chars()
          .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' })
          .collect();
        let derived = derived.trim_matches('_').to_string();
        if derived.is_empty() { "lib".to_string() } else { derived }
      };
      // 大小写不敏感去重
      let mut final_name = candidate.clone();
      let mut n = 2;
      while used.iter().any(|u| u.eq_ignore_ascii_case(&final_name)) {
        final_name = format!("{candidate}_{n}");
        n += 1;
      }
      used.push(final_name.clone());
      lib.name = final_name;
    }
  }

  /// 书库名是否与既有书库重复（大小写不敏感；exclude_id 用于更新场景）
  pub fn library_name_taken(&self, name: &str, exclude_id: Option<&str>) -> bool {
    self
      .libraries
      .iter()
      .filter(|l| Some(l.id.as_str()) != exclude_id)
      .any(|l| l.name.eq_ignore_ascii_case(name))
  }

  /// 旧版 llm 段迁移：base_url/api_key/models 平铺字段 → providers[0]
  fn migrate_llm(&mut self) {
    if self.llm.providers.is_empty() {
      if let Some(key) = self.llm.api_key.clone().filter(|k| !k.trim().is_empty()) {
        self.llm.providers.push(LlmProvider {
          name: "默认".to_string(),
          base_url: self.llm.base_url.clone().unwrap_or_default(),
          api_key: key,
          models: std::mem::take(&mut self.llm.models),
        });
        self.llm.default_provider = Some("默认".to_string());
      }
    }
    // 旧字段迁移后清除，下次保存即为新格式
    self.llm.base_url = None;
    self.llm.api_key = None;
    self.llm.models = Vec::new();
  }

  /// 将配置写入磁盘
  pub fn save(&self) -> anyhow::Result<()> {
    let path = Self::config_file_path();
    if let Some(parent) = path.parent() {
      std::fs::create_dir_all(parent)?;
    }
    let content = toml::to_string_pretty(self)?;
    std::fs::write(&path, content)?;
    println!("配置已保存到 {}", path.display());
    Ok(())
  }

  /// 获取书库路径（当前书库根目录，不存在则自动创建目录），未配置则报错
  pub fn require_library_path(&self) -> anyhow::Result<PathBuf> {
    let p = match self.current_library() {
      Some(l) => l.path.clone(),
      None => self
        .library_path
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("未配置书库路径，请先运行: library config set <PATH>"))?
        .clone(),
    };
    let p = dunce::canonicalize(&p).unwrap_or_else(|_| p.clone());
    if !p.exists() {
      std::fs::create_dir_all(&p)?;
    }
    Ok(p)
  }

  /// 获取封面缓存目录，默认 `{library_path}/covers`
  pub fn covers_dir(&self) -> anyhow::Result<PathBuf> {
    match &self.covers_path {
      Some(p) => Ok(p.clone()),
      None => {
        let lib = self.require_library_path()?;
        let dir = lib.join("covers");
        std::fs::create_dir_all(&dir)?;
        Ok(dir)
      }
    }
  }

  /// 数据库文件绝对路径：优先使用配置值；
  /// 未配置时回退到平台数据目录（CLI 与 GUI 共用，避免相对 CWD 不一致）
  pub fn database_file(&self) -> PathBuf {
    if let Some(p) = &self.database_path {
      return p.clone();
    }
    match base_data_dir() {
      Some(d) => {
        let dir = d.join("mystery-novel-agent");
        let _ = std::fs::create_dir_all(&dir);
        dir.join("mystery_novel.db")
      }
      None => PathBuf::from("mystery_novel.db"),
    }
  }
}
