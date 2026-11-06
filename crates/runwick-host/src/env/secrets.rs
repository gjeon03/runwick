//! Where secret values of global variables are kept.
//!
//! - macOS: the login Keychain through the `security` tool (service `dev.runwick.env`, account =
//!   variable name). Values travel on stdin as hex (`security -i`, `-X`), never on a command line.
//! - elsewhere: `<RUNWICK_HOME>/secrets/env.json`, readable only by this user (0600).
//!
//! A test data folder (`RUNWICK_HOME` other than the default) uses its own keychain service so it
//! never touches the person's real secrets. Values are never logged.

use runwick_core::api::env::{SecretStoreInfo, SecretStoreKind};
use runwick_core::paths;
use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub trait SecretStore: Send + Sync {
    fn get(&self, key: &str) -> Result<Option<String>, String>;
    fn set(&self, key: &str, value: &str) -> Result<(), String>;
    fn delete(&self, key: &str) -> Result<(), String>;
    fn info(&self) -> SecretStoreInfo;
}

/// The store for this platform and data folder.
pub fn platform_store() -> Box<dyn SecretStore> {
    if cfg!(target_os = "macos") && Path::new(SECURITY).is_file() {
        Box::new(Keychain { service: keychain_service() })
    } else {
        Box::new(FileStore { path: paths::home().join("secrets").join("env.json") })
    }
}

/// The login keychain under its own service (the backup signing key), apart from the variables;
/// `None` where there is no keychain tool.
pub fn keychain(service: &str) -> Option<Box<dyn SecretStore>> {
    (cfg!(target_os = "macos") && Path::new(SECURITY).is_file())
        .then(|| Box::new(Keychain { service: service.to_string() }) as Box<dyn SecretStore>)
}

const SECURITY: &str = "/usr/bin/security";
const SERVICE: &str = "dev.runwick.env";
const TIMEOUT: Duration = Duration::from_secs(10);
/// `security` exit code for "item not found".
const NOT_FOUND: i32 = 44;

fn keychain_service() -> String {
    let default_home = paths::user_home().join(".runwick");
    if paths::home() == default_home {
        SERVICE.to_string()
    } else {
        // Another data folder (tests, a second instance) keeps its own secrets.
        format!("{SERVICE}.{:08x}", fnv32(paths::home().as_os_str().as_encoded_bytes()))
    }
}

fn fnv32(bytes: &[u8]) -> u32 {
    let mut h: u32 = 0x811c9dc5;
    for b in bytes {
        h ^= *b as u32;
        h = h.wrapping_mul(0x01000193);
    }
    h
}

pub struct Keychain {
    service: String,
}

struct Out {
    code: Option<i32>,
    stderr: String,
}

impl Keychain {
    fn run(&self, args: &[&str], stdin: Option<&str>) -> Result<Out, String> {
        let mut child = Command::new(SECURITY)
            .args(args)
            .stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() })
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("키체인 도구를 실행하지 못했어요: {e}"))?;
        if let (Some(input), Some(mut w)) = (stdin, child.stdin.take()) {
            let _ = w.write_all(input.as_bytes());
        }
        let mut err = child.stderr.take();
        let reader = std::thread::spawn(move || {
            let mut s = String::new();
            if let Some(e) = err.as_mut() {
                let _ = e.take(64 * 1024).read_to_string(&mut s);
            }
            s
        });
        let deadline = Instant::now() + TIMEOUT;
        let status = loop {
            match child.try_wait() {
                Ok(Some(s)) => break s,
                Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(15)),
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err("키체인이 응답하지 않아요. 잠금이 풀려 있는지 확인해 주세요".into());
                }
            }
        };
        Ok(Out { code: status.code(), stderr: reader.join().unwrap_or_default() })
    }
}

impl SecretStore for Keychain {
    fn get(&self, key: &str) -> Result<Option<String>, String> {
        let out = self.run(&["find-generic-password", "-s", &self.service, "-a", key, "-g"], None)?;
        match out.code {
            Some(0) => {
                parse_password_line(&out.stderr).map(Some).ok_or_else(|| "키체인 응답을 읽지 못했어요".to_string())
            }
            Some(NOT_FOUND) => Ok(None),
            _ => Err("키체인에서 값을 읽지 못했어요. 접근을 허용했는지 확인해 주세요".into()),
        }
    }

    fn set(&self, key: &str, value: &str) -> Result<(), String> {
        // Interactive mode reads the command from stdin, so the value never appears in `ps`.
        // `-X` takes the value as hex; an empty value has no hex digits, so it goes as `-w ""`.
        let data = if value.is_empty() { "-w \"\"".to_string() } else { format!("-X {}", hex(value.as_bytes())) };
        let line = format!("add-generic-password -U -s {} -a {} {data}\n", self.service, key);
        self.run(&["-i"], Some(&line))?;
        // `security -i` does not report a failed command in its exit code: read it back.
        match self.get(key)? {
            Some(v) if v == value => Ok(()),
            _ => Err("키체인에 저장하지 못했어요".into()),
        }
    }

    fn delete(&self, key: &str) -> Result<(), String> {
        let out = self.run(&["delete-generic-password", "-s", &self.service, "-a", key], None)?;
        match out.code {
            Some(0) | Some(NOT_FOUND) => Ok(()),
            _ => Err("키체인에서 지우지 못했어요".into()),
        }
    }

    fn info(&self) -> SecretStoreInfo {
        SecretStoreInfo { kind: SecretStoreKind::Keychain, location: format!("macOS 키체인 · {}", self.service) }
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

/// The value in `security find-generic-password -g` output: `password: "text"` for printable
/// values, `password: 0xHEX  "escaped"` otherwise, `password: ` when empty.
fn parse_password_line(stderr: &str) -> Option<String> {
    let line = stderr.lines().find_map(|l| l.strip_prefix("password:"))?;
    let rest = line.strip_prefix(' ').unwrap_or(line);
    if rest.trim().is_empty() {
        return Some(String::new());
    }
    if let Some(h) = rest.strip_prefix("0x") {
        let hexpart: String = h.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
        return String::from_utf8(unhex(&hexpart)?).ok();
    }
    let inner = rest.strip_prefix('"')?.strip_suffix('"')?;
    Some(inner.to_string())
}

/// Secrets in a 0600 JSON file (platforms without a keychain tool).
pub struct FileStore {
    pub path: PathBuf,
}

impl FileStore {
    fn load(&self) -> Result<BTreeMap<String, String>, String> {
        match std::fs::read(&self.path) {
            Ok(b) => serde_json::from_slice(&b).map_err(|_| "비밀값 파일을 읽지 못했어요".to_string()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(BTreeMap::new()),
            Err(e) => Err(format!("비밀값 파일을 열지 못했어요: {e}")),
        }
    }

    fn save(&self, map: &BTreeMap<String, String>) -> Result<(), String> {
        use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
        let dir = self.path.parent().ok_or("비밀값 폴더가 없어요")?;
        std::fs::DirBuilder::new().recursive(true).mode(0o700).create(dir).map_err(|e| e.to_string())?;
        let tmp = self.path.with_extension("json.tmp");
        let bytes = serde_json::to_vec(map).map_err(|e| e.to_string())?;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&tmp)
            .map_err(|e| format!("비밀값 파일을 쓰지 못했어요: {e}"))?;
        f.write_all(&bytes).and_then(|_| f.sync_all()).map_err(|e| format!("비밀값 파일을 쓰지 못했어요: {e}"))?;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600)).map_err(|e| e.to_string())?;
        std::fs::rename(&tmp, &self.path).map_err(|e| format!("비밀값 파일을 쓰지 못했어요: {e}"))
    }
}

impl SecretStore for FileStore {
    fn get(&self, key: &str) -> Result<Option<String>, String> {
        Ok(self.load()?.remove(key))
    }

    fn set(&self, key: &str, value: &str) -> Result<(), String> {
        let mut m = self.load()?;
        m.insert(key.to_string(), value.to_string());
        self.save(&m)
    }

    fn delete(&self, key: &str) -> Result<(), String> {
        let mut m = self.load()?;
        if m.remove(key).is_some() {
            self.save(&m)?;
        }
        Ok(())
    }

    fn info(&self) -> SecretStoreInfo {
        SecretStoreInfo { kind: SecretStoreKind::File, location: paths::display_path(&self.path) }
    }
}

/// In-memory store for tests.
#[cfg(test)]
#[derive(Default)]
pub struct FakeStore {
    pub map: std::sync::Mutex<BTreeMap<String, String>>,
    /// Keys whose reads fail (a locked keychain).
    pub failing: std::sync::Mutex<Vec<String>>,
}

#[cfg(test)]
impl SecretStore for FakeStore {
    fn get(&self, key: &str) -> Result<Option<String>, String> {
        if self.failing.lock().unwrap().iter().any(|k| k == key) {
            return Err("잠김".into());
        }
        Ok(self.map.lock().unwrap().get(key).cloned())
    }
    fn set(&self, key: &str, value: &str) -> Result<(), String> {
        self.map.lock().unwrap().insert(key.into(), value.into());
        Ok(())
    }
    fn delete(&self, key: &str) -> Result<(), String> {
        self.map.lock().unwrap().remove(key);
        Ok(())
    }
    fn info(&self) -> SecretStoreInfo {
        SecretStoreInfo { kind: SecretStoreKind::File, location: "test".into() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_both_password_output_forms() {
        assert_eq!(parse_password_line("keychain: x\npassword: \"plain value\"\n").as_deref(), Some("plain value"));
        assert_eq!(parse_password_line("password: \"c\"d\"").as_deref(), Some("c\"d"));
        assert_eq!(parse_password_line("password: 0xC3A9 \n").as_deref(), Some("é"));
        assert_eq!(parse_password_line("password: 0x610A62  \"a\\012b\"").as_deref(), Some("a\nb"));
        assert_eq!(parse_password_line("password: \n").as_deref(), Some(""));
        assert_eq!(parse_password_line("no such line"), None);
        assert_eq!(unhex(&hex("한글 값\n".as_bytes())), Some("한글 값\n".as_bytes().to_vec()));
    }

    #[test]
    fn file_store_is_private_and_round_trips() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let s = FileStore { path: d.path().join("secrets").join("env.json") };
        assert_eq!(s.get("A").unwrap(), None);
        s.set("A", "1").unwrap();
        s.set("B", "two words").unwrap();
        assert_eq!(s.get("B").unwrap().as_deref(), Some("two words"));
        let mode = std::fs::metadata(&s.path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        s.delete("A").unwrap();
        assert_eq!(s.get("A").unwrap(), None);
    }

    /// Removes every item the test created, even when an assertion fails.
    struct Cleanup<'a>(&'a Keychain, &'a [&'a str]);

    impl Drop for Cleanup<'_> {
        fn drop(&mut self) {
            for k in self.1 {
                let _ = self.0.delete(k);
            }
        }
    }

    /// The real login keychain under a throw-away service name. Ignored by default because it
    /// needs an unlocked keychain: `cargo test -p runwick-host keychain -- --ignored`.
    #[test]
    #[ignore]
    fn keychain_round_trips_values_without_putting_them_on_a_command_line() {
        assert!(Path::new(SECURITY).is_file(), "macOS security tool missing");
        let kc = Keychain { service: format!("{SERVICE}.test-{}", std::process::id()) };
        let keys = ["RW_KC_A", "RW_KC_B"];
        let _cleanup = Cleanup(&kc, &keys);
        assert_eq!(kc.get("RW_KC_A").unwrap(), None);
        let tricky = "한글 \"quoted\" $HOME `x` ; rm -rf /\nsecond line";
        kc.set("RW_KC_A", tricky).unwrap();
        kc.set("RW_KC_B", "").unwrap();
        assert_eq!(kc.get("RW_KC_A").unwrap().as_deref(), Some(tricky));
        assert_eq!(kc.get("RW_KC_B").unwrap().as_deref(), Some(""));
        kc.set("RW_KC_A", "v2").unwrap();
        assert_eq!(kc.get("RW_KC_A").unwrap().as_deref(), Some("v2"));
        kc.delete("RW_KC_A").unwrap();
        kc.delete("RW_KC_A").unwrap();
        assert_eq!(kc.get("RW_KC_A").unwrap(), None);
        assert!(kc.info().location.contains(".test-"));
    }
}
