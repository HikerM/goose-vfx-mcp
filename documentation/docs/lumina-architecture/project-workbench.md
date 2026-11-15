---
title: Project Workbench
---

# Project Workbench

Project Workbench turns Lumina Desktop into a persistent project-development environment. A project is an absolute workspace directory, not a single file and not a disposable chat. Lumina detects repository manifests and Git metadata, stores project work independently from the message transcript, and links every execution to the ACP session that performed it.

## Persistent model

The session database owns the following records:

- `projects` and `project_roots` identify a canonical workspace and its permitted roots.
- `work_items` store an objective, acceptance criteria, and review state.
- `project_runs` link a work item to an ACP session and a terminal outcome.
- `run_steps` expose the discover, plan, approval, execute, validate, and review lifecycle.
- `workspace_checkpoints` record resumable lifecycle state.
- `change_sets`, `change_files`, `validation_runs`, and `project_artifacts` provide stable extension points for diff, test, and artifact features.

Only one running run is allowed per work item and per linked session. A process restart changes unfinished runs to `interrupted`; completed history is never rewritten. Reopening a canonical root updates its detected profile while preserving an explicitly assigned project name.

## Desktop workflow

The Projects entry opens a directory chooser, then creates or reopens the project. Project Home lists stored projects and their detected stacks. Project Workbench uses three coordinated surfaces:

1. the work-item list for persistent objectives;
2. the execution surface for starting a project-aware ACP session and reviewing its outcome;
3. the inspector for manifests, Git state, run steps, checkpoints, and interruption recovery.

Starting a work item creates an ACP session with `projectId` and `workItemId` metadata. The session working directory is the project root, so repository search, multi-file edits, commands, validation, and later follow-up prompts all operate in the project context. The run remains visible after navigating away or restarting the desktop app.

## Project media and transcript archives

Tool dispatch freezes the session's project, work item and active run before execution. Native jobs inherit that scope; direct native execution freezes it before resource admission. Publication uses the frozen scope, so moving a session to another project while a task runs cannot move its result. Existing outputs without a frozen scope are not retrospectively assigned to a project.

`project_execution_scopes_v1` retains execution ownership. `project_task_configurations_v1` retains validated native parameter snapshots independently of the source session. `project_assets_v1` stores immutable media metadata or transcript text, configuration, source identity and a bounded summary. Marking media ready and inserting its archive share a transaction; prepared-publication recovery uses the same path. Transcript publication and archiving also share a transaction. Files remain in the managed media output store.

The read-only `native_tasks__project_assets` and `native_tasks__project_asset` tools resolve the current project from the authenticated session. Listing returns paged summaries; detail returns configuration and bounded transcript text. The tools accept no caller-supplied project or session identity. Another session in the same project can discover the archived result after the source conversation is deleted and the database is reopened. Existing session-scoped media access remains unchanged: archive discovery does not itself provide cross-session playback, input reuse, or editing.

Session IDs retain the existing date-and-sequence format but use a persistent high-water mark. An upgrade seeds it from known retained session references, and subsequent imports advance it transactionally. Deleting a conversation never lowers the mark. Historical IDs whose records and every known reference were already removed before the upgrade cannot be reconstructed.

Project assets are a persistence foundation. Parent/child asset versions, typed text/image/time selections, capability-aware edits, preserved-region verification and the complete natural-language workflow still require implementation and acceptance.

## ACP extension boundary

Desktop clients use the `_lumina/project/*` extension methods defined in `lumina-sdk-types`. The renderer keeps local DTO types and does not depend on generated OpenAPI clients. New capabilities should extend this typed boundary and the persistent domain first; UI-only state must not become the source of truth for run status.

## Extension rules

- Add a schema migration for every persistent model change and test both fresh creation and upgrade.
- Keep terminal run history immutable; create a new run for retries.
- Store large file content outside the project tables and reference it through artifacts.
- Require explicit user approval before destructive workspace or external-system actions.
- Treat checkpoints as recovery metadata, not as a replacement for Git or filesystem backups.
