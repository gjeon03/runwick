# Runwick project context

- Communicate in Korean. Follow inherited personal Git and coding preferences.
- Runwick is a new independent Rust product, not a Pinloom refactor or branch.
- Do not modify the running Pinloom installation, its repository, or its data as part of Runwick work.
- Read `docs/decisions.md` before proposing architecture. Recommendations in research documents are not approved implementation decisions.
- The user chose a Rust-based native GUI. Electron is excluded; the Rust UI framework remains open.
- Keep the existing CLI interaction experience while supporting durable conversation search and pins. The record panel and provider adapters are design proposals, not implemented features.
- The approved navigation model is project > workspace > session. Start in an automatic default workspace; create a separate Git worktree only when the user chooses to separate work. Files and document tabs follow the selected workspace.
- Closing the window preserves running CLI processes; reopening reconnects to live runs. An explicit stop-and-quit action shows managed running work before stopping it. After an OS restart, restore records and layout, and let the user select supported sessions to resume. Keep background execution visible and stoppable.
- Automatically save conversation changes and update keyword indexes incrementally. Automatic AI wiki maintenance, timeline summaries, and embeddings require project-level opt-in. On battery, defer new automatic analysis while allowing explicit user requests. Expose each job's reason, status, recent result, and stop control.
- Environment settings in Runwick are global only; project-specific configuration stays in project tooling such as .env. Provide a session refresh action to apply environment and MCP changes while preserving the conversation and workspace. Provider reload versus restart-and-resume must be verified; never substitute a new chat or restart unrelated sessions silently.
- Pinloom source is research material; compatibility means deliberate data import or interoperability, not shared writable storage.
- Keep confirmed observations, user decisions, and proposed designs distinguishable. Cite source revisions for implementation claims.
- Do not put credentials, local databases, provider transcripts, or personal wiki content in this repository.
- This repository currently contains documentation only. Do not add a JavaScript package setup or Rust scaffold merely to satisfy a documentation check.
