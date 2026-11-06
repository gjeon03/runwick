//! Roles (작업 방식): reusable instructions, preferred tool and optional checklist that
//! delegation can hand to a session. Only Runwick windows change them; anyone may read them.

use crate::host::Ctx;
use crate::router::Router;
use crate::store::{Migration, Store, db_err, ts_from_sql, ts_to_sql};
use runwick_core::api::projects::ById;
use runwick_core::api::roles::*;
use runwick_core::api::sessions::Tool;
use runwick_core::rpc::{RpcError, codes};
use rusqlite::{OptionalExtension, params};
use uuid::Uuid;

pub const MIGRATIONS: &[Migration] = &[(
    "0203_roles",
    "CREATE TABLE roles (
        id BLOB PRIMARY KEY,
        name TEXT NOT NULL,
        instructions TEXT NOT NULL DEFAULT '',
        preferred_tool TEXT,
        -- JSON list of checklist steps.
        procedure TEXT NOT NULL DEFAULT '[]',
        created_at TEXT NOT NULL,
        updated_at TEXT NOT NULL
    );
    CREATE UNIQUE INDEX roles_name ON roles(name COLLATE NOCASE);",
)];

const MAX_NAME: usize = 60;
const MAX_INSTRUCTIONS: usize = 20_000;
const MAX_STEPS: usize = 30;

fn writer_only(ctx: &Ctx) -> Result<(), RpcError> {
    if ctx.client.is_interactive() {
        Ok(())
    } else {
        Err(RpcError::new(codes::DENIED, "역할은 Runwick 창에서만 바꿀 수 있어요"))
    }
}

pub fn register(r: &mut Router) {
    r.add::<RolesList, _, _>(|ctx: Ctx, _: ()| async move { list(&ctx.host.store) });
    r.add::<RolesGet, _, _>(|ctx: Ctx, p: ById| async move { get(&ctx.host.store, p.id) });
    r.add::<RolesSave, _, _>(|ctx: Ctx, p: RoleSave| async move {
        writer_only(&ctx)?;
        let role = save(&ctx.host.store, p)?;
        ctx.host.bus.changed(&[TOPIC]);
        Ok(role)
    });
    r.add::<RolesDuplicate, _, _>(|ctx: Ctx, p: ById| async move {
        writer_only(&ctx)?;
        let role = duplicate(&ctx.host.store, p.id)?;
        ctx.host.bus.changed(&[TOPIC]);
        Ok(role)
    });
    r.add::<RolesDelete, _, _>(|ctx: Ctx, p: ById| async move {
        writer_only(&ctx)?;
        let n = ctx.host.store.with(|c| c.execute("DELETE FROM roles WHERE id = ?1", [p.id])).map_err(db_err)?;
        if n == 0 {
            return Err(RpcError::not_found("역할을 찾을 수 없어요"));
        }
        ctx.host.bus.changed(&[TOPIC]);
        Ok(())
    });
}

fn tool_to_sql(t: Tool) -> &'static str {
    match t {
        Tool::Claude => "claude",
        Tool::Codex => "codex",
        Tool::Shell => "shell",
    }
}

fn tool_from_sql(s: Option<String>) -> Option<Tool> {
    match s.as_deref() {
        Some("claude") => Some(Tool::Claude),
        Some("codex") => Some(Tool::Codex),
        Some("shell") => Some(Tool::Shell),
        _ => None,
    }
}

fn from_row(r: &rusqlite::Row) -> rusqlite::Result<Role> {
    Ok(Role {
        id: r.get("id")?,
        name: r.get("name")?,
        instructions: r.get("instructions")?,
        preferred_tool: tool_from_sql(r.get("preferred_tool")?),
        procedure: serde_json::from_str(&r.get::<_, String>("procedure")?).unwrap_or_default(),
        created_at: ts_from_sql(&r.get::<_, String>("created_at")?),
        updated_at: ts_from_sql(&r.get::<_, String>("updated_at")?),
    })
}

pub fn list(store: &Store) -> Result<Vec<Role>, RpcError> {
    store
        .with(|c| {
            let mut st = c.prepare("SELECT * FROM roles ORDER BY name COLLATE NOCASE")?;
            let rows = st.query_map([], from_row)?;
            rows.collect()
        })
        .map_err(db_err)
}

pub fn get(store: &Store, id: RoleId) -> Result<Role, RpcError> {
    store
        .with(|c| c.query_row("SELECT * FROM roles WHERE id = ?1", [id], from_row).optional())
        .map_err(db_err)?
        .ok_or_else(|| RpcError::not_found("역할을 찾을 수 없어요"))
}

fn name_taken(store: &Store, name: &str, except: Option<RoleId>) -> Result<bool, RpcError> {
    store
        .with(|c| {
            c.query_row(
                "SELECT 1 FROM roles WHERE name = ?1 COLLATE NOCASE AND (?2 IS NULL OR id <> ?2)",
                params![name, except],
                |_| Ok(()),
            )
            .optional()
        })
        .map(|r| r.is_some())
        .map_err(db_err)
}

pub fn save(store: &Store, p: RoleSave) -> Result<Role, RpcError> {
    let name: String = p.name.split_whitespace().collect::<Vec<_>>().join(" ");
    if name.is_empty() {
        return Err(RpcError::invalid("역할 이름을 입력해 주세요"));
    }
    if name.chars().count() > MAX_NAME {
        return Err(RpcError::invalid(format!("역할 이름은 {MAX_NAME}자까지 쓸 수 있어요")));
    }
    if p.instructions.chars().count() > MAX_INSTRUCTIONS {
        return Err(RpcError::invalid("지시문이 너무 길어요 (2만 자까지)"));
    }
    let steps: Vec<String> = p.procedure.iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    if steps.len() > MAX_STEPS {
        return Err(RpcError::invalid(format!("절차는 {MAX_STEPS}단계까지 둘 수 있어요")));
    }
    if name_taken(store, &name, p.id)? {
        return Err(RpcError::new(codes::CONFLICT, format!("‘{name}’ 역할이 이미 있어요")));
    }
    let now = ts_to_sql(&runwick_core::now());
    let procedure = serde_json::to_string(&steps).unwrap_or_else(|_| "[]".into());
    let tool = p.preferred_tool.map(tool_to_sql);
    let instructions = p.instructions.trim_end().to_string();
    let id = match p.id {
        Some(id) => {
            let n = store
                .with(|c| {
                    c.execute(
                        "UPDATE roles SET name = ?2, instructions = ?3, preferred_tool = ?4, procedure = ?5,
                            updated_at = ?6 WHERE id = ?1",
                        params![id, name, instructions, tool, procedure, now],
                    )
                })
                .map_err(db_err)?;
            if n == 0 {
                return Err(RpcError::not_found("역할을 찾을 수 없어요"));
            }
            id
        }
        None => {
            let id = Uuid::now_v7();
            store
                .with(|c| {
                    c.execute(
                        "INSERT INTO roles (id, name, instructions, preferred_tool, procedure, created_at, updated_at)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
                        params![id, name, instructions, tool, procedure, now],
                    )
                })
                .map_err(db_err)?;
            id
        }
    };
    get(store, id)
}

/// A copy named `<name> 사본` (`사본 2`, … when taken).
pub fn duplicate(store: &Store, id: RoleId) -> Result<Role, RpcError> {
    let src = get(store, id)?;
    let base: String = format!("{} 사본", src.name).chars().take(MAX_NAME - 3).collect();
    let mut name = base.clone();
    let mut n = 2;
    while name_taken(store, &name, None)? {
        name = format!("{base} {n}");
        n += 1;
    }
    save(
        store,
        RoleSave {
            id: None,
            name,
            instructions: src.instructions,
            preferred_tool: src.preferred_tool,
            procedure: src.procedure,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn new(store: &Store, name: &str) -> Result<Role, RpcError> {
        save(
            store,
            RoleSave {
                id: None,
                name: name.into(),
                instructions: "회귀를 찾는다\n".into(),
                preferred_tool: Some(Tool::Codex),
                procedure: vec![" 테스트 ".into(), "".into(), "보고".into()],
            },
        )
    }

    #[test]
    fn create_edit_duplicate_delete() {
        let s = Store::open_in_memory().unwrap();
        let r = new(&s, "  회귀   검토 ").unwrap();
        assert_eq!(r.name, "회귀 검토");
        assert_eq!(r.procedure, vec!["테스트".to_string(), "보고".to_string()]);
        assert_eq!(r.preferred_tool, Some(Tool::Codex));
        assert_eq!(new(&s, "회귀 검토").unwrap_err().code, codes::CONFLICT);
        assert_eq!(new(&s, " ").unwrap_err().code, codes::INVALID);
        let edited = save(
            &s,
            RoleSave { id: Some(r.id), name: "회귀 검토".into(), instructions: "x".into(), ..Default::default() },
        )
        .unwrap();
        assert_eq!((edited.instructions.as_str(), edited.preferred_tool), ("x", None));
        let d1 = duplicate(&s, r.id).unwrap();
        let d2 = duplicate(&s, r.id).unwrap();
        assert_eq!((d1.name.as_str(), d2.name.as_str()), ("회귀 검토 사본", "회귀 검토 사본 2"));
        assert_eq!(list(&s).unwrap().len(), 3);
        s.with(|c| c.execute("DELETE FROM roles WHERE id = ?1", [d1.id])).unwrap();
        assert!(get(&s, d1.id).is_err());
    }
}
