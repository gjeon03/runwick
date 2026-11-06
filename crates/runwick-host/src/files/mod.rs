//! Files and documents (S03): tree listing, file-name search, reading, conditional saving,
//! new documents, change checks for open documents, and handing files to other apps.
//! Every request is resolved against the workspace folder on this host and confined to it.

mod content;
mod external;
mod find;
mod fuzzy;
mod list;
mod paths;

use crate::host::Ctx;
use crate::router::Router;
use runwick_core::api::files::*;
use runwick_core::rpc::{RpcError, topic};
use std::path::PathBuf;

/// Run blocking file work off the async runtime.
async fn blocking<T: Send + 'static>(f: impl FnOnce() -> Result<T, RpcError> + Send + 'static) -> Result<T, RpcError> {
    tokio::task::spawn_blocking(f).await.map_err(RpcError::internal)?
}

fn root(ctx: &Ctx, id: uuid::Uuid) -> Result<PathBuf, RpcError> {
    paths::workspace_root(&ctx.host.store, id)
}

pub fn register(r: &mut Router) {
    r.add::<FilesList, _, _>(|ctx: Ctx, p: ListDir| async move {
        let root = root(&ctx, p.workspace_id)?;
        let git = list::git_marks(&root).await;
        blocking(move || {
            let git_ref = git.as_ref().map(|(m, prefix)| (m.as_ref(), prefix.as_str()));
            list::list(&root, &p.dir, p.show_hidden, git_ref)
        })
        .await
    });
    r.add::<FilesFind, _, _>(|ctx: Ctx, p: FindFiles| async move {
        let root = root(&ctx, p.workspace_id)?;
        find::find(root, p.query, p.limit).await
    });
    r.add::<FilesRead, _, _>(|ctx: Ctx, p: ReadFile| async move {
        let root = root(&ctx, p.workspace_id)?;
        blocking(move || content::read(&root, &p.path, p.max_bytes)).await
    });
    r.add::<FilesWrite, _, _>(|ctx: Ctx, p: WriteFile| async move {
        let root = root(&ctx, p.workspace_id)?;
        let r2 = root.clone();
        let out = blocking(move || content::write(&r2, &p.path, &p.text, p.base_hash.as_deref())).await?;
        if matches!(out, WriteOutcome::Saved { .. }) {
            find::invalidate(&root);
            ctx.host.bus.changed(&[topic::FILES]);
        }
        Ok(out)
    });
    r.add::<FilesCreate, _, _>(|ctx: Ctx, p: CreateFile| async move {
        let root = root(&ctx, p.workspace_id)?;
        let r2 = root.clone();
        let out = blocking(move || content::create(&r2, &p.rel_dir, &p.name, p.kind)).await?;
        find::invalidate(&root);
        ctx.host.bus.changed(&[topic::FILES]);
        Ok(out)
    });
    r.add::<FilesStat, _, _>(|ctx: Ctx, p: StatFiles| async move {
        let root = root(&ctx, p.workspace_id)?;
        blocking(move || Ok(content::stat(&root, &p.paths))).await
    });
    r.add::<FilesReveal, _, _>(|ctx: Ctx, p: FileRef| async move {
        let root = root(&ctx, p.workspace_id)?;
        let path = paths::resolve_existing(&root, &p.path)?;
        external::reveal(&path).await
    });
    r.add::<FilesOpenExternal, _, _>(|ctx: Ctx, p: OpenExternal| async move {
        let root = root(&ctx, p.workspace_id)?;
        let path = paths::resolve_existing(&root, &p.path)?;
        let editor = crate::meta::kv_get(&ctx.host.store, EDITOR_SETTING)
            .ok()
            .flatten()
            .and_then(|v| v.as_str().map(str::to_string));
        external::open(&path, p.with, editor).await
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Store;

    #[test]
    fn workspace_root_comes_from_records() {
        let store = Store::open_in_memory().unwrap();
        let d = tempfile::tempdir().unwrap();
        let opened = crate::projects::open_folder(&store, &d.path().to_string_lossy()).unwrap();
        let root = paths::workspace_root(&store, opened.workspace.id).unwrap();
        assert_eq!(root, std::fs::canonicalize(d.path()).unwrap());
        assert!(paths::workspace_root(&store, uuid::Uuid::new_v4()).is_err());
        drop(d);
        let gone = paths::workspace_root(&store, opened.workspace.id).unwrap_err();
        assert_eq!(gone.code, runwick_core::rpc::codes::NOT_FOUND);
    }
}
