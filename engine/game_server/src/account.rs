use core_protocol::dto::AccountRole;
use rand::{distributions::Alphanumeric, Rng};
use ring::pbkdf2;
use ring::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::num::NonZeroU32;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const DEFAULT_PASSWORD: &str = "123456";
const PASSWORD_ITERATIONS: u32 = 100_000;
const DEVICE_BAN_STRIKE_RESET_SECS: u64 = 30 * 60;

lazy_static::lazy_static! {
    static ref DEVICE_BANS: Mutex<HashMap<String, DeviceBan>> = Mutex::new(HashMap::new());
    static ref DEVICE_BAN_FILE: Mutex<Option<PathBuf>> = Mutex::new(None);
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct DeviceBan {
    strikes: u8,
    until_unix: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct StoredSession {
    account_id: String,
    device_id: String,
    expires_at: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AccountIdentity {
    pub account_id: String,
    pub account_name: String,
    pub role: AccountRole,
    pub device_id: String,
}

pub struct AccountStore {
    root: PathBuf,
    sessions: HashMap<String, StoredSession>,
    devices: HashMap<String, String>,
}

impl AccountStore {
    pub fn open(root: PathBuf) -> Result<Self, String> {
        fs::create_dir_all(root.join("password")).map_err(|error| error.to_string())?;
        secure_directory(&root)?;
        secure_directory(&root.join("password"))?;
        initialize_device_bans(root.join("restrictions.json"))?;
        let sessions = read_json_or_empty(&root.join("sessions.json"))?;
        let devices = read_json_or_empty(&root.join("devices.json"))?;
        Ok(Self {
            root,
            sessions,
            devices,
        })
    }

    pub fn ensure_device(&self, current: Option<&str>) -> Result<(String, bool), String> {
        if let Some(device_id) = current.filter(|value| is_token(value)) {
            Ok((device_id.to_string(), false))
        } else {
            Ok((random_token()?, true))
        }
    }

    pub fn register(
        &mut self,
        account_name: &str,
        password: &str,
        device_id: &str,
    ) -> Result<(String, AccountIdentity), String> {
        validate_device_id(device_id)?;
        validate_account_name(account_name)?;
        validate_password(password)?;

        if self.devices.contains_key(device_id) {
            return Err("此设备已经注册过账户。".into());
        }
        if self.find_account(account_name)?.is_some() {
            return Err("该账户名已被使用。".into());
        }

        let account_id = loop {
            let mut rng = rand::thread_rng();
            let first = rng.gen_range(b'a'..=b'z') as char;
            let second = rng.gen_range(b'0'..=b'9') as char;
            let rest: String = rng
                .sample_iter(&Alphanumeric)
                .take(6)
                .map(char::from)
                .collect();
            let candidate = format!("{first}{second}{rest}");
            if self.account_path_by_id(&candidate)?.is_none() {
                break candidate;
            }
        };
        let path = self.root.join(format!("{account_name}.txt"));
        let contents = format!("{account_name}_{account_id}{{\npower:player;\n}}\n");
        write_private_file(&path, contents.as_bytes())?;
        if let Err(error) = self.write_password(&account_id, password) {
            let _ = fs::remove_file(path);
            return Err(error);
        }

        self.devices
            .insert(device_id.to_string(), account_id.clone());
        if let Err(error) = self.persist_devices() {
            self.devices.remove(device_id);
            let _ = fs::remove_file(self.root.join("password").join(&account_id));
            let _ = fs::remove_file(path);
            return Err(error);
        }
        let identity = AccountIdentity {
            account_id: account_id.clone(),
            account_name: account_name.to_string(),
            role: AccountRole::Player,
            device_id: device_id.to_string(),
        };
        let session = self.create_session(&identity)?;
        Ok((session, identity))
    }

    pub fn login(
        &mut self,
        account_name: &str,
        password: &str,
        device_id: &str,
    ) -> Result<(String, AccountIdentity), String> {
        validate_device_id(device_id)?;
        if account_name.chars().count() > 24 || account_name.len() > 96 || password.len() > 128 {
            return Err("账户名或密码错误。".into());
        }
        let (stored_name, account_id, role) = self
            .find_account(account_name)?
            .ok_or_else(|| "账户名或密码错误。".to_string())?;
        if !self.verify_password(&account_id, password)? {
            return Err("账户名或密码错误。".into());
        }
        let identity = AccountIdentity {
            account_id,
            account_name: stored_name,
            role,
            device_id: device_id.to_string(),
        };
        let session = self.create_session(&identity)?;
        Ok((session, identity))
    }

    pub fn session(&self, token: Option<&str>, device_id: Option<&str>) -> Option<AccountIdentity> {
        let token = token.filter(|value| is_token(value))?;
        let device_id = device_id.filter(|value| is_token(value))?;
        let stored = self.sessions.get(token)?;
        if stored.device_id != device_id || stored.expires_at <= unix_now().ok()? {
            return None;
        }
        let (account_name, account_id, role) = self
            .account_path_by_id(&stored.account_id)
            .ok()
            .flatten()
            .and_then(|path| read_account_file(&path).ok().flatten())?;
        Some(AccountIdentity {
            account_id,
            account_name,
            role,
            device_id: device_id.to_string(),
        })
    }

    pub fn logout(&mut self, token: Option<&str>) -> Result<(), String> {
        if let Some(token) = token.filter(|value| is_token(value)) {
            self.sessions.remove(token);
            self.persist_sessions()?;
        }
        Ok(())
    }

    pub fn change_password(
        &mut self,
        token: &str,
        device_id: &str,
        current_password: &str,
        new_password: &str,
    ) -> Result<String, String> {
        let identity = self
            .session(Some(token), Some(device_id))
            .ok_or_else(|| "登录状态已失效，请重新登录。".to_string())?;
        validate_password(new_password)?;
        if !self.verify_password(&identity.account_id, current_password)? {
            return Err("当前密码错误。".into());
        }
        self.write_password(&identity.account_id, new_password)?;
        self.sessions
            .retain(|_, session| session.account_id != identity.account_id);
        let new_token = self.create_session(&identity)?;
        Ok(new_token)
    }

    pub fn verify_delete_password(
        &self,
        token: &str,
        device_id: &str,
        password: &str,
    ) -> Result<(), String> {
        let identity = self
            .session(Some(token), Some(device_id))
            .ok_or_else(|| "登录状态已失效，请重新登录。".to_string())?;
        if self.verify_password(&identity.account_id, password)? {
            Ok(())
        } else {
            Err("密码错误。".into())
        }
    }

    pub fn delete_account(
        &mut self,
        token: &str,
        device_id: &str,
        password: &str,
    ) -> Result<(), String> {
        let identity = self
            .session(Some(token), Some(device_id))
            .ok_or_else(|| "登录状态已失效，请重新登录。".to_string())?;
        if !self.verify_password(&identity.account_id, password)? {
            return Err("密码错误。".into());
        }
        let account_path = self
            .account_path_by_id(&identity.account_id)?
            .ok_or_else(|| "账户不存在。".to_string())?;
        let password_path = self.root.join("password").join(&identity.account_id);
        fs::remove_file(account_path).map_err(|error| error.to_string())?;
        if password_path.exists() {
            fs::remove_file(password_path).map_err(|error| error.to_string())?;
        }
        self.sessions
            .retain(|_, session| session.account_id != identity.account_id);
        self.devices
            .retain(|_, account_id| account_id != &identity.account_id);
        self.persist_sessions()?;
        self.persist_devices()
    }

    fn create_session(&mut self, identity: &AccountIdentity) -> Result<String, String> {
        let now = unix_now()?;
        self.sessions.retain(|_, session| session.expires_at > now);
        self.sessions.retain(|_, session| {
            session.account_id != identity.account_id || session.device_id != identity.device_id
        });
        let token = random_token()?;
        self.sessions.insert(
            token.clone(),
            StoredSession {
                account_id: identity.account_id.clone(),
                device_id: identity.device_id.clone(),
                expires_at: now.saturating_add(365 * 24 * 60 * 60),
            },
        );
        if let Err(error) = self.persist_sessions() {
            self.sessions.remove(&token);
            return Err(error);
        }
        Ok(token)
    }

    fn find_account(
        &self,
        account_name: &str,
    ) -> Result<Option<(String, String, AccountRole)>, String> {
        let wanted = account_name.to_lowercase();
        let entries = fs::read_dir(&self.root).map_err(|error| error.to_string())?;
        for entry in entries {
            let entry = entry.map_err(|error| error.to_string())?;
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("txt") {
                continue;
            }
            if let Some((name, account_id, role)) =
                read_account_file(&path).map_err(|error| error.to_string())?
            {
                if name.to_lowercase() == wanted {
                    return Ok(Some((name, account_id, role)));
                }
            }
        }
        Ok(None)
    }

    fn account_path_by_id(&self, account_id: &str) -> Result<Option<PathBuf>, String> {
        let entries = fs::read_dir(&self.root).map_err(|error| error.to_string())?;
        for entry in entries {
            let entry = entry.map_err(|error| error.to_string())?;
            let path = entry.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("txt") {
                continue;
            }
            if read_account_file(&path)
                .map_err(|error| error.to_string())?
                .map(|(_, id, _)| id == account_id)
                .unwrap_or(false)
            {
                return Ok(Some(path));
            }
        }
        Ok(None)
    }

    fn verify_password(&self, account_id: &str, password: &str) -> Result<bool, String> {
        let path = self.root.join("password").join(account_id);
        if !path.exists() {
            return Ok(password == DEFAULT_PASSWORD);
        }
        let encoded = fs::read_to_string(path).map_err(|error| error.to_string())?;
        verify_password_hash(&encoded, password)
    }

    fn write_password(&self, account_id: &str, password: &str) -> Result<(), String> {
        write_private_file(
            &self.root.join("password").join(account_id),
            password_hash(password)?.as_bytes(),
        )
    }

    fn persist_sessions(&self) -> Result<(), String> {
        write_json(&self.root.join("sessions.json"), &self.sessions)
    }

    fn persist_devices(&self) -> Result<(), String> {
        write_json(&self.root.join("devices.json"), &self.devices)
    }
}

fn initialize_device_bans(path: PathBuf) -> Result<(), String> {
    let bans = read_json_or_empty(&path)?;
    *DEVICE_BANS.lock().unwrap() = bans;
    *DEVICE_BAN_FILE.lock().unwrap() = Some(path);
    Ok(())
}

pub fn restrict_device(device_id: &str) -> Result<(u32, Duration), String> {
    validate_device_id(device_id)?;
    let now = unix_now()?;
    let mut bans = DEVICE_BANS.lock().unwrap();
    let ban = bans.entry(device_id.to_string()).or_default();
    reset_device_ban_strikes_if_cooled_off(ban, now);
    let minutes = match ban.strikes {
        0 => 3,
        1 => 5,
        2 => 10,
        3 => 30,
        _ => 60,
    };
    ban.strikes = ban.strikes.saturating_add(1);
    ban.until_unix = ban
        .until_unix
        .max(now)
        .saturating_add(u64::from(minutes) * 60)
        .min(now.saturating_add(60 * 60));
    let remaining = Duration::from_secs(ban.until_unix.saturating_sub(now));
    let path = DEVICE_BAN_FILE
        .lock()
        .unwrap()
        .clone()
        .ok_or_else(|| "设备禁言存储尚未初始化。".to_string())?;
    write_json(&path, &*bans)?;
    Ok((minutes, remaining))
}

fn reset_device_ban_strikes_if_cooled_off(ban: &mut DeviceBan, now: u64) {
    if ban.strikes > 0 && now.saturating_sub(ban.until_unix) >= DEVICE_BAN_STRIKE_RESET_SECS {
        ban.strikes = 0;
    }
}

pub fn device_ban_remaining(device_id: &str) -> Option<Duration> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH).ok()?.as_secs();
    let bans = DEVICE_BANS.lock().ok()?;
    let remaining = bans.get(device_id)?.until_unix.saturating_sub(now);
    (remaining > 0).then(|| Duration::from_secs(remaining))
}

fn read_account_file(path: &Path) -> Result<Option<(String, String, AccountRole)>, std::io::Error> {
    let contents = fs::read_to_string(path)?;
    let first_line = match contents.lines().next() {
        Some(line) => line,
        None => return Ok(None),
    };
    let inner = match first_line.strip_suffix('{') {
        Some(inner) => inner,
        None => return Ok(None),
    };
    let (name, account_id) = match inner.rsplit_once('_') {
        Some(parts) => parts,
        None => return Ok(None),
    };
    if !valid_account_name(name)
        || account_id.len() != 8
        || !account_id.chars().all(|c| c.is_ascii_alphanumeric())
    {
        return Ok(None);
    }
    let role = contents
        .lines()
        .find_map(|line| line.trim().strip_prefix("power:"))
        .and_then(|role| role.strip_suffix(';'))
        .map(str::trim)
        .map(|role| match role {
            "admin" => AccountRole::Admin,
            "superadmin" => AccountRole::SuperAdmin,
            _ => AccountRole::Player,
        })
        .unwrap_or_default();
    Ok(Some((name.to_string(), account_id.to_string(), role)))
}

fn validate_account_name(name: &str) -> Result<(), String> {
    if valid_account_name(name) {
        Ok(())
    } else {
        Err("账户名需为 3–24 个字母或数字，可使用下划线和连字符。".into())
    }
}

fn valid_account_name(name: &str) -> bool {
    (3..=24).contains(&name.chars().count())
        && name
            .chars()
            .all(|character| character.is_alphanumeric() || character == '_' || character == '-')
}

fn validate_password(password: &str) -> Result<(), String> {
    if (6..=128).contains(&password.len()) {
        Ok(())
    } else {
        Err("密码长度需为 6–128 个字节。".into())
    }
}

fn validate_device_id(device_id: &str) -> Result<(), String> {
    if is_token(device_id) {
        Ok(())
    } else {
        Err("设备标识无效，请刷新页面后重试。".into())
    }
}

fn is_token(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn random_token() -> Result<String, String> {
    let mut bytes = [0u8; 32];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| "无法生成安全随机标识。".to_string())?;
    Ok(hex_encode(&bytes))
}

fn unix_now() -> Result<u64, String> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|error| error.to_string())
}

fn password_hash(password: &str) -> Result<String, String> {
    let mut salt = [0u8; 16];
    SystemRandom::new()
        .fill(&mut salt)
        .map_err(|_| "无法生成安全随机盐值。".to_string())?;
    let mut hash = [0u8; 32];
    pbkdf2::derive(
        pbkdf2::PBKDF2_HMAC_SHA256,
        NonZeroU32::new(PASSWORD_ITERATIONS).unwrap(),
        &salt,
        password.as_bytes(),
        &mut hash,
    );
    Ok(format!(
        "pbkdf2_sha256${}${}${}",
        PASSWORD_ITERATIONS,
        hex_encode(&salt),
        hex_encode(&hash)
    ))
}

fn verify_password_hash(encoded: &str, password: &str) -> Result<bool, String> {
    let mut parts = encoded.split('$');
    let algorithm = parts.next();
    let iterations = parts.next().and_then(|value| value.parse::<u32>().ok());
    let salt = parts.next().and_then(hex_decode);
    let expected = parts.next().and_then(hex_decode);
    if algorithm != Some("pbkdf2_sha256")
        || parts.next().is_some()
        || iterations
            .map(|value| value == 0 || value > 1_000_000)
            .unwrap_or(true)
        || salt.as_ref().map(Vec::len) != Some(16)
        || expected.as_ref().map(Vec::len) != Some(32)
    {
        return Err("账户密码文件格式无效。".into());
    }
    Ok(pbkdf2::verify(
        pbkdf2::PBKDF2_HMAC_SHA256,
        NonZeroU32::new(iterations.unwrap()).unwrap(),
        salt.as_ref().unwrap(),
        password.as_bytes(),
        expected.as_ref().unwrap(),
    )
    .is_ok())
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut result = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        result.push(HEX[(byte >> 4) as usize] as char);
        result.push(HEX[(byte & 0x0f) as usize] as char);
    }
    result
}

fn hex_decode(value: &str) -> Option<Vec<u8>> {
    if value.len() % 2 != 0 {
        return None;
    }
    value
        .as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let high = (pair[0] as char).to_digit(16)?;
            let low = (pair[1] as char).to_digit(16)?;
            Some((high * 16 + low) as u8)
        })
        .collect()
}

fn read_json_or_empty<T: for<'de> Deserialize<'de> + Default>(path: &Path) -> Result<T, String> {
    if !path.exists() {
        return Ok(T::default());
    }
    let contents = fs::read(path).map_err(|error| error.to_string())?;
    serde_json::from_slice(&contents).map_err(|error| error.to_string())
}

fn write_json<T: Serialize>(path: &Path, value: &T) -> Result<(), String> {
    let temporary = path.with_extension("tmp");
    let contents = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    write_private_file(&temporary, &contents)?;
    fs::rename(temporary, path).map_err(|error| error.to_string())
}

fn write_private_file(path: &Path, contents: &[u8]) -> Result<(), String> {
    let mut options = OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|error| error.to_string())?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        file.set_permissions(fs::Permissions::from_mode(0o600))
            .map_err(|error| error.to_string())?;
    }
    file.write_all(contents).map_err(|error| error.to_string())
}

fn secure_directory(path: &Path) -> Result<(), String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_ban_strikes_reset_after_thirty_minute_cooldown() {
        let mut ban = DeviceBan {
            strikes: 4,
            until_unix: 1_000,
        };

        reset_device_ban_strikes_if_cooled_off(&mut ban, 1_000 + 30 * 60 - 1);
        assert_eq!(ban.strikes, 4);

        reset_device_ban_strikes_if_cooled_off(&mut ban, 1_000 + 30 * 60);
        assert_eq!(ban.strikes, 0);
    }

    #[test]
    fn account_lifecycle_and_role_file() {
        let root = std::env::temp_dir().join(format!(
            "mk48-account-test-{}",
            random_token().expect("random token")
        ));
        let mut store = AccountStore::open(root.clone()).expect("open store");
        let device_id = random_token().expect("device id");
        let (token, identity) = store
            .register("fleet_user", "secret123", &device_id)
            .expect("register account");

        assert_eq!(
            fs::read_to_string(root.join("fleet_user.txt")).expect("account file"),
            format!("fleet_user_{}{{\npower:player;\n}}\n", identity.account_id)
        );
        let password_file = root.join("password").join(&identity.account_id);
        let password_hash = fs::read_to_string(&password_file).expect("password hash");
        assert!(!password_hash.contains("secret123"));
        assert!(store
            .register("second_user", "secret123", &device_id)
            .is_err());
        assert_eq!(
            store
                .session(Some(&token), Some(&device_id))
                .expect("session")
                .role,
            AccountRole::Player
        );

        fs::remove_file(&password_file).expect("remove password hash");
        let (default_token, _) = store
            .login("fleet_user", DEFAULT_PASSWORD, &device_id)
            .expect("default password login");
        fs::write(
            root.join("fleet_user.txt"),
            format!("fleet_user_{}{{\npower:admin;\n}}\n", identity.account_id),
        )
        .expect("edit role");
        assert_eq!(
            store
                .session(Some(&default_token), Some(&device_id))
                .expect("updated session role")
                .role,
            AccountRole::Admin
        );

        let changed_token = store
            .change_password(&default_token, &device_id, DEFAULT_PASSWORD, "new-secret")
            .expect("change password");
        assert!(store
            .session(Some(&changed_token), Some(&device_id))
            .is_some());
        assert!(store
            .login("fleet_user", DEFAULT_PASSWORD, &device_id)
            .is_err());
        let (new_token, _) = store
            .login("fleet_user", "new-secret", &device_id)
            .expect("login with changed password");
        assert!(store
            .verify_delete_password(&new_token, &device_id, "wrong-password")
            .is_err());
        store
            .delete_account(&new_token, &device_id, "new-secret")
            .expect("delete account");
        assert!(!root.join("fleet_user.txt").exists());
        for expected_minutes in [3, 5, 10, 30, 60] {
            let (minutes, remaining) = restrict_device(&device_id).expect("restrict device");
            assert_eq!(minutes, expected_minutes);
            assert!(remaining <= Duration::from_secs(60 * 60));
        }
        assert!(device_ban_remaining(&device_id).is_some());
        let _reloaded_store = AccountStore::open(root.clone()).expect("reload restrictions");
        assert_eq!(
            restrict_device(&device_id).expect("repeat restriction").0,
            60
        );
        drop(store);
        drop(_reloaded_store);
        fs::remove_dir_all(root).expect("remove test directory");
    }
}
