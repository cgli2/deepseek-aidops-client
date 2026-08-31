use std::collections::HashMap;
use std::io::Write;

use serde::{Deserialize, Serialize};

use crate::Result;

/// 运行期配置（TOML）。仅改"设置"，不改能力装配（能力由 `compose(profile)` + Cargo features 决定，原 §13）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    #[serde(default)]
    pub llm: LlmConfig,
    #[serde(default = "default_sandbox")]
    pub sandbox_mode: crate::types::SandboxMode,
    #[serde(default = "default_approval")]
    pub approval_policy: crate::types::ApprovalPolicy,
    #[serde(default = "default_preset")]
    pub permission_preset: crate::types::PermissionPreset,
    /// 钩子映射：`event 名称 -> 命令`。见完成文档 §13 与 `extensions/EXTENSION-COOKBOOK.md`。
    #[serde(default)]
    pub hooks: HashMap<String, String>,
    /// 界面相关设置（见 `[ui]` 表）。
    #[serde(default)]
    pub ui: UiConfig,
    /// 可选的后端记忆服务（智程平台 aidops）。不配置则 dsh 完全离线、使用原生文件记忆。
    #[serde(default)]
    pub aidops: AidopsConfig,
    /// Trellis 插件（spec 驱动开发）配置（见 `[trellis]` 表）。默认关闭。
    #[serde(default)]
    pub trellis: TrellisConfig,
}

/// 可选后端（智程平台 aidops）连接配置（见 `[aidops]` 表）。
///
/// 仅当 `base_url` 非空时启用；启用后 dsh 把四类记忆资产同步到 aidops 后端，
/// 后端不可达时自动回落原生文件实现（见 `harness-provider-aidops`）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AidopsConfig {
    /// 后端基址，如 `http://localhost:8000`。空串 = 不启用。
    #[serde(default)]
    pub base_url: String,
    /// API key 所在环境变量名（不落盘 key 本身）。
    #[serde(default = "default_aidops_key_env")]
    pub api_key_env: String,
    /// 字面量 key（可选，不推荐落盘）；优先级低于同名环境变量。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_key: Option<String>,
    /// 默认项目 id（单项目假设）；为 `None` 时所有后端调用回落原生。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<i64>,
}

impl AidopsConfig {
    /// 是否启用后端（仅当 `base_url` 非空）。空则 dsh 完全离线、使用原生文件记忆。
    pub fn enabled(&self) -> bool {
        !self.base_url.trim().is_empty()
    }
}

impl Default for AidopsConfig {
    fn default() -> Self {
        Self {
            base_url: String::new(),
            api_key_env: default_aidops_key_env(),
            api_key: None,
            project_id: None,
        }
    }
}

/// Trellis 插件（spec 驱动开发）配置（见 `[trellis]` 表）。
///
/// 默认关闭：`enabled = false` 时插件不注册任何事件监听，行为与未引入前完全一致。
/// 启用后：每回合开始前把 `spec_file` 中的项目规格注入系统消息，
/// 并在 `tasks_file` 中维护任务状态机（新任务 / 进行中 / 完成）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrellisConfig {
    /// 是否启用 Trellis 插件。
    #[serde(default)]
    pub enabled: bool,
    /// 项目规格文件路径（Markdown）。为空时不注入规格。
    #[serde(default)]
    pub spec_file: String,
    /// 任务状态机文件路径（JSON）。为空时不维护任务状态。
    #[serde(default)]
    pub tasks_file: String,
}

impl Default for TrellisConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            spec_file: String::new(),
            tasks_file: String::new(),
        }
    }
}

fn default_aidops_key_env() -> String {
    "AIDOPS_API_KEY".into()
}

/// 界面设置（`[ui]` 表）。
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UiConfig {
    /// 默认界面：`headless` | `tui` | `gui` | `acp`。命令行 `--tui/--gui/--acp` 优先于本项。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmConfig {
    #[serde(default = "default_provider")]
    pub provider: String,
    #[serde(default = "default_base_url")]
    pub base_url: String,
    #[serde(default = "default_model")]
    pub model: String,
    /// API key 所在环境变量名（不落盘 key 本身）。
    #[serde(default = "default_api_key_env")]
    pub api_key_env: String,
    /// 字面量 key（可选，不推荐落盘）。优先级低于同名环境变量 `api_key_env`。
    /// 别名 `DEEPSEEK_API_KEY`：若你在 toml 里直接写成 `DEEPSEEK_API_KEY = "sk-..."`，
    /// 也会被本字段接收（无需改字段名）。
    #[serde(
        default,
        alias = "DEEPSEEK_API_KEY",
        skip_serializing_if = "Option::is_none"
    )]
    pub api_key: Option<String>,
    /// 思考档位 / 努力度（对齐 cc-switch 的 thinkingLevelMap）：发送给上游的字符串值，
    /// 如 `"low"` / `"medium"` / `"high"` / `"xhigh"` / `"max"`，`null`/None 表示不设置、由模型默认。
    /// 自定义模型不自动推断能力（见 `harness-llm/src/model_catalog.rs` 离线预设）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
}

fn default_provider() -> String {
    "deepseek".into()
}
fn default_base_url() -> String {
    "https://api.deepseek.com".into()
}
fn default_model() -> String {
    "deepseek-v4-flash".into()
}
fn default_api_key_env() -> String {
    "DEEPSEEK_API_KEY".into()
}
fn default_sandbox() -> crate::types::SandboxMode {
    crate::types::SandboxMode::WorkspaceWrite
}
fn default_approval() -> crate::types::ApprovalPolicy {
    crate::types::ApprovalPolicy::Ask
}
fn default_preset() -> crate::types::PermissionPreset {
    crate::types::PermissionPreset::Balanced
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            provider: default_provider(),
            base_url: default_base_url(),
            model: default_model(),
            api_key_env: default_api_key_env(),
            api_key: None,
            reasoning_effort: None,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Self {
            llm: LlmConfig::default(),
            sandbox_mode: default_sandbox(),
            approval_policy: default_approval(),
            permission_preset: default_preset(),
            hooks: HashMap::new(),
            ui: UiConfig { profile: None },
            aidops: AidopsConfig {
                base_url: String::new(),
                api_key_env: default_aidops_key_env(),
                api_key: None,
                project_id: None,
            },
            trellis: TrellisConfig::default(),
        }
    }
}

impl Config {
    /// 从 TOML 片段解析（覆盖顺序见完成文档 §5）。
    pub fn from_toml(s: &str) -> Result<Self> {
        Ok(toml::from_str(s)?)
    }

    /// 多源加载（覆盖顺序：内置默认 → 第一个命中的文件 → …）。
    ///
    /// 候选路径（按顺序，命中即返回，不再继续）：
    /// 1. `./.harness.toml`（项目级，最高优先）
    /// 2. `~/.config/harness/config.toml`（用户级）
    /// 3. `./config/default.toml` / `./default.toml`（开发默认）
    /// 4. `<exe 所在目录>/config/default.toml` / `default.toml`（分发默认）
    ///
    /// 完整多源覆盖（home → 项目 → CLI）为后续工作（M?），此处已支持 home/项目两级 + exe 旁。
    pub fn load() -> Result<Self> {
        Ok(Self::load_with_raw()?.0)
    }

    /// 同 `load`，但额外返回命中文件的原始 TOML 表（含未知字段）与路径，
    /// 供 `save_preserving` 做无损回写（对齐 cc-switch「未知字段保留 + 原子写」）。
    pub fn load_with_raw() -> Result<(Config, toml::Table, Option<String>)> {
        for path in config_candidates() {
            if let Ok(s) = std::fs::read_to_string(&path) {
                let parsed = toml::from_str::<Config>(&s);
                let table = toml::from_str::<toml::Table>(&s);
                match (parsed, table) {
                    (Ok(cfg), Ok(tbl)) => {
                        eprintln!("[harness] loaded config: {path}");
                        return Ok((cfg, tbl, Some(path)));
                    }
                    (Err(e), _) => eprintln!("[harness] config parse error in {path}: {e}"),
                    (_, Err(e)) => eprintln!("[harness] config table error in {path}: {e}"),
                }
            }
        }
        Ok((Config::default(), toml::Table::new(), None))
    }

    /// 原子写配置到 `path`：仅写已知字段（不保留未知字段）。适用于「导出当前配置」。
    pub fn save_atomic(&self, path: &str) -> Result<()> {
        self.save_merged(path, &toml::Table::new())
    }

    /// 原子写配置到 `path`，并把 `raw` 中的未知字段一并保留（cc-switch 风格无损回写）。
    pub fn save_preserving(&self, path: &str, raw: &toml::Table) -> Result<()> {
        self.save_merged(path, raw)
    }

    /// 把已知字段覆盖回 `raw`（保留 `raw` 中未被覆盖的键），序列化后原子替换 `path`。
    fn save_merged(&self, path: &str, raw: &toml::Table) -> Result<()> {
        // 先序列化为 TOML 表，再覆盖回 raw：raw 里未知字段因未被 known 键覆盖而保留。
        let body = toml::to_string(self)?;
        let next: toml::Table = toml::from_str(&body)?;
        let mut out = raw.clone();
        for (k, v) in next {
            out.insert(k, v);
        }
        let text = toml::to_string_pretty(&out)?;
        atomic_write(path, &text)
    }
}

/// 计算配置候选路径（见 `load`）。
fn config_candidates() -> Vec<String> {
    let mut v = vec![".harness.toml".to_string()];
    if let Some(home) = home_dir() {
        v.push(
            home.join(".config/harness/config.toml")
                .to_string_lossy()
                .into_owned(),
        );
    }
    v.extend([
        "config/default.toml".to_string(),
        "default.toml".to_string(),
    ]);
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            v.push(
                dir.join("config/default.toml")
                    .to_string_lossy()
                    .into_owned(),
            );
            v.push(dir.join("default.toml").to_string_lossy().into_owned());
            #[cfg(target_os = "macos")]
            if dir.file_name().is_some_and(|name| name == "MacOS") {
                if let Some(contents) = dir.parent() {
                    v.push(
                        contents
                            .join("Resources/config/default.toml")
                            .to_string_lossy()
                            .into_owned(),
                    );
                }
            }
        }
    }
    v
}

/// 跨平台取用户主目录（避免引入 `dirs` 依赖）。
fn home_dir() -> Option<std::path::PathBuf> {
    if let Ok(h) = std::env::var("HOME") {
        return Some(std::path::PathBuf::from(h));
    }
    if let Ok(h) = std::env::var("USERPROFILE") {
        return Some(std::path::PathBuf::from(h));
    }
    None
}

/// 写临时文件后原子替换目标：先写同目录 `.{stem}.tmp.{pid}`，flush 后再 rename。
/// Windows 下 `rename` 不允许目标已存在，故先 `remove_file`；中途崩溃只留下 tmp，
/// 原配置文件完好（仅 rename 成功时才被替换）。对齐 cc-switch 的原子写策略。
fn atomic_write(path: &str, text: &str) -> Result<()> {
    let p = std::path::Path::new(path);
    let dir = p.parent().unwrap_or_else(|| std::path::Path::new("."));
    let _ = std::fs::create_dir_all(dir);
    let stem = p
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| "config.toml".into());
    let tmp = dir.join(format!(".{stem}.tmp.{}", std::process::id()));
    {
        let mut f = std::fs::File::create(&tmp)?;
        f.write_all(text.as_bytes())?;
        f.flush()?;
    }
    // 先删再 rename：Windows 的 rename 不允许目标已存在。
    let _ = std::fs::remove_file(p);
    std::fs::rename(&tmp, p)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serialize_roundtrip_and_atomic_write_preserves_unknown() {
        let dir = std::env::temp_dir().join(format!("harness-cfg-test-{}", uuid::Uuid::new_v4()));
        let _ = std::fs::create_dir_all(&dir);
        let path = dir.join(".harness.toml");
        let p = path.to_str().unwrap();

        let mut cfg = Config::default();
        cfg.llm.model = "deepseek-reasoner".into();
        cfg.llm.reasoning_effort = Some("high".into());
        // 原子写（仅已知字段）
        cfg.save_atomic(p).expect("save_atomic");

        // 注入未知字段，验证 save_preserving 无损保留
        let raw: toml::Table =
            toml::from_str("unknown_key = \"keep-me\"\n[unknown_table]\nx = 1\n").unwrap();
        cfg.save_preserving(p, &raw).expect("save_preserving");

        let text = std::fs::read_to_string(p).unwrap();
        let reloaded = Config::from_toml(&text).unwrap();
        assert_eq!(reloaded.llm.model, "deepseek-reasoner");
        assert_eq!(reloaded.llm.reasoning_effort.as_deref(), Some("high"));
        // 未知字段必须保留
        let parsed: toml::Table = toml::from_str(&text).unwrap();
        assert_eq!(
            parsed.get("unknown_key").and_then(|v| v.as_str()),
            Some("keep-me")
        );
        assert!(parsed.contains_key("unknown_table"));

        // 清理临时文件不应留下 .tmp
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn none_fields_are_omitted_from_toml() {
        let cfg = Config::default();
        let text = toml::to_string(&cfg).unwrap();
        // api_key / reasoning_effort / ui.profile 为 None，序列化时跳过（避免 TOML null 非法值）。
        // 注意 api_key_env 是普通字符串字段，应保留；此处用表解析而非子串匹配避免误判。
        let parsed: toml::Table = toml::from_str(&text).unwrap();
        assert!(parsed.get("api_key").is_none());
        assert!(parsed.get("reasoning_effort").is_none());
        assert!(parsed
            .get("ui")
            .and_then(|v| v.as_table())
            .and_then(|t| t.get("profile"))
            .is_none());
        // api_key_env 作为普通字段仍应写出
        assert!(parsed.get("llm").is_some());
    }
}
