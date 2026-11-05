# Runwick project context

- Communicate in Korean. Follow inherited personal Git and coding preferences.
- Runwick is a new independent Rust product, not a Pinloom refactor or branch.
- Do not modify the running Pinloom installation, its repository, or its data as part of Runwick work.
- Read `docs/decisions.md` before proposing architecture. Recommendations in research documents are not approved implementation decisions.
- The user chose a Rust-based native GUI. Electron is excluded; the Rust UI framework remains open.
- Keep the existing CLI interaction experience while supporting durable conversation search and pins. The record panel and provider adapters are design proposals, not implemented features.
- The approved navigation model is project > workspace > session. Start in an automatic default workspace; create a separate Git worktree only when the user chooses to separate work. Files and document tabs follow the selected workspace.
- Pinloom source is research material; compatibility means deliberate data import or interoperability, not shared writable storage.
- Keep confirmed observations, user decisions, and proposed designs distinguishable. Cite source revisions for implementation claims.
- Do not put credentials, local databases, provider transcripts, or personal wiki content in this repository.
- This repository currently contains documentation only. Do not add a JavaScript package setup or Rust scaffold merely to satisfy a documentation check.
