# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

Obsidian community plugin that two-way-syncs a vault folder with a Lark Wiki space. Status: **pull + verified push working on the lark-cli docx v2 API**; conflict-resolution UI (3-way diff modal) is still a stub. Roadmap and open questions live in [PLAN.md](PLAN.md).

`isDesktopOnly: true` — the plugin shells out to `lark-cli` via Node `child_process.spawn`, so it cannot run on Obsidian mobile. Requires `lark-cli` ≥ 1.0.30 (the version that ships the docx v2 commands).

This plugin is the surviving frontend of the abandoned **larknotes** standalone desktop app (this repo's `apps/desktop/` + Rust `crates/`). The sync engine here is being incrementally rebuilt in TypeScript by porting larknotes' battle-tested Rust logic — `crates/sync/` and `crates/provider-cli/` are the donor reference.

- **Increment 1** — two-axis change detection + push verification, ported from `crates/sync/src/decision.rs` and `Executor::execute_push`.
- **Increment 2** — switched all doc I/O to the **lark-cli docx v2 API** (`--api-version v2 --doc-format markdown`). v2's markdown converter round-trips losslessly (math, tables, code all survive), so the v1-era hacks are gone: no `<lark-table>`/`<image>` rewriting, no underscore-escaping math `port` (`provider-cli/math.rs` deleted in larknotes too — the same v2 switch fixed both).
- **Increment 3** — remote-change detection via `obj_edit_time` instead of content hashes. `wiki nodes list` returns `obj_edit_time` per node for free, so a sync only fetches docs whose edit time moved — a no-change sync now does **zero `fetchDoc` calls**. Ported in spirit from larknotes' `query_metas` / `modify_time` baseline.
- **Increment 4** — pull path moved to the **v2 XML format** (`--doc-format xml --detail with-ids`), converted to Markdown locally by `larkXmlToObsidianMarkdown` (`src/util/larkXml.ts`, `@xmldom/xmldom`). v2 *markdown* drops images to `![](signed-url)`; v2 *XML* keeps `<img src="<file_token>">`, so the `_attachments/` download cache works again — images now sync as stable `![[token.ext]]` embeds. Push still goes out as v2 markdown; the two-axis state tolerates the format split.

## Commands

```bash
npm install
npm run dev        # esbuild watch: main.ts → main.js (inline sourcemap)
npm run build      # tsc --noEmit typecheck, then esbuild production bundle (minified)
npm run typecheck  # tsc --noEmit -skipLibCheck
npm test           # vitest run — pure-logic unit tests (*.test.ts colocated with source)
```

`vitest` covers the pure, I/O-free logic ported from larknotes (`src/sync/decision.test.ts`). Anything touching `lark-cli` or the vault still has to be validated by loading the built `main.js` into a real vault (see "Dev install" below).

### Dev install into a vault

```bash
cd /path/to/vault/.obsidian/plugins
ln -s /absolute/path/to/obsidian-lark-wiki-sync ./lark-wiki-sync
# in Obsidian: Settings → Community plugins → reload → enable "Lark Wiki Sync"
```

The repo already contains a self-symlink `obsidian-lark-wiki-sync → .` — it's intentional, used to make the symlink target name stable inside vault plugin folders.

## Architecture

The plugin is deliberately thin: **`lark-cli` owns all Lark API knowledge, auth, and scopes.** This repo only adds an Obsidian-side UX (ribbon, wizard, settings tab) and the sync state machine. If a Lark operation is missing, add it to `lark-cli` first, not here.

Layering (entry point → leaves):

- [main.ts](main.ts) — `Plugin` subclass. Wires `LarkCli`, `StateStore`, `SyncEngine`, registers ribbon icon + 3 commands (`sync-now`, `setup`, `dry-run`) + settings tab. First-time ribbon click opens the wizard instead of syncing.
- [src/settings.ts](src/settings.ts) — `LarkWikiSyncSettings` schema, `DEFAULT_SETTINGS`, and `LarkWikiSyncSettingTab`. `configured: boolean` gates whether sync runs or opens the wizard.
- [src/ui/SetupWizardModal.ts](src/ui/SetupWizardModal.ts) — 6-step modal (`intro → auth → space → root → local → confirm`). Mutates a local `draft` object; calls `savePartial()` on each step to persist and re-prime `LarkCli`.
- [src/lark/LarkCli.ts](src/lark/LarkCli.ts) — typed shell-out wrapper around `lark-cli`. Every call appends `--as <identity>` from settings. Doc I/O uses the **docx v2 API**; `fetchDoc` pulls XML (`--doc-format xml --detail with-ids`), `createDoc`/`updateDoc` push markdown. All return structured JSON (`data.document.{content,document_id,url}`).
- [src/util/larkXml.ts](src/util/larkXml.ts) — `larkXmlToObsidianMarkdown` (pull): parses v2 XML (`@xmldom/xmldom`) → Obsidian GFM, including `<img src=token>` → `![[file]]` and wiki links → `[[wikilinks]]`. `extractImageTokens` pulls the `src` tokens for the download cache. Unit-tested in `larkXml.test.ts`.
- [src/util/obsidianToLarkMd.ts](src/util/obsidianToLarkMd.ts) — `obsidianToLarkMarkdown` (push): an identity passthrough — v2 markdown accepts vault GFM directly. Kept as a seam for future Obsidian→Lark transforms.
- [src/sync/SyncEngine.ts](src/sync/SyncEngine.ts) — the state machine (see below).
- [src/sync/decision.ts](src/sync/decision.ts) — pure `decide()` classifier + `SyncDecision` enum. No I/O. Ported from larknotes `crates/sync/src/decision.rs`; unit-tested in `decision.test.ts`.
- [src/state/StateStore.ts](src/state/StateStore.ts) — persists `FileSyncState` to `.obsidian/plugins/<id>/sync-state.json`. Keyed by `nodeToken` (schema v4).
- [src/util/hash.ts](src/util/hash.ts) — `sha1(utf8)`.

### The sync classification (core of the engine)

`SyncEngine.run()` iterates Wiki nodes with `obj_type === "docx"` (other types are skipped in v0.1). Change detection is **two-axis** — local and remote use *different signals*, each cheap on its own side. `FileSyncState` stores:

- `localBaseHash` — hash of the vault file (Obsidian-form) at last sync
- `remoteEditTime` — the wiki node's `obj_edit_time` at last sync

```
localChanged  = hash(vault file)      ≠ localBaseHash
remoteChanged = node.obj_edit_time    ≠ remoteEditTime
```

`obj_edit_time` comes back in the `wiki nodes list` call the engine already makes, so **`fetchDoc` is only called when a doc actually needs its content** — `remoteChanged`, or first sight of a node. A no-change sync fetches nothing. (The classify loop reads each vault file regardless — hashing local content is free and needs no network.)

`decide(localChanged, remoteChanged, hasBase)` (in `decision.ts`) maps the two booleans to `NoChange` / `PushLocal` / `PullRemote` / `BothModified` / `NewFile`. Two `NewFile` edge cases handled at the call site in `planOneSpace`:

- **No state + no local file** → brand-new pull.
- **No state + local file exists** → reconcile if `hash(local) === hash(remote)`, else conflict (untracked collision; logged, counted, not overwritten).

**Post-push baseline.** After `updateDoc`, `captureRemoteBaseline()` calls `getNode()` for the node's *new* `obj_edit_time` and records that — so the next sync doesn't see our own write as a remote change. `getNode` succeeding also confirms the node still exists. Same call runs for the `keep-local` conflict resolution.

**Legacy migration (v3→v4).** Entries written before increment 3 have a `remoteBaseHash` content hash but no `remoteEditTime`. On their first post-upgrade sync they're fetched once, classified by the old content-hash compare, then re-baselined onto `obj_edit_time`; an unchanged legacy entry goes onto `plan.rebaselines` (state-stamp only, no I/O). `remoteBaseHash` is dropped the moment an entry is next recorded.

`ask` policy is currently a stub: it writes the remote side to `<localPath>.remote.conflict.md` so neither side is destroyed, and logs a warning. Building the real 3-way diff modal (`src/ui/ConflictModal.ts`) is the next major UI task.

### Path mapping (v0.1 flat)

`mapNodeToLocalPath()` mirrors the node tree into folders via each node's `parentPath`. `StateStore` is keyed by `nodeToken` (not `localPath`), so path-mapping changes don't orphan state — the entry's `localPath` field just gets updated on the next sync.

### lark-cli contract assumptions

`LarkCli` assumes these `lark-cli` commands and shapes — they are the integration contract and must match the installed CLI (**≥ 1.0.30** for the v2 docx commands):

- `contact +get-user` → `{ data: { user: { name, user_id } } }`
- `wiki spaces list` → `{ data: { items: [{ space_id, name }] } }`
- `wiki nodes list --params {space_id,…}` → `{ data: { items: [{ node_token, obj_token, obj_type, title, obj_edit_time }], has_more, page_token } }` — `obj_edit_time` (Unix seconds, string) is the remote-change signal
- `wiki spaces get_node --params {token,obj_type:"wiki"}` → `{ data: { node: { …, obj_edit_time } } }` — used post-push to capture the new edit time
- `docs +fetch --doc <t> --api-version v2 --doc-format xml --detail with-ids` → `{ data: { document: { content, document_id, revision_id } } }` — `content` is XML; `with-ids` tags every block with its id
- `docs +create --api-version v2 --doc-format markdown --content - [--parent-token]` → `{ data: { document: { document_id, revision_id, url } } }` (title comes from the content's leading H1 — there is no `--title` flag)
- `docs +update --doc <t> --api-version v2 --doc-format markdown --command overwrite --content -` → `{ data: { document: {…}, result: "success" } }`
- `docs +media-download --token <t> --output <rel> --overwrite` → `{ data: { saved_path, content_type, size_bytes } }` — used to pull `<img src>` assets into `_attachments/`

The v2 markdown roundtrip is verified lossless (math, tables, code, nested lists) — see the live `test_live_math_roundtrip_preserved` in `crates/provider-cli`. The XML→Markdown converter is verified against live XML in `larkXml.test.ts`. If a call starts returning unexpected shapes, check `lark-cli` version first.

## Conventions worth preserving

- **Don't add direct Lark HTTP calls.** Everything goes through `LarkCli` / `lark-cli`. Auth and scope handling belong upstream.
- **`SyncEngine.run()` must be idempotent.** Re-running with no changes should produce `skipped: N, pulled: 0, pushed: 0, conflicts: 0`. This is checklist item #4 before calling v0.1 done.
- **Never destroy either side on conflict.** The `ask` fallback writes a `.remote.conflict.md` sidecar; preserve this invariant when building the modal.
- **The remote baseline is `obj_edit_time`, never a content hash.** After any push, `captureRemoteBaseline()` must record the node's *fresh* `obj_edit_time` (via `getNode`) — not the pre-push value, and never `hash(localMd)`. A stale remote baseline re-creates the phantom-change bug.
- **`recordSync()` takes `{ localBaseHash, remoteEditTime }` explicitly.** Pull/reconcile use the listing's `obj_edit_time`; push uses the post-push `getNode` value. A wrong value here breaks all future classification for that file. Recording an entry also drops its legacy `remoteBaseHash`.
- **Don't fetch in the classify loop unless `mustFetch`.** The whole point of increment 3 is that an unchanged doc costs one cheap `obj_edit_time` compare and zero network. Adding an unconditional `fetchDoc` back into `planOneSpace` undoes it.
- **Port, don't reinvent.** New sync logic should be ported from larknotes' Rust `crates/sync/` with its tests, not designed fresh. Keep the ported TS structurally close to the Rust so the two can be diffed.
- **Stay on the v2 docx API.** Never reintroduce v1 (`--format pretty` / `--mode` / `--markdown`) — v1 is deprecated and its lossy markdown converter is the bug the v2 switch fixed. Don't re-add `<lark-table>`/`<image>` rewriting to the converters; v2 is plain GFM both ways.
- **Pull fetches XML, push sends markdown — keep it that way unless you also do push images.** XML is needed on pull for stable image tokens; markdown push is fine because the remote baseline is `obj_edit_time`, not a content hash, so the format split costs nothing. Pushing *new local* images back is still unimplemented (the markdown push path can't carry a file token) — that needs `docs +media-insert` or a v2-XML push.
