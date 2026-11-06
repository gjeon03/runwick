//! General preferences (S13 일반), kept in the host's key/value table so every window of this
//! computer shows the same settings. Also the external editor command used by "외부 에디터로
//! 열기" (`files.editor`), checked against the login shell's `PATH` when it is saved.

use crate::host::Ctx;
use crate::router::Router;
use crate::store::{Store, db_err};
use runwick_core::api::files::EDITOR_SETTING;
use runwick_core::api::prefs::*;
use runwick_core::josa;
use runwick_core::rpc::{RpcError, codes, topic};
use serde_json::Value;
use std::path::Path;

const KEY: &str = "prefs";
/// Longest accepted editor command line.
const MAX_EDITOR: usize = 300;

pub fn register(r: &mut Router) {
    r.add::<PrefsGet, _, _>(|ctx: Ctx, _: ()| async move { load(&ctx.host.store) });
    r.add::<PrefsSet, _, _>(|ctx: Ctx, p: Prefs| async move {
        let p = p.sanitized();
        crate::meta::kv_put(&ctx.host.store, KEY, &runwick_core::rpc::to_value(&p)).map_err(db_err)?;
        ctx.host.bus.changed(&[topic::SETTINGS]);
        Ok(p)
    });
    r.add::<PrefsEditor, _, _>(|ctx: Ctx, _: ()| async move { editor(&ctx.host.store) });
    r.add::<PrefsSetEditor, _, _>(|ctx: Ctx, p: SetEditor| async move {
        // The host runs this command later: only a person in a Runwick window may set it.
        if !ctx.client.is_interactive() {
            return Err(RpcError::new(codes::DENIED, "외부 에디터는 Runwick 창에서만 바꿀 수 있어요"));
        }
        // The login environment may still be being read (up to a few seconds after start).
        let path = tokio::task::spawn_blocking(|| crate::sessions::env::login_env().path().to_string())
            .await
            .map_err(RpcError::internal)?;
        let command = resolve_editor(&p.command, &path).map_err(RpcError::invalid)?;
        crate::meta::kv_put(&ctx.host.store, EDITOR_SETTING, &Value::String(command)).map_err(db_err)?;
        ctx.host.bus.changed(&[topic::SETTINGS]);
        editor(&ctx.host.store)
    });
}

pub fn load(store: &Store) -> Result<Prefs, RpcError> {
    let v = crate::meta::kv_get(store, KEY).map_err(db_err)?;
    Ok(v.and_then(|v| serde_json::from_value::<Prefs>(v).ok()).unwrap_or_default().sanitized())
}

/// The saved editor command and whether it can still start.
pub fn editor(store: &Store) -> Result<EditorSetting, RpcError> {
    let command = crate::meta::kv_get(store, EDITOR_SETTING)
        .map_err(db_err)?
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default();
    let problem =
        command.split_whitespace().next().filter(|program| !is_executable(Path::new(program))).map(|program| {
            format!("{} 더 이상 찾을 수 없어요. 다시 지정해 주세요", josa::eul_reul(&format!("‘{program}’")))
        });
    Ok(EditorSetting { command, problem })
}

/// The command to save for `input`: the program resolved to a full path on `path` (the login
/// shell's `PATH`), then the arguments as typed. Empty input clears the setting. The command
/// is split on whitespace when it runs, so a program path with spaces cannot be used.
pub fn resolve_editor(input: &str, path: &str) -> Result<String, String> {
    let input = input.trim();
    if input.is_empty() {
        return Ok(String::new());
    }
    if input.chars().count() > MAX_EDITOR {
        return Err(format!("명령은 {MAX_EDITOR}자까지 쓸 수 있어요"));
    }
    if input.chars().any(char::is_control) {
        return Err("명령에 줄바꿈이나 제어 문자를 넣을 수 없어요".into());
    }
    let mut words = input.split_whitespace();
    let program = words.next().unwrap_or_default();
    let full = if program.contains('/') {
        let p = Path::new(program);
        if !p.is_absolute() {
            return Err("경로는 /로 시작하는 전체 경로로 적어 주세요".into());
        }
        if !is_executable(p) {
            return Err(format!("{} 실행할 수 있는 파일이 아니에요", josa::eun_neun(&format!("‘{program}’"))));
        }
        p.to_path_buf()
    } else {
        crate::tools::resolve(program, path).ok_or_else(|| {
            format!(
                "{} 로그인 셸의 PATH에서 찾지 못했어요. 설치했는지 확인하거나 전체 경로를 적어 주세요",
                josa::eul_reul(&format!("‘{program}’"))
            )
        })?
    };
    let full = full.to_string_lossy().into_owned();
    if full.chars().any(char::is_whitespace) {
        return Err(
            "경로에 공백이 있어 쓸 수 없어요. 에디터의 명령줄 도구(예: code, zed)를 PATH에 설치해 주세요".into()
        );
    }
    Ok(std::iter::once(full.as_str()).chain(words).collect::<Vec<_>>().join(" "))
}

fn is_executable(p: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.is_absolute() && std::fs::metadata(p).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn defaults_until_saved_then_sanitized() {
        let s = Store::open_in_memory().unwrap();
        assert_eq!(load(&s).unwrap(), Prefs::default());
        let p = Prefs { terminal_font_size: 40.0, reduce_motion: true, ..Default::default() };
        crate::meta::kv_put(&s, KEY, &serde_json::to_value(p).unwrap()).unwrap();
        let got = load(&s).unwrap();
        assert!(got.reduce_motion);
        assert_eq!(got.terminal_font_size, TERMINAL_FONT_MAX);
    }

    fn program(dir: &Path, name: &str, mode: u32) -> String {
        let p = dir.join(name);
        std::fs::write(&p, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(mode)).unwrap();
        p.to_string_lossy().into_owned()
    }

    #[test]
    fn editor_commands_resolve_on_the_login_path() {
        let d = tempfile::tempdir().unwrap();
        let bin = d.path().join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let zed = program(&bin, "zed", 0o755);
        program(&bin, "notes", 0o644);
        let path = format!("/nonexistent:{}", bin.display());
        // A name on PATH becomes its full path; arguments are kept.
        assert_eq!(resolve_editor("  zed   --wait ", &path).unwrap(), format!("{zed} --wait"));
        assert_eq!(resolve_editor(&zed, "").unwrap(), zed);
        assert_eq!(resolve_editor("", &path).unwrap(), "");
        // Not found, not executable, relative, multi-line, or a path the runner would split.
        assert!(resolve_editor("nvim-not-here", &path).unwrap_err().contains("PATH"));
        assert!(resolve_editor("notes", &path).is_err());
        assert!(resolve_editor("bin/zed", &path).unwrap_err().contains("전체 경로"));
        assert!(resolve_editor("zed\n--rm", &path).is_err());
        let spaced = d.path().join("My Apps");
        std::fs::create_dir_all(&spaced).unwrap();
        program(&spaced, "ed", 0o755);
        assert!(resolve_editor("ed", &spaced.to_string_lossy()).unwrap_err().contains("공백"));
    }

    #[test]
    fn a_saved_editor_that_disappeared_is_reported() {
        let s = Store::open_in_memory().unwrap();
        assert_eq!(editor(&s).unwrap(), EditorSetting::default());
        let d = tempfile::tempdir().unwrap();
        let p = program(d.path(), "ed", 0o755);
        crate::meta::kv_put(&s, EDITOR_SETTING, &Value::String(format!("{p} -w"))).unwrap();
        assert_eq!(editor(&s).unwrap().problem, None);
        std::fs::remove_file(&p).unwrap();
        assert!(editor(&s).unwrap().problem.is_some());
    }
}
