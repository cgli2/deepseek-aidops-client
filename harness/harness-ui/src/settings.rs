use rusqlite::{Connection, params};
use std::path::PathBuf;
use std::sync::Mutex;

pub struct SettingsDb {
    conn: Mutex<Connection>,
    path: PathBuf,
}

#[derive(Clone, Debug)]
pub struct ModelProfile {
    pub name: String,
    pub provider: String,
    pub base_url: String,
    pub model: String,
    pub api_key: String,
    /// 是否启用：停用的条目不出现在快捷切换菜单，仅作为配置保留。
    pub enabled: bool,
}

/// 侧栏项目列表行（projects 表投影）。
#[derive(Clone, Debug)]
pub struct ProjectRow {
    pub name: String,
    pub path: String,
    pub archived: bool,
}

/// 插件列表行（plugins 表投影）；`path` 仅导入的 WASM 插件非空。
#[derive(Clone, Debug)]
pub struct PluginRow {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub path: Option<String>,
}

impl SettingsDb {
    pub fn open_default() -> Result<Self, String> {
        // 便携优先：数据目录默认放 exe 旁边（程序拷走数据跟着走）；
        // exe 目录不可写时回退 %LOCALAPPDATA%（Windows）/ 当前目录。
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(PathBuf::from));
        let local_dir = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);

        #[cfg(target_os = "macos")]
        let bases = app_data_dir().into_iter().collect::<Vec<_>>();
        #[cfg(not(target_os = "macos"))]
        let bases = exe_dir
            .clone()
            .into_iter()
            .chain(local_dir.clone())
            .collect::<Vec<_>>();

        // 旧数据一次性迁移：exe 旁无 db 而 %LOCALAPPDATA% 有时，整体拷贝（db + 密钥文件）。
        if let (Some(e), Some(l)) = (&exe_dir, &local_dir) {
            let de = e.join("DeepSeekAIOps");
            let dl = l.join("DeepSeekAIOps");
            if !de.join("settings.db").exists() && dl.join("settings.db").exists() {
                if std::fs::create_dir_all(&de).is_ok() {
                    let _ = std::fs::copy(dl.join("settings.db"), de.join("settings.db"));
                    let k = dl.join("settings.key");
                    if k.exists() {
                        let _ = std::fs::copy(k, de.join("settings.key"));
                    }
                    trace_migrated(&de);
                }
            }
        }

        let mut dir: Option<PathBuf> = None;
        for base in bases {
            #[cfg(target_os = "macos")]
            let d = base;
            #[cfg(not(target_os = "macos"))]
            let d = base.join("DeepSeekAIOps");
            if std::fs::create_dir_all(&d).is_ok() {
                dir = Some(d);
                break;
            }
        }
        let dir = dir.unwrap_or_else(|| PathBuf::from("."));
        Self::open(dir.join("settings.db"))
    }

    pub fn open(path: PathBuf) -> Result<Self, String> {
        let conn = Connection::open(&path).map_err(|e| format!("打开 SQLite 失败: {e}"))?;
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;
          CREATE TABLE IF NOT EXISTS settings(key TEXT PRIMARY KEY,value BLOB NOT NULL,is_secret INTEGER NOT NULL DEFAULT 0,updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
          CREATE TABLE IF NOT EXISTS projects(id INTEGER PRIMARY KEY AUTOINCREMENT,name TEXT NOT NULL,path TEXT NOT NULL UNIQUE,last_opened_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
          CREATE TABLE IF NOT EXISTS conversations(id TEXT PRIMARY KEY,project_path TEXT,title TEXT NOT NULL,created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);
          CREATE TABLE IF NOT EXISTS plugins(id TEXT PRIMARY KEY,name TEXT NOT NULL,enabled INTEGER NOT NULL DEFAULT 0,config_json TEXT NOT NULL DEFAULT '{}');")
            .map_err(|e| format!("初始化 SQLite 失败: {e}"))?;
        conn.execute_batch("CREATE TABLE IF NOT EXISTS model_profiles(name TEXT PRIMARY KEY,provider TEXT NOT NULL,base_url TEXT NOT NULL,model TEXT NOT NULL,api_key BLOB NOT NULL,updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP);")
            .map_err(|e| format!("初始化模型配置失败: {e}"))?;
        // 旧库迁移：projects 表补 archived 列（已存在则忽略错误）。
        let _ = conn
            .execute_batch("ALTER TABLE projects ADD COLUMN archived INTEGER NOT NULL DEFAULT 0;");
        // 旧库迁移：model_profiles 表补 enabled 列（默认启用；已存在则忽略错误）。
        let _ = conn.execute_batch(
            "ALTER TABLE model_profiles ADD COLUMN enabled INTEGER NOT NULL DEFAULT 1;",
        );
        Ok(Self {
            conn: Mutex::new(conn),
            path,
        })
    }
    pub fn path(&self) -> &std::path::Path {
        &self.path
    }
    pub fn get(&self, key: &str) -> Option<String> {
        let conn = self.conn.lock().ok()?;
        let (value, secret): (Vec<u8>, bool) = conn
            .query_row(
                "SELECT value,is_secret FROM settings WHERE key=?1",
                [key],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok()?;
        String::from_utf8(if secret {
            self.unprotect(&value).ok()?
        } else {
            value
        })
        .ok()
    }
    pub fn set(&self, key: &str, value: &str) -> Result<(), String> {
        self.set_bytes(key, value.as_bytes(), false)
    }
    pub fn set_secret(&self, key: &str, value: &str) -> Result<(), String> {
        self.set_bytes(key, &self.protect(value.as_bytes())?, true)
    }
    fn set_bytes(&self, key: &str, value: &[u8], secret: bool) -> Result<(), String> {
        self.conn.lock().map_err(|_|"SQLite 锁异常".to_string())?.execute(
          "INSERT INTO settings(key,value,is_secret,updated_at) VALUES(?1,?2,?3,CURRENT_TIMESTAMP) ON CONFLICT(key) DO UPDATE SET value=excluded.value,is_secret=excluded.is_secret,updated_at=CURRENT_TIMESTAMP",
          params![key,value,secret]).map_err(|e|format!("保存设置失败: {e}"))?;
        Ok(())
    }
    pub fn add_project(&self, path: &std::path::Path) -> Result<(), String> {
        let name = path.file_name().and_then(|v| v.to_str()).unwrap_or("项目");
        self.conn.lock().map_err(|_|"SQLite 锁异常".to_string())?.execute(
          "INSERT INTO projects(name,path,last_opened_at) VALUES(?1,?2,CURRENT_TIMESTAMP) ON CONFLICT(path) DO UPDATE SET name=excluded.name,last_opened_at=CURRENT_TIMESTAMP",
          params![name,path.to_string_lossy()]).map_err(|e|format!("保存项目失败: {e}"))?;
        Ok(())
    }
    /// 侧栏项目列表（按最近打开倒序）。
    pub fn projects(&self) -> Vec<ProjectRow> {
        let Ok(conn) = self.conn.lock() else {
            return vec![];
        };
        let Ok(mut stmt) =
            conn.prepare("SELECT name,path,archived FROM projects ORDER BY last_opened_at DESC")
        else {
            return vec![];
        };
        let Ok(rows) = stmt.query_map([], |row| {
            Ok(ProjectRow {
                name: row.get(0)?,
                path: row.get(1)?,
                archived: row.get::<_, i64>(2)? != 0,
            })
        }) else {
            return vec![];
        };
        rows.filter_map(|r| r.ok()).collect()
    }
    /// 归档 / 取消归档项目（归档后侧栏隐藏，记录仍保留）。
    pub fn archive_project(&self, path: &str, archived: bool) -> Result<(), String> {
        self.conn
            .lock()
            .map_err(|_| "SQLite 锁异常".to_string())?
            .execute(
                "UPDATE projects SET archived=?1 WHERE path=?2",
                params![archived as i64, path],
            )
            .map_err(|e| format!("归档项目失败: {e}"))?;
        Ok(())
    }
    /// 读取全部已登记插件（内置 + 导入的 WASM）。
    pub fn plugins(&self) -> Vec<PluginRow> {
        let Ok(conn) = self.conn.lock() else {
            return vec![];
        };
        let Ok(mut stmt) =
            conn.prepare("SELECT id,name,enabled,config_json FROM plugins ORDER BY id")
        else {
            return vec![];
        };
        let Ok(rows) = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)? != 0,
                row.get::<_, String>(3)?,
            ))
        }) else {
            return vec![];
        };
        rows.filter_map(|r| r.ok())
            .map(|(id, name, enabled, cfg)| PluginRow {
                id,
                name,
                enabled,
                // WASM 插件的 config_json 直接存产物路径；内置插件为 '{}'。
                path: (!cfg.is_empty() && !cfg.starts_with('{')).then_some(cfg),
            })
            .collect()
    }
    /// 单项插件启用状态读写（不存在则插入）。
    pub fn set_plugin_enabled(&self, id: &str, name: &str, enabled: bool) -> Result<(), String> {
        self.conn.lock().map_err(|_| "SQLite 锁异常".to_string())?.execute(
            "INSERT INTO plugins(id,name,enabled) VALUES(?1,?2,?3) ON CONFLICT(id) DO UPDATE SET name=excluded.name,enabled=excluded.enabled",
            params![id, name, enabled],
        ).map_err(|e| format!("保存插件状态失败: {e}"))?;
        Ok(())
    }
    /// 登记导入的 WASM 插件（默认启用，产物路径记入 config_json）。
    pub fn add_wasm_plugin(&self, id: &str, name: &str, path: &str) -> Result<(), String> {
        self.conn.lock().map_err(|_| "SQLite 锁异常".to_string())?.execute(
            "INSERT INTO plugins(id,name,enabled,config_json) VALUES(?1,?2,1,?3) ON CONFLICT(id) DO UPDATE SET name=excluded.name,enabled=1,config_json=excluded.config_json",
            params![id, name, path],
        ).map_err(|e| format!("添加插件失败: {e}"))?;
        Ok(())
    }
    /// 移除插件登记（仅用于用户导入的 WASM 插件）。
    pub fn remove_plugin(&self, id: &str) -> Result<(), String> {
        self.conn
            .lock()
            .map_err(|_| "SQLite 锁异常".to_string())?
            .execute("DELETE FROM plugins WHERE id=?1", [id])
            .map_err(|e| format!("移除插件失败: {e}"))?;
        Ok(())
    }

    pub fn save_model_profile(&self, profile: &ModelProfile) -> Result<(), String> {
        let encrypted = self.protect(profile.api_key.as_bytes())?;
        self.conn.lock().map_err(|_| "SQLite 锁异常".to_string())?.execute(
            "INSERT INTO model_profiles(name,provider,base_url,model,api_key,enabled,updated_at) VALUES(?1,?2,?3,?4,?5,?6,CURRENT_TIMESTAMP) ON CONFLICT(name) DO UPDATE SET provider=excluded.provider,base_url=excluded.base_url,model=excluded.model,api_key=excluded.api_key,enabled=excluded.enabled,updated_at=CURRENT_TIMESTAMP",
            params![profile.name, profile.provider, profile.base_url, profile.model, encrypted, profile.enabled],
        ).map_err(|e| format!("保存模型配置失败: {e}"))?;
        Ok(())
    }
    pub fn model_profiles(&self) -> Vec<ModelProfile> {
        let Ok(conn) = self.conn.lock() else {
            return vec![];
        };
        let Ok(mut stmt) = conn.prepare("SELECT name,provider,base_url,model,api_key,enabled FROM model_profiles ORDER BY updated_at DESC") else { return vec![] };
        let Ok(rows) = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Vec<u8>>(4)?,
                row.get::<_, i64>(5)? != 0,
            ))
        }) else {
            return vec![];
        };
        rows.filter_map(|r| r.ok())
            .filter_map(|(name, provider, base_url, model, key, enabled)| {
                Some(ModelProfile {
                    name,
                    provider,
                    base_url,
                    model,
                    api_key: String::from_utf8(self.unprotect(&key).ok()?).ok()?,
                    enabled,
                })
            })
            .collect()
    }
    /// 切换单个模型配置的启用 / 停用状态（停用条目不参与快捷切换）。
    pub fn set_model_profile_enabled(&self, name: &str, enabled: bool) -> Result<(), String> {
        self.conn
            .lock()
            .map_err(|_| "SQLite 锁异常".to_string())?
            .execute(
                "UPDATE model_profiles SET enabled=?1,updated_at=CURRENT_TIMESTAMP WHERE name=?2",
                params![enabled as i64, name],
            )
            .map_err(|e| format!("更新模型配置状态失败: {e}"))?;
        Ok(())
    }
    pub fn delete_model_profile(&self, name: &str) -> Result<(), String> {
        self.conn
            .lock()
            .map_err(|_| "SQLite 锁异常".to_string())?
            .execute("DELETE FROM model_profiles WHERE name=?1", [name])
            .map_err(|e| format!("删除模型配置失败: {e}"))?;
        Ok(())
    }

    // ── 密钥加密：OS 无关的 AES-256-GCM ────────────────────────────
    // 密钥为首次使用时本地生成的 32 字节随机数，存于数据库旁的 `settings.key`
    // （Unix 下收紧为 0600）。不再依赖 Windows DPAPI，Windows/Linux/macOS 行为一致。
    fn key_path(&self) -> PathBuf {
        self.path.with_extension("key")
    }
    fn crypto_key(&self) -> Result<[u8; 32], String> {
        let kp = self.key_path();
        if let Ok(bytes) = std::fs::read(&kp) {
            if bytes.len() == 32 {
                let mut k = [0u8; 32];
                k.copy_from_slice(&bytes);
                return Ok(k);
            }
        }
        let mut k = [0u8; 32];
        rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut k);
        std::fs::write(&kp, k).map_err(|e| format!("写入密钥文件失败: {e}"))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&kp, std::fs::Permissions::from_mode(0o600));
        }
        Ok(k)
    }
    /// 加密：nonce(12B) || ciphertext+tag，随机 nonce 保证同明文不同密文。
    fn protect(&self, input: &[u8]) -> Result<Vec<u8>, String> {
        use aes_gcm::aead::{Aead, KeyInit};
        let key = self.crypto_key()?;
        let cipher =
            aes_gcm::Aes256Gcm::new_from_slice(&key).map_err(|e| format!("加密初始化失败: {e}"))?;
        let mut nonce = [0u8; 12];
        rand::RngCore::fill_bytes(&mut rand::thread_rng(), &mut nonce);
        let ct = cipher
            .encrypt(aes_gcm::Nonce::from_slice(&nonce), input)
            .map_err(|e| format!("加密失败: {e}"))?;
        let mut out = Vec::with_capacity(12 + ct.len());
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&ct);
        Ok(out)
    }
    /// 解密：先试 AES-GCM；失败时回退旧版 DPAPI 密文（仅 Windows，
    /// 解密成功后下次保存自动以新格式重写）。
    fn unprotect(&self, input: &[u8]) -> Result<Vec<u8>, String> {
        use aes_gcm::aead::{Aead, KeyInit};
        if input.len() >= 12 {
            if let Ok(key) = self.crypto_key() {
                if let Ok(cipher) = aes_gcm::Aes256Gcm::new_from_slice(&key) {
                    if let Ok(pt) =
                        cipher.decrypt(aes_gcm::Nonce::from_slice(&input[..12]), &input[12..])
                    {
                        return Ok(pt);
                    }
                }
            }
        }
        legacy_unprotect(input)
    }
}

pub(crate) fn app_data_dir() -> Option<PathBuf> {
    #[cfg(target_os = "macos")]
    {
        return std::env::var_os("HOME")
            .map(PathBuf::from)
            .map(|home| home.join("Library/Application Support/com.clotee.aidops"));
    }
    #[cfg(not(target_os = "macos"))]
    {
        std::env::current_exe()
            .ok()
            .and_then(|path| path.parent().map(PathBuf::from))
    }
}

/// 旧版 Windows DPAPI 解密（仅用于兼容升级前的密文）。
#[cfg(windows)]
fn legacy_unprotect(input: &[u8]) -> Result<Vec<u8>, String> {
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Cryptography::{
        CRYPT_INTEGER_BLOB, CRYPTPROTECT_UI_FORBIDDEN, CryptUnprotectData,
    };
    let mut src = CRYPT_INTEGER_BLOB {
        cbData: input.len() as u32,
        pbData: input.as_ptr() as *mut u8,
    };
    let mut out = CRYPT_INTEGER_BLOB {
        cbData: 0,
        pbData: std::ptr::null_mut(),
    };
    if unsafe {
        CryptUnprotectData(
            &mut src,
            std::ptr::null_mut(),
            std::ptr::null(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            CRYPTPROTECT_UI_FORBIDDEN,
            &mut out,
        )
    } == 0
    {
        return Err("解密失败".into());
    }
    let result = unsafe { std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec() };
    unsafe { LocalFree(out.pbData as *mut std::ffi::c_void) };
    Ok(result)
}
#[cfg(not(windows))]
fn legacy_unprotect(_input: &[u8]) -> Result<Vec<u8>, String> {
    Err("解密失败".into())
}

/// 迁移日志追加一行（与 GUI trace 同文件，无控制台环境也能排查）。
fn trace_migrated(dir: &std::path::Path) {
    if let Some(d) = app_data_dir() {
        let _ = std::fs::create_dir_all(&d);
        let log = d.join("harness_gui_trace.log");
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(log)
        {
            use std::io::Write;
            let _ = writeln!(f, "[settings] migrated data dir -> {}", dir.display());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn roundtrip() {
        let p = std::env::temp_dir().join(format!("harness-settings-{}.db", std::process::id()));
        let db = SettingsDb::open(p.clone()).unwrap();
        db.set("model", "deepseek-chat").unwrap();
        db.set_secret("key", "secret").unwrap();
        assert_eq!(db.get("model").as_deref(), Some("deepseek-chat"));
        assert_eq!(db.get("key").as_deref(), Some("secret"));
        drop(db);
        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file(p.with_extension("key"));
    }
    #[test]
    fn model_profile_enabled_roundtrip() {
        let p = std::env::temp_dir().join(format!("harness-settings-mp-{}.db", std::process::id()));
        let db = SettingsDb::open(p.clone()).unwrap();
        db.save_model_profile(&ModelProfile {
            name: "deepseek · deepseek-chat".into(),
            provider: "deepseek".into(),
            base_url: "https://api.deepseek.com/v1".into(),
            model: "deepseek-chat".into(),
            api_key: "sk-test".into(),
            enabled: true,
        })
        .unwrap();
        assert!(db.model_profiles()[0].enabled);
        // 停用后状态需持久化；重新保存不应丢失其余字段。
        db.set_model_profile_enabled("deepseek · deepseek-chat", false)
            .unwrap();
        let row = &db.model_profiles()[0];
        assert!(!row.enabled);
        assert_eq!(row.api_key, "sk-test");
        db.delete_model_profile("deepseek · deepseek-chat").unwrap();
        assert!(db.model_profiles().is_empty());
        drop(db);
        let _ = std::fs::remove_file(&p);
        let _ = std::fs::remove_file(p.with_extension("key"));
    }
}
