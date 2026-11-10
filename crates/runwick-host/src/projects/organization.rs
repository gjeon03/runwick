//! Manual ordering and one-level groups. Every change is atomic and window-only.

use super::*;
use rusqlite::Connection;

pub const LOCAL_PROJECT: &str =
    "EXISTS (SELECT 1 FROM workspaces w WHERE w.project_id = p.id AND w.remote_device_id IS NULL)";
pub const PROJECT_SELECT: &str = "SELECT p.*, NOT EXISTS (
    SELECT 1 FROM workspaces w WHERE w.project_id = p.id AND w.remote_device_id IS NULL
) AS remote_only FROM projects p";

pub(super) fn register(r: &mut Router) {
    r.add::<ProjectsMove, _, _>(|ctx: Ctx, p: ProjectMove| async move {
        move_project(&ctx.host.store, &p)?;
        ctx.host.bus.changed(&[topic::PROJECTS]);
        Ok(())
    });
    r.add::<ProjectGroupsList, _, _>(|ctx: Ctx, _: ()| async move { list_groups(&ctx.host.store).map_err(db_err) });
    r.add::<ProjectGroupsCreate, _, _>(|ctx: Ctx, p: ProjectGroupCreate| async move {
        let group = create_group(&ctx.host.store, &p)?;
        ctx.host.bus.changed(&[topic::PROJECTS]);
        Ok(group)
    });
    r.add::<ProjectGroupsUpdate, _, _>(|ctx: Ctx, p: ProjectGroupUpdate| async move {
        let group = update_group(&ctx.host.store, &p)?;
        ctx.host.bus.changed(&[topic::PROJECTS]);
        Ok(group)
    });
    r.add::<ProjectGroupsDelete, _, _>(|ctx: Ctx, p: ById| async move {
        delete_group(&ctx.host.store, p.id)?;
        ctx.host.bus.changed(&[topic::PROJECTS]);
        Ok(())
    });
    r.add::<ProjectGroupsMove, _, _>(|ctx: Ctx, p: ProjectGroupMove| async move {
        move_group(&ctx.host.store, &p)?;
        ctx.host.bus.changed(&[topic::PROJECTS]);
        Ok(())
    });
}

fn atomic<T>(store: &Store, f: impl FnOnce(&Connection) -> Result<T, RpcError>) -> Result<T, RpcError> {
    store
        .with(|c| {
            let tx = c.unchecked_transaction()?;
            match f(&tx) {
                Ok(out) => {
                    tx.commit()?;
                    Ok(Ok(out))
                }
                Err(e) => Ok(Err(e)),
            }
        })
        .map_err(db_err)?
}

fn group_from_row(r: &Row) -> rusqlite::Result<ProjectGroup> {
    Ok(ProjectGroup {
        id: r.get("id")?,
        name: r.get("name")?,
        order_index: r.get("order_index")?,
        collapsed: r.get("collapsed")?,
    })
}

pub fn list_groups(store: &Store) -> rusqlite::Result<Vec<ProjectGroup>> {
    store.with(|c| {
        c.prepare("SELECT * FROM project_groups WHERE deleted = 0 ORDER BY order_index, id")?
            .query_map([], group_from_row)?
            .collect()
    })
}

fn get_group(c: &Connection, id: Uuid) -> Result<ProjectGroup, RpcError> {
    c.query_row("SELECT * FROM project_groups WHERE id = ?1 AND deleted = 0", [id], group_from_row)
        .optional()
        .map_err(db_err)?
        .ok_or_else(|| RpcError::not_found("그룹을 찾을 수 없어요"))
}

fn require_local(c: &Connection, id: Uuid) -> Result<(), RpcError> {
    let local: Option<bool> = c
        .query_row(&format!("SELECT {LOCAL_PROJECT} FROM projects p WHERE id = ?1"), [id], |r| r.get(0))
        .optional()
        .map_err(db_err)?;
    match local {
        Some(true) => Ok(()),
        Some(false) => Err(RpcError::invalid("이 컴퓨터의 프로젝트만 옮길 수 있어요")),
        None => Err(RpcError::not_found("프로젝트를 찾을 수 없어요")),
    }
}

fn checked_name(name: &str) -> Result<&str, RpcError> {
    let name = name.trim();
    if name.is_empty() { Err(RpcError::invalid("그룹 이름을 입력해 주세요")) } else { Ok(name) }
}

pub fn create_group(store: &Store, p: &ProjectGroupCreate) -> Result<ProjectGroup, RpcError> {
    let name = checked_name(&p.name)?;
    atomic(store, |c| {
        if let Some(id) = p.project {
            require_local(c, id)?;
        }
        let id = Uuid::now_v7();
        let now = ts_to_sql(&runwick_core::now());
        c.execute(
            "INSERT INTO project_groups (id, name, order_index, updated_at)
            VALUES (?1, ?2, (SELECT COALESCE(MAX(order_index), -1) + 1 FROM project_groups WHERE deleted = 0), ?3)",
            params![id, name, now],
        )
        .map_err(db_err)?;
        if let Some(project) = p.project {
            c.execute(
                "UPDATE projects SET group_id = ?2, order_index = 0, placement_updated_at = ?3 WHERE id = ?1",
                params![project, id, now],
            )
            .map_err(db_err)?;
        }
        get_group(c, id)
    })
}

pub fn update_group(store: &Store, p: &ProjectGroupUpdate) -> Result<ProjectGroup, RpcError> {
    let name = p.name.as_deref().map(checked_name).transpose()?;
    atomic(store, |c| {
        get_group(c, p.id)?;
        c.execute("UPDATE project_groups SET name = COALESCE(?2, name), collapsed = COALESCE(?3, collapsed), updated_at = ?4 WHERE id = ?1",
            params![p.id, name, p.collapsed, ts_to_sql(&runwick_core::now())]).map_err(db_err)?;
        get_group(c, p.id)
    })
}

fn bucket(c: &Connection, group: Option<Uuid>) -> Result<Vec<Uuid>, RpcError> {
    c.prepare(&format!(
        "SELECT id FROM projects p WHERE group_id IS ?1 AND {LOCAL_PROJECT} ORDER BY order_index, created_at DESC, id"
    ))
    .map_err(db_err)?
    .query_map([group], |r| r.get(0))
    .map_err(db_err)?
    .collect::<rusqlite::Result<_>>()
    .map_err(db_err)
}

fn place_projects(c: &Connection, ids: &[Uuid], group: Option<Uuid>) -> Result<(), RpcError> {
    let now = ts_to_sql(&runwick_core::now());
    for (index, id) in ids.iter().enumerate() {
        c.execute(
            "UPDATE projects SET group_id = ?2, order_index = ?3, placement_updated_at = ?4
            WHERE id = ?1 AND (group_id IS NOT ?2 OR order_index != ?3)",
            params![id, group, index as i64, now],
        )
        .map_err(db_err)?;
    }
    Ok(())
}

pub fn move_project(store: &Store, p: &ProjectMove) -> Result<(), RpcError> {
    atomic(store, |c| {
        require_local(c, p.id)?;
        if let Some(group) = p.group_id {
            get_group(c, group)?;
        }
        let mut ids = bucket(c, p.group_id)?;
        if p.before == Some(p.id) {
            return if ids.contains(&p.id) {
                Ok(())
            } else {
                Err(RpcError::invalid("이동할 위치를 다시 골라 주세요"))
            };
        }
        ids.retain(|id| *id != p.id);
        let index = match p.before {
            Some(before) => ids
                .iter()
                .position(|id| *id == before)
                .ok_or_else(|| RpcError::invalid("목록이 바뀌었어요. 이동할 위치를 다시 골라 주세요"))?,
            None => ids.len(),
        };
        ids.insert(index, p.id);
        place_projects(c, &ids, p.group_id)
    })
}

pub fn delete_group(store: &Store, id: Uuid) -> Result<(), RpcError> {
    atomic(store, |c| {
        get_group(c, id)?;
        let mut ids = bucket(c, None)?;
        ids.extend(bucket(c, Some(id))?);
        place_projects(c, &ids, None)?;
        // Keep a tombstone so an offline computer cannot resurrect a deleted group.
        c.execute(
            "UPDATE project_groups SET deleted = 1, updated_at = ?2 WHERE id = ?1",
            params![id, ts_to_sql(&runwick_core::now())],
        )
        .map_err(db_err)?;
        Ok(())
    })
}

pub fn move_group(store: &Store, p: &ProjectGroupMove) -> Result<(), RpcError> {
    atomic(store, |c| {
        get_group(c, p.id)?;
        if p.before == Some(p.id) {
            return Ok(());
        }
        let mut ids = c
            .prepare("SELECT id FROM project_groups WHERE deleted = 0 AND id != ?1 ORDER BY order_index, id")
            .map_err(db_err)?
            .query_map([p.id], |r| r.get::<_, Uuid>(0))
            .map_err(db_err)?
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(db_err)?;
        let index = match p.before {
            Some(before) => ids
                .iter()
                .position(|id| *id == before)
                .ok_or_else(|| RpcError::invalid("그룹 목록이 바뀌었어요. 다시 골라 주세요"))?,
            None => ids.len(),
        };
        ids.insert(index, p.id);
        let now = ts_to_sql(&runwick_core::now());
        for (index, id) in ids.iter().enumerate() {
            c.execute(
                "UPDATE project_groups SET order_index = ?2, updated_at = ?3 WHERE id = ?1 AND order_index != ?2",
                params![id, index as i64, now],
            )
            .map_err(db_err)?;
        }
        Ok(())
    })
}

#[cfg(test)]
mod tests;
