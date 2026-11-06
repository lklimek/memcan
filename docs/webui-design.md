# MemCan web UI design

Implement a static JavaScript SPA for one operator supervising agents across 10–30 projects. **Phase 1 ships read-only Tasks and Dashboard/status.** Preserve `/ui/tasks/<id>` links. For Phase 1, no changes to core, MCP, health, authentication, or proxy configuration; the only server changes allowed are static asset serving, SPA fallback, and security headers within the existing UI module.

## 1. Evidence and current usability

Verified against [CLAUDE.md](../CLAUDE.md), [README.md](../README.md), [UI list](../rs/memcan-server/src/ui/list.rs), [UI detail](../rs/memcan-server/src/ui/detail.rs), [UI router](../rs/memcan-server/src/ui/mod.rs), [serve.rs](../rs/memcan-server/src/serve.rs), [TodoItem](../rs/memcan-core/src/todo.rs), [pipeline](../rs/memcan-core/src/pipeline.rs), [search](../rs/memcan-core/src/search.rs), [schema](../rs/memcan-core/src/schema.rs), [export](../rs/memcan-core/src/export.rs), and [Compose](../docker-compose.yml).

| Current behavior | Operator impact | Design decision |
| --- | --- | --- |
| Filters require an Apply submission and full navigation; no text, priority, or owner filter | Finding one agent's urgent task requires scanning and repeated reloads | Immediate local filtering; shareable query state |
| List truncates at 500; footer says “N tasks · limit 500”; underlying scans stop at 10,000 | Neither remaining work nor completeness is clear; no next page | Explicit scan coverage, progressive loading, client pagination |
| Six-column table scrolls horizontally on narrow screens | Title, owner, and status cannot be read together | Stacked task cards on mobile |
| Blocked-by links contain only UUIDs | Every dependency requires another page just to learn its title/status | Resolve labels; show missing references and reverse relationships |
| Dates show month/day; enum labels include `in_progress`; absent values use “—” | Age, terminology, and missing ownership require interpretation | Relative age plus exact date; readable labels; “Unassigned” |
| No status overview, queue view, freshness timestamp, or inline retry | An empty backlog and a failing service are difficult to distinguish | Independent health/data panels and recoverable errors |
| Light-only styling; no density choice, saved views, copy controls, or shortcut help | Repeated triage and sharing cost unnecessary effort | System/manual theme, compact mode, reusable views |
| Browser Basic Auth prompt is the entire login experience | Adding MCP credentials introduces an unexplained second credential | Explain page access versus API connection, preserve intended route |

Retain existing strengths: escaped text, labeled filters, semantic table, text/glyph badges, encoded task paths, filter-preserving back links, and fail-closed UI mounting. Read-only behavior itself is intentional, not a defect.

Live-code corrections to the supplied context:

- `http_mcp_router` uses `with_legacy_session_mode(false)` and `with_json_response(true)`. Current tests verify **stateless JSON**, no session header, and acceptance of obsolete session IDs. Support optional sessions for older deployments; never require one.
- `list_collections` returns only `{collections:[{name,count}]}` with internal names such as `memcan_todos`. Despite its description, it supplies no project/filter vocabulary; failed counts can omit a collection.
- `TodoItem` has no `updated_at`, author, due date, or change history. `owner` is not necessarily the creator. `completed_at` is the first terminal-state timestamp, including cancellation.
- Queue entries lack operation IDs in the recent-list response. They are an LRU sample, not a chronological audit log.

## 2. Users, jobs, and delivery order

Frequency is a design assumption, not measured analytics. Least technical persona: a developer who follows an agent's link but does not know MCP, LanceDB, or the difference between page credentials and an API key.

| Rank | Actor / job | Frequency assumption | Outcome and priority |
| --- | --- | --- | --- |
| 1 | Operator: understand a task posted by an agent | Several times daily | Open title, context, owner, and blockers directly; P1 |
| 2 | Operator: decide what needs attention across projects | Daily | Filter open/high-priority/unassigned/blocked work; P1 inspection, P2 changes |
| 3 | Operator: explain an unavailable or delayed result | When work stalls | Health, model/backend, pending operations, freshness; P1 |
| 4 | Operator: correct, assign, complete, or postpone work | Daily after inspection | Safe single/bulk task actions; P2 |
| 5 | Operator: find and improve agent knowledge | Several times weekly | Memory browsing, editing, unified search; P3 |
| 6 | Maintainer: inspect indexed sources and export data | Occasional | Code/standards, indexing controls, detailed queue, export; P4 |
| — | AI agent: hand off actionable context | Continuous writes | Existing MCP tools and stable task links; no agent login/UI |

Core stories:

- Given an agent's task link and a disconnected browser, when the operator connects, then that task opens without a global scan prerequisite.
- Given projects with mixed statuses, when the operator selects Open + High + Unassigned, then only matching loaded tasks appear and incomplete coverage remains visible.
- Given an LLM outage and functioning storage, when the operator opens Tasks, then task reading remains usable and Dashboard identifies the affected dependency.
- Given an agent uses existing MCP writes, when its task is fetched/refreshed, then the UI reflects the stored fields without requiring an agent workflow change.
- Given a maintainer needs a backup (P4), when export finishes, then the download contains the fetched JSONL records and reports its scope and consistency limitation.

Delivery dependencies: connection/static shell → data adapters and direct detail → task scan/filter views → Dashboard → accessibility/failure validation. P2 depends on P1; P3 and P4 reuse its client/state components. Exclude teams, roles, comments, notifications, due dates, drag-and-drop boards, and a general MCP console: no supporting domain/API, and no need for initial triage.

## 3. Information architecture and URLs

P1 primary navigation: **Tasks**, **Dashboard**. Logo opens Tasks. Later add **Memories**, **Search**, **Code**, **Standards**; place **Queue** and **Export** under Operations. Do not show disabled future navigation.

| Route | Purpose / phase |
| --- | --- |
| `/ui`, `/ui/` | Replace with `/ui/tasks`; preserve recognized query state |
| `/ui/connect?returnTo=…` | Connection form; returnTo accepts only known same-origin `/ui/` routes |
| `/ui/tasks` | Task list; P1 |
| `/ui/tasks/<encoded-id>` | Task detail; P1; canonical copy link omits list filters |
| `/ui/dashboard?project=…` | Scoped task overview; server health/queue/counts remain explicitly global; P1 |
| `/ui/tasks/new`, `/ui/tasks/<id>/edit` | P2; match literal routes before ID route |
| `/ui/memories`, `/ui/memories/<id>` | P3; full record via export when needed |
| `/ui/search?q=…&collection=…&project=…` | P3; collection repeatable; explicit memory scope |
| `/ui/code`, `/ui/code/<id>`, `/ui/standards`, `/ui/standards/<id>` | P4 |
| `/ui/queue`, `/ui/queue/<operation-id>`, `/ui/export` | P4; operation detail only for a known/pasted ID |

Task query contract: `project` (omitted = all), `status_mode=open|all|selected`, repeated `status`, `status_filter_present=1` (explicitly zero statuses is valid), repeated `priority`, `owner` (exact), `unassigned=1` (exclusive with owner), `q`, `group=none|project|status`, `sort=priority|created|project|status`, `dir=asc|desc`, `page` (1-based), `size=25|50|100`. Default: open statuses `pending,in_progress,blocked,postponed`, priority high first, created oldest as tie-breaker, ID final tie-breaker, group none, size 50. Open also includes unknown imported statuses unless terminal; display “Unknown: <value>”. `status_mode=all` includes every value; selected matches only repeated statuses. Without a mode, legacy status values/marker imply selected, otherwise open. Changing status checkboxes writes selected mode; Open/All views write their mode so unknown values survive URL round trips.

Retain old `project`, repeated `status`, `status_filter_present`, and `sort` URLs. Without `dir`, created sorts newest first; other sorts use their documented default. Ignore unknown keys; invalid or duplicated scalar values reset that field with a quiet notice. Build URLs with `URL`/`URLSearchParams`, encode ID path segments, never concatenate raw record IDs. Filter/sort/group changes reset page to 1; manual search uses replaceState after 150 ms, committed filter/navigation actions use pushState. Back restores filters, page, scroll, and originating title-link focus.

URL state wins over saved/default preferences. A bare Tasks route expands a chosen default view into explicit URL state once; otherwise use Open. “Reset to Open” always writes explicit Open filters so a custom default cannot intercept it. Saved views store named filter/sort/group/page-size snapshots, not result data; copyable URLs carry the full state, never a device-specific view ID.

## 4. Phase 1 screens

### Connect

This is the Phase 1 connection flow. [Section 12](#12-follow-up-authentication-and-authorization) proposes a later account login without browser-held master keys; it is not a P1 prerequisite.

The browser's native Basic Auth challenge protects HTML and assets first. The SPA cannot replace it or reliably log it out. Once loaded, show an application connection form only when no usable MCP key exists; health can load independently.

```text
MemCan                                          Theme: System
Connect to MemCan
Server: https://memcan.example:8190  [fixed origin]
Page access granted. Connect to read tasks and server activity.
API key  [********************************] [Show]
This key grants full API access, including deletion.
It stays in this tab's memory; reload or close clears it.
[Connect]                 [View server health]
After connecting: return to task 81cbe927…
```

- Use labeled password input, paste/password-manager support, reveal toggle with pressed state, required nonblank validation; do not impose an invented key format. Blank/control-character input fails locally. Clear the DOM input after submitting; hold the key only inside the transport closure. Never offer “copy key”.
- Fixed `location.origin`; no endpoint field, key in query, token prefill, or remote URL from task content. Connect runs initialization, initialized notification, then `tools/list` to verify available capabilities. Required missing read tools get a compatibility message; retain working health/detail features.
- **Storage choice: memory only**, including session IDs and fetched records. No key in localStorage, sessionStorage, IndexedDB, cookies, logs, analytics, error reports, URL, or service worker. A new tab/reload reconnects; internal navigation does not. Persist only theme/density and explicitly saved view preferences. Explain that saved filters and URL queries may contain project/owner/search text and persist in browser history; never put credentials in them.
- Header action **Disconnect / forget key** aborts requests, advances connection generation, clears DOM/data/key/session and returns to Connect. Late responses cannot repopulate state. Clear sensitive state on pagehide too; a bfcache restore must reconnect. Best-effort session DELETE, if supported, must not delay clearing. Offer separate “Clear saved preferences”. State: “API connection cleared. Browser page login may remain active.” No claim of token revocation or Basic Auth logout.
- Compose exposes plain HTTP. On non-loopback HTTP show a persistent warning before key entry: “This connection is unencrypted; page credentials and the API key can be intercepted. Use HTTPS or a localhost tunnel.” Label submission **Connect over HTTP** so the risk is explicit; keep warning in the shell. Client code cannot make this deployment secure. No auto-upgrade/redirect with a key.
- A P1 tool allowlist prevents accidental app writes, **not** use of the all-powerful key by injected JavaScript. Existing Basic Auth, allowlist and rate limiting stay intact. TLS remains an operator deployment concern.

### Dashboard

```text
MemCan   Tasks  [Dashboard]       Project: All       Commands
Connected · Read-only            Updated 12:34:56    Refresh

Overview · All projects         [Open tasks]
[Open 86] [High + open 12] [Blocked 9] [Unassigned + open 18]
Task coverage: 246 records scanned · finished 12:34

Server: Degraded · v…                 Queue [server-wide]
Dependency     State        Last observed    2 pending operations
LanceDB        Healthy      10s ago          Recent operations (10)
Embedding      Healthy      1m ago           add_memory · storing
LLM primary    Down         2m ago           update_memory · completed
  ollama / model-name        [Details]
LLM fallback   Healthy      30s ago
  anthropic / model-name

Collections [server-wide]: Memories 920 | TODOs 246 | Code …
Projects needing attention
Project       Open   High   Blocked        [View tasks]
```

- Cards link to exact task filters; Open excludes done/cancelled and includes postponed. Blocked means stored `status=blocked`, not an inferred dependency state. High and Unassigned cards intersect Open. Counts derive from the same task scan as the list; while incomplete show “≥N observed” and coverage, never authoritative totals.
- Project table sorts high-open count descending, then blocked count, then name; counts use loaded records. No “overdue”, productivity trends, recent edits, or creator attribution from unavailable fields.
- `/health` renders overall `ok|degraded`, version, and dependency map: `healthy`, `down`, `half_open` → Healthy, Unavailable, Checking recovery. Show backend/model for both LLM slots; missing fallback means “Not configured”, not failed. Missing observation age means “Not observed yet” even if reported healthy. This is circuit-breaker state, not a live diagnostic test; Refresh reads state and does not probe models.
- Health failures do not hide task content. Explain impact: LLM failure can affect processing; embedding failure affects semantic search/writes; task reads mainly need LanceDB. Do not globally disable read tools based on health alone.
- Queue panel uses `get_queue_status({limit:10})`; `pending_tasks` is queued plus running, not a percentage or ETA. Display `operation`, `user_id`, status, queued/completed timestamps; warnings/errors in an expandable text panel. “Recent” means retained sample, not complete history or guaranteed time order. P1 panel has no retry/cancel actions or invented operation links.
- Collection counts use `list_collections`; map internal table names to labels. An omitted expected collection is “Unavailable or not created”, not zero. Collection cards are informational in P1. Each panel has its own timestamp, loading state and Retry.
- “View server health” opens this route without an API connection: only health loads; task/count/queue panels show Connect actions. Do not redirect this health-only mode back to Connect automatically.

### Tasks list

```text
MemCan  [Tasks]  Dashboard       Project: All projects v
Tasks                           Read-only   12:34  [Refresh]
[Open] [High priority] [Blocked] [Unassigned] [All] [Saved v]
Find title, description, project, owner or ID [____________]
Status [4 selected] Priority [Any] Owner [Any] [Clear filters]
Group [Project v] Sort [Priority v] [High first] Density [Compact]
Showing 1–50 of 86 matches · 246 scanned · scan complete

v memcan (23 matches)
Priority   Task / context              Status       Owner      Age
High       Fix task links             Pending      Unassigned  3d
           Resolve encoded IDs        Blockers: 2
Medium     Review extraction          In progress  agent-a     1w
v other-project (63 matches)
…
[Previous] Page 1 of 2 [Next]                Rows [50 v]
```

- Title is the primary link; whole rows are not nested click targets. Columns: priority, title plus optional one-line description and blocker count, project (omit within project group), status, owner, created age. Full title wraps; description preview may truncate, detail never does. Show unknown/malformed metadata without crashing or converting it to valid defaults.
- Search-as-you-type is **local literal search**, not a semantic MCP call. Case-folded, whitespace-separated tokens must all occur across title, description, project, owner, or ID. No regex or SQL. Debounce 150 ms, ignore intermediate IME composition, announce settled result counts. Highlight using text nodes and `<mark>`; indicate when a match is in a hidden description. Search covers only fetched data; coverage stays beside results.
- Status/priority are multi-select checkboxes in labeled popovers; owner is exact selection with explicit Unassigned. Project/owner choices derive from fetched records, include currently selected missing values, and permit typed exact values. Project picker has “All projects”, recent choices and local filtering, with “Projects observed in tasks” label. Selecting a project scopes tasks, not global health/queue.
- Built-in views: Open; High priority = Open + High; Blocked = blocked; Unassigned = Open + no owner; All = all statuses including unknown. Views preserve project, reset other filters, and write explicit query state. Save current view with a name; choose/set default, rename/delete locally; handle storage unavailable with “Preferences apply for this tab”. No personalized “Mine” until the operator explicitly chooses an owner.
- Sort within groups: priority high/medium/low/unknown; created newest or oldest; project alphabetical; status pending/in progress/blocked/postponed/done/cancelled/unknown. For non-created sorts tie-break by created oldest then ID; invalid dates last in either direction. Group order: project alphabetical or that status order; none is default. Group headings are collapse buttons with counts and `aria-expanded`; collapse affects presentation only.
- Order groups, sort rows within each group, then paginate the flattened set at 50 rows; group headers may repeat across pages with “continued”. Previous/Next are native buttons; page change focuses results heading. Clamped out-of-range pages announce the change. Avoid infinite scroll/virtualized keyboard navigation in P1.
- Comfortable rows minimum 56 px, Compact 40 px; both retain accessible targets. Preference is device-local, not URL state. Mobile uses readable cards and a Filters button with active-filter count; same logical order and data, no horizontal page scrolling.
- Empty collection: “No tasks yet. Agents can add tasks through MemCan.” No creation button in P1. No matches: show active filters and Clear filters. Zero selected statuses: “Select a status or choose Open.” Partial scan with zero matches: “No matches in loaded tasks” plus Continue loading, never “No tasks”.

### Task detail

```text
< Back to tasks                 [Copy link] [Copy ID]
Fix task links
[High] [Pending]   memcan       Read-only

Description                              Details
Full stored text, wrapped;               Owner       Unassigned
line breaks preserved.                  Created     3 days ago
                                        Exact       Nov 3, 2026 10:12 UTC
Blocked by (2)                          Completed/cancelled   Not recorded
[Done] Update routing · agent-a         ID          81cbe927…
[Blocked] Deploy proxy · other-project

Blocks (observed in loaded tasks)
Publish release · Pending
[Scan all tasks for relationships]
Data fetched 12:34:56 [Refresh]
```

- On direct entry, `get_todo({todo_id})` immediately after connection; do not wait for counts or export. Resolve a unique short prefix to its canonical full-ID path using replaceState. Show ambiguous-ID errors as “Use the full task ID”; malformed/imported legacy IDs are passed to the existing tool, with failures distinguished from absence.
- Render `id,title,description,project,priority,status,owner,blocked_by,created_at,completed_at`. Empty description = “No description”; absent owner = “Unassigned”. Label the terminal timestamp “Completed/cancelled at”; do not call it latest update. No invented audit timeline. Show full ID in selectable text, even if visually shortened in the header.
- Resolve blockers from cache then `get_todo` for missing IDs, at most two concurrent requests; render progressively. Link title, status, owner and cross-project badge. Deleted/unresolvable references keep their ID and “Task unavailable”; other failures get per-reference Retry. A done/cancelled blocker remains a visible relationship, not silently removed. Dependencies never automatically change a stored task status.
- Reverse “Blocks” is a client index of `blocked_by` across loaded tasks. No matches means “None observed in loaded tasks”; a complete scan can say “None found in the last scan”. Direct detail offers an explicit all-task scan; no automatic full graph traversal. Handle self-links/cycles without recursion and label them as such. Cached cross-project blockers are not inserted into a scoped list's counts.
- Back uses preserved list state (recognized query parameters on detail links and in-session navigation context); direct entry falls back to that task's project Open view. Following blocker links preserves the original list context. Project picker on detail navigates to a project task list rather than hiding the current task.
- Copy link copies `location.origin + /ui/tasks/<encoded-full-id>` without search/auth parameters. Copy ID copies the full ID. Use Clipboard API on a user gesture; denied/insecure-context access opens a small selectable-text dialog with manual-copy instructions. No false success toast.

### Shared components

Header: logo, primary navigation with current-page state, project picker, Commands button, health/connection indicator, Refresh, theme selector, Disconnect. Wrap controls on small screens; keep title and primary actions visible. Health and “Connected to API” are distinct indicators. P1 displays a persistent Read-only label.

Command palette (`Ctrl/Cmd+K`) is a local command/navigation dialog: Tasks, Dashboard, project selection, built-in/saved views, Refresh, Theme, Disconnect, Shortcut help. It does not perform unified content search or call arbitrary tools in P1. Filter commands locally; arrows select, Enter runs, Escape closes; focus returns to trigger. A visible Commands button exposes the same actions.

Toasts: short confirmations such as “Link copied” in a polite live region, dismiss after 5 seconds; pause dismissal on hover/focus. Errors stay in context with a recovery action, never only in a toast. Do not repeat routine polling success announcements.

## 5. Data states and refresh

| State | Presentation and behavior |
| --- | --- |
| Initial load | Stable skeleton plus “Loading tasks”; `aria-busy` on the affected region; detail/header load independently |
| Progressive scan | Render first page immediately; “N records loaded; loading more”; search/counts clearly partial; Stop loading |
| Empty | Distinguish absent records, no filter matches, no selected statuses, and incomplete coverage |
| `/mcp` 401 | Stop all API work, forget key/data, keep destination, focus Connect error: “API key rejected. Reconnect with a valid key.” |
| `/ui/*` 401 | Browser-managed Basic Auth; failed module load retains static “Reload to sign in to page access” guidance |
| 403 | “Access denied by server/network policy”; stop automatic retries; explain allowlist/origin policy may be responsible, not necessarily the key |
| Network/timeout/503 | Keep prior data with fetched time and “Refresh failed”; Retry panel; no fake empty result |
| 429 | Honor Retry-After or bounded backoff; show when retry is available |
| Degraded `/health` | HTTP success can still mean degraded; render dependency state and continue independent reads |
| Missing task | Handle successful tool payload `{error:"todo not found",todo_id}` as not found; offer Back/Retry, not reconnect |
| Protocol/schema error | “Server response could not be read”; retain previous data, offer Retry; no raw HTML/stack traces |
| Unknown enum/invalid date | Neutral badge/raw selectable value and “Unrecognized”; do not silently discard the task |

Refresh rules:

- GET health every 30 s while visible; queue on Dashboard every 15 s, counts every 60 s. Refresh current detail every 60 s. Only poll panels in use, except the lightweight header health indicator; pause while hidden/offline. Disconnect stops MCP polling; health-only Dashboard can still refresh health. On return, refresh stale resources, coalescing duplicate requests.
- Task list has a 60 s freshness target. While visible, run at most one replacement scan per 60 s; no overlapping scans. Keep the previous snapshot while refreshing; stage refreshed data, show “Updated data available”, and apply on click to avoid rows moving under a pointer or keyboard focus. Manual Refresh applies when ready and restores focus by task ID. New/removed records invalidate derived counters and relationships together.
- Mark individual resources Stale after two missed freshness intervals; retain last successful fetch time. Never claim that refresh locks the database or detects every concurrent write. Health age reflects server observation; UI fetch time is separate.
- Timeout P1 reads after 20 s. Retry transient read failures at most twice with jittered 1 s/3 s delays; then wait for next scheduled refresh or manual Retry. Auth/validation/missing-record failures do not retry. Cancelled route requests are silent. Limit API concurrency to two and starts to four per second; user detail requests take priority over background scans.
- UI's 10 rps/burst 20 limit applies to static assets under `/ui`, **not** `/mcp` in current Compose. Keep initial asset fan-out below that burst; avoid cache-busting reload loops. Request pacing is still useful for MCP server load.

## 6. Later phases

### P2 — task editing and triage

Create/edit uses a full-page form or wide dialog from detail: required trimmed title/project, description textarea, priority, owner text, blocker IDs selected from loaded tasks or resolved by `get_todo`. Create always produces pending; editing project is unavailable because `update_todo` has no project field. Explicit blank owner/description and empty blocker list clear those fields; omit untouched fields. Show server validation beside the relevant control and keep drafts in RAM on failure.

Detail actions: Edit, Complete, Reopen (pending), Start, Block, Postpone, Cancel, Delete. Status changes do not modify blockers; explain unresolved blockers before completion. Complete uses `complete_todo`; other changes use `update_todo`. Delete confirms exact title/project/ID and irreversibility; Cancel is a reversible status action with different wording. No fictional server undo. Refresh before editing/submission, detect changed fields by comparing fetched values, and ask the operator to reconcile; without revisions there is still a race after that check.

List selection appears only in P2. “Select this page” is explicit; changing filters clears selection. Bulk assign/priority/status/complete/delete shows selected count and summary before applying individual calls, tracks each outcome, and retains failed items for retry. No atomicity claim or automatic replay after an ambiguous network failure. For any mutation timeout, refetch before offering another action; creation cannot be deduplicated reliably without server support. Delete/complete require live confirmed IDs, never search-result excerpts.

### P3 — memories and unified search

Memories has scope selector **Default scope / Project / Explicit user scope / All scopes (browse)**, text list with content preview, type/topic/source when present, created/updated time, and detail with complete content plus expandable metadata. `user_id=project:<name>` determines project scope; do not assume arbitrary metadata `project` is authoritative. `global` is the normal default but server configuration may override it; use omitted scope for “Default”, explicit `user_id:"global"` for “Global”.

Browse scoped memories with `get_memories`/`count_memories`; use paged `export_collection` for All scopes, continuation beyond limits, and exact-ID detail. Edit content only through `update_memory`; scope/metadata editing is unsupported. Create supports content, scope and optional metadata via `add_memory`. Add/update return an operation ID: show Queued/Processing, poll, then reload stored content (extraction/dedup can change the result). Preserve draft text on failure. Delete confirms full record identity and scope.

Search route offers query, collections, memory scope, project/code path/stack and standards filters. Submit explicitly or after a 400 ms pause with at least two characters; ignore stale responses. Use `search` for mixed results; dedicated search tools expose additional filters. Result cards identify collection, scope, excerpt and relevance ordering; scores are not percentages/confidence. Unified text/metadata excerpts can be truncated to 500 characters; open full details before editing or copying full content.

“All projects” is not an all-memory semantic search: omitted project selects default memory scope, while code/TODOs can span projects and standards stay global. Label this coverage next to the scope control. All-scope memory browse/literal filtering works through export; genuinely global semantic memory search needs server support or explicit, bounded per-scope queries labeled as partial. `has_more` means increase top-N/refine query, not a next-page cursor. Empty query never triggers embedding.

### P4 — code, standards, queue and export

- **Code:** project → file → indexed records; path, stack, line range, indexed time if present, description/snippet as plain `<pre><code>`. `search_code` for semantic results; export for browsing/IDs/full payloads. Indexed content is not guaranteed to be the current repository or a full file; no source editing/download claims. Optional file upload preview with paths/project/stack calls `index_code_files`; never request arbitrary server paths. Scoped deletion uses `delete_code_records` with project and optional exact path/extension; show actual scope and warn the returned count is approximate.
- **Standards:** standard ID/type/version/language → chapter/section, refs and source URL; `search_standards` and paged export. Validate external links as HTTP(S), display destination, open with noopener/noreferrer, no automatic fetching/embedded media. Upload/paste Markdown plus standard ID/type/version/lang/URL invokes `index_standards` (500 KiB content limit), then queue tracking. Replace/reindex needs explicit confirmation of source/scope; dropping uses `drop_indexed_standards({standard_id})` and affects every indexed version under that ID.
- **Queue:** pending count plus up to 100 retained operations, local status/operation/scope filters, expandable warnings/errors. Explain queued, embedding, extracting facts, deduplicating, storing, completed, completed with warnings (`completed_degraded`), failed. A known ID from this tab or pasted by the operator supports polling/detail. Recent rows have no retrievable ID and therefore no fabricated deep link. No percentage, durable history, cancellation, or automatic retry tool exists. Unknown/expired ID says “No longer available”, not failed.
- **Export:** choose collection and structured equality filters; preview scope and estimated count where available, then explicit Download JSONL. Fetch pages, show records/bytes, cancel client download, deduplicate IDs, preserve full exported fields except vectors (already omitted). Build a Blob only within a 32 MiB client budget, revoke object URL after use; over budget offer separately labeled page/part downloads or the existing CLI. Never silently download a truncated “complete backup”. No `_import_records` UI. Offset scans are best effort during agent writes, not transactional backups.

## 7. Visual system

Use system fonts and CSS custom properties; no remote fonts, icons, analytics or CDNs. Language is English. UI dates use the browser locale with an explicit timezone; UTC remains available as exact selectable text.

| Token | Light | Dark |
| --- | --- | --- |
| `--bg` | `#F8FAFC` | `#0F172A` |
| `--surface` | `#FFFFFF` | `#1E293B` |
| `--text` | `#0F172A` | `#F1F5F9` |
| `--muted` | `#475569` | `#CBD5E1` |
| `--border` (decorative) | `#CBD5E1` | `#475569` |
| `--control-border` | `#64748B` | `#94A3B8` |
| `--accent` / focus | `#1D4ED8` | `#93C5FD` |
| `--action-bg` / action text | `#1D4ED8` / `#FFFFFF` | `#93C5FD` / `#0F172A` |
| `--danger` | `#991B1B` | `#FCA5A5` |

Theme = System (default via `prefers-color-scheme`), Light, Dark; manual `data-theme` overrides media query and persists. Set `color-scheme` for native controls. Respect `prefers-reduced-motion`; no pulsing skeletons or essential animation.

Type: 12 px auxiliary metadata, 14 px compact labels, 16 px body/control text, 20 px section titles, 28 px page title; body line-height 1.5, titles 1.25. Monospace only for IDs/code. Spacing scale 4/8/12/16/24/32/48 px; radius 6 px controls, 10 px panels; use borders, minimal shadows. Content max width 1440 px; detail description approximately 75 characters per line. Controls 40 px desktop, 44 px touch.

Badges always include text; icons are supplementary and hidden from assistive technology. Status pairs (foreground/background, light → dark): Pending `#334155/#E2E8F0` → `#E2E8F0/#334155`; In progress `#1E40AF/#DBEAFE` → `#BFDBFE/#1E3A8A`; Blocked `#991B1B/#FEE2E2` → `#FECACA/#7F1D1D`; Done `#166534/#DCFCE7` → `#BBF7D0/#14532D`; Postponed `#854D0E/#FEF9C3` → `#FEF08A/#713F12`; Cancelled/Unknown use neutral pairs. Priority: ▲ High uses blocked pair, ■ Medium uses postponed pair, ▽ Low uses neutral pair; labels disambiguate shared colors. Never strike through cancelled titles into unreadability.

Breakpoints: below 640 px, one-column cards, filters dialog, wrapped header; 640–1023 px, condensed table/stacked detail; 1024 px and above, full table and description/metadata columns. Test at 320 CSS px and zoom; long words/IDs wrap, code alone may scroll horizontally. Sticky elements must not cover focused controls.

## 8. Accessibility contract

Target [WCAG 2.2 AA](https://www.w3.org/TR/WCAG22/): text contrast ≥4.5:1 (large text ≥3:1), meaningful controls/focus ≥3:1, minimum 24×24 CSS px targets with spacing; prefer 44 px touch targets. Verify both themes, hover/selected/error states and text at 200%/page at 400% zoom.

- Use header/nav/main landmarks, skip link, one H1, actual buttons/links, labeled fields, table caption/headers and `aria-sort`. Card metadata uses lists/definition lists. Status never relies on color.
- Route changes update document title and focus H1 (`tabindex=-1`); browser Back restores origin link/scroll. Filtering retains input focus. Modal dialogs trap focus, Escape closes, and return focus to trigger. No keyboard trap in tables.
- Use polite live regions for settled result counts, completed refresh and copy confirmations; assertive alerts for blocked submission/auth failure. Associate inline errors with inputs, move focus to error summary after failed submission. Respect reduced motion; keep errors until resolved. Allow paste/password managers.

| Keys | Behavior |
| --- | --- |
| Tab / Shift+Tab, Enter / Space | Native focus and activation; full workflow without custom shortcuts |
| Ctrl/Cmd+K | Open/close Commands |
| `/` | Focus task search when outside inputs/editors/dialogs |
| `?` | Shortcut help when outside inputs/editors/dialogs |
| Escape | Close topmost popover/dialog; do not navigate away or erase search |
| Arrow keys, Enter | Palette/project suggestions only; native behavior elsewhere |

Settings/help can disable single-character shortcuts. No unmodified global delete/complete shortcut; no interception of browser refresh/find/back shortcuts. Shortcuts are enhancements, not the only path.

## 9. Client implementation architecture

### Placement and runtime

Place static sources under `rs/memcan-server/src/ui/static/`: `index.html`, `styles.css`, `app.js`; `router.js`, `mcp.js`, `store.js`, `data.js`; `views/{connect,dashboard,tasks,task}.js`; `components.js`. Later phases add view modules without changing transport/domain code. This is a suggested boundary map; small pure utilities can live beside their owner. Plain native ES modules and DOM APIs are enough; a framework/runtime/compiler provides little value for four P1 screens. Native history, dialog, fetch and URL APIs avoid a vendored dependency and build step. Keep the initial module graph below 20 requests; lazy-load later screens. Test files are not served.

Embed the allowlisted assets via Rust `include_str!`/`include_bytes!` so the existing Rust-only Docker image needs no Node, asset-copy layer or filesystem browsing. UI router serves HTML for recognized SPA routes, assets with correct MIME types, and genuine 404 for unknown assets; no arbitrary filesystem paths or fallback HTML returned as JS. Preserve Basic Auth and fail-closed mount requirements on every UI route/asset. Unknown application route renders an in-app Not found page; `/mcp` and `/health` never enter SPA fallback. Include static `<noscript>` and module-load failure guidance.

UI response CSP: `default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self'; font-src 'none'; object-src 'none'; base-uri 'none'; frame-ancestors 'none'; form-action 'none'`. All scripts/styles external; no inline handlers, eval, remote content or worker. Preserve `Cache-Control: no-store`, `X-Content-Type-Options: nosniff`; add `Referrer-Policy: no-referrer` within UI headers. No service worker/offline persistence. DOM rendering uses `createElement`, `textContent`, `append` and safe property assignment; never `innerHTML`/`insertAdjacentHTML` with records, errors, queries or metadata. Descriptions remain plain text in P1. Validate link schemes/destinations separately; HTML escaping alone does not secure a URL.

### MCP client contract

Keep endpoint/credentials/auth-error handling inside `mcp.js`; the session transport seam is defined in [§12](#12-follow-up-authentication-and-authorization). P1 implements only the Bearer strategy below.

Use same-origin `/mcp`, explicit Bearer Authorization, JSON Content-Type, Accept `application/json, text/event-stream`, `credentials:'omit'` (no browser-managed Basic credentials), `redirect:'error'`, and `cache:'no-store'`. Explicitly authored Authorization must be verified in browser tests. GET `/health` omits authorization. Asset requests use normal browser page authentication. See [Fetch credential behavior](https://developer.mozilla.org/en-US/docs/Web/API/Fetch_API/Using_Fetch#including_credentials).

1. POST JSON-RPC `initialize`, unique ID, `{protocolVersion:"2025-11-25",capabilities:{},clientInfo:{name:"memcan-webui",version:"1"}}`. Validate returned version against supported versions; do not guess compatibility.
2. Capture optional `Mcp-Session-Id` in RAM. POST `notifications/initialized` without JSON-RPC ID; accept empty 202, do not parse it as JSON. Subsequent requests include negotiated `MCP-Protocol-Version` and session header only when supplied. This follows [MCP lifecycle](https://modelcontextprotocol.io/specification/2025-11-25/basic/lifecycle).
3. Discover `tools/list` once per connection (follow its cursor if present); intersect with a hard-coded P1 allowlist: `get_todo`, `list_todos`, `export_collection` restricted to todos, `list_collections`, `get_queue_status`. Discovery never enables arbitrary server-advertised writes.
4. Calls use `{jsonrpc:"2.0",id,method:"tools/call",params:{name,arguments}}`. Match ID; handle JSON-RPC error, `result.isError`, and domain error payload separately. Current tools wrap JSON in `result.content[].text`; parse the expected text block once, validate per-tool shape. Export then parses `data` as JSONL, not a nested metadata object. If structuredContent is present, validate it through the same adapter.
5. Current server returns application/json without sessions. For session-bearing deployments, a 404 clears the session and triggers one shared reinitialization, then retries the read once; a second 404 is endpoint failure. A sessionless 404 is not a task-not-found signal or an endless reconnect trigger. Send negotiated version headers consistently. See [MCP transport/session contract](https://modelcontextprotocol.io/specification/2025-11-25/basic/transports).
6. Support POST SSE responses with a bounded streaming parser (UTF-8 chunks, blank-line event boundaries, multiline data, comments, matching RPC ID); current JSON path is primary. No background GET/SSE subscription is needed. Unexpected server requests are rejected as unsupported, not dispatched as UI commands. Disconnection/abort never implies a later-phase mutation was cancelled.

Keep connection generation and initialization promise centralized: concurrent reads share initialization, stale-generation responses cannot alter state, and only reads are automatically replayable. JSON parsing/data validation failures become typed client categories: auth, forbidden, unavailable, throttled, missing, invalid-input, protocol. UI errors use controlled messages; expandable diagnostics show sanitized codes/timestamps, never headers or credentials. Do not render arbitrary server error bodies as HTML or blindly copy them to telemetry.

### Data normalization, scanning and state

`data.js` maps export's flattened `{_collection,id,...payload}` into TodoItem; require usable ID, preserve unknown enum/raw date values, normalize missing optional fields. Count/report malformed rows separately so “scan complete” cannot hide skipped records. Store normalized objects in `Map`s, not attacker-keyed prototype objects. The task store keeps entity data, per-resource fetched time, scan scope/coverage, reverse dependency index, and errors separately from route/UI state.

P1 all-project list/dashboard algorithm:

1. Start `export_collection({collection:"todos",limit:500,offset:0})`; no `list_todos` fan-out across guessed projects. Render each page incrementally. Offset advances by returned **count**, not deduplicated size; finish when count < page size (including zero). Count disagreement with parsed JSONL is a response error.
2. Deduplicate by full ID; collect project/owner vocabulary and reverse links. Sort/filter/paginate locally. `list_collections` provides a comparison count, never the stopping condition. Offset pagination has no stable snapshot/order guarantee under concurrent writes: deduplication removes duplicates but cannot repair omissions.
3. Auto-scan at most 10,000 records or 16 MiB of response bytes, then pause with “Partial data” and Continue loading. Continue fetches one 500-record page per action up to a 32 MiB task-cache budget measured as serialized record bytes, including staged scans (JS heap use will be higher). At the budget show “Narrow to a project”; evict inactive scans when switching scope. Keep response decoding bounded, abort oversized responses, and do not persist data. These are client safety limits, not claims about the collection size.
4. Selected project can use a scoped export scan with fixed-column equality filter `project = '<escaped-value>'`; quote apostrophes as doubled apostrophes, matching core `sanitize_eq`, with tests for quotes/control characters. Never accept raw SQL/filter strings from URLs or inputs. A complete global cache can satisfy the project locally. Scoped scans do not pretend to enumerate other projects.
5. Optional quick project preview uses `list_todos({project,limit:200})`; treat it as partial even with `has_more:false` near the core scan ceiling. Export remains the authoritative UI browse path. Direct detail/cache refresh wins over older scan responses for the same entity; timestamp request generations so a slow scan cannot overwrite a newer fetch. Replacement scans remove absent rows only when their scope completes, and retain unresolved status if a scan fails.

No cross-tab data sharing or persisted query-result cache. Preferences are versioned, validated on read, and resilient to quota/private-mode errors. Navigation aborts obsolete view reads; route generation checks also ignore completed responses for old routes. Keep form/search focus stable with keyed, small DOM updates rather than replacing the entire page on each keystroke.

### Testing approach

Before implementation, encode acceptance cases below. Use `node --test` with pure `.mjs` fixtures/modules for URL round trips/legacy queries, filtering/sorting/grouping, enum/date handling, SQL equality escaping, export decoding/limits/dedup, generation races, capability allowlisting and mocked MCP lifecycle/session errors. Node is a development/CI test runner only; production serves unchanged ES modules.

Use Playwright against disposable fixtures with fake keys: direct navigation and reload, real Basic-plus-Bearer separation, stateless JSON and simulated session/SSE variants, reconnect/forget, panel failures, progressive scans, mobile layout and keyboard flow. Intercept network to assert **zero mutation calls in P1**, no key leakage in URL/storage/logs, and no unwanted external requests. Include real browser-to-test-server smoke coverage through the actual UI router so static MIME, CSP, Basic challenge, deep-link fallback and browser Origin handling are exercised. Use local accessibility tooling plus keyboard/screen-reader checks; automated scans alone do not establish AA conformance. No live operator data or API keys in fixtures.

## 10. API mapping and limitations

All tool rows below mean `POST /mcp` → `tools/call` with these names/arguments; the MCP/auth lifecycle is shared. No browser REST CRUD endpoints are assumed.

| Feature | Concrete call(s) | Contract / phase |
| --- | --- | --- |
| Connect | `initialize`, `notifications/initialized`, `tools/list` | Protocol calls, not tools; P1 |
| Health/backend/model | `GET /health` | status, version, dependency map; no Bearer; P1 |
| All/project task browse, vocabulary, counters | `export_collection({collection:"todos",limit:500,offset,filter?})` | `{count,offset,data}` JSONL; max tool limit 10,000/page; P1 |
| Project preview | `list_todos({project,status?,owner?,limit:200})` | `{todos,has_more}`; no offset; P1 optional |
| Task/blocker/detail refresh | `get_todo({todo_id:fullId})` | TodoItem or successful not-found marker; P1 |
| Filter/group/sort/search/saved views/copy/reverse links | Client state + fetched task records | No extra API; reverse index has scan coverage; P1 |
| Collection counts | `list_collections({})` | Internal table names; missing count is unknown; P1 |
| Queue summary / known operation | `get_queue_status({limit:10})`; later `{limit:100}` or `{operation_id}` | pending_tasks + operations, or single status/error; P1/P4 |
| Create task | `add_todo({title,project,description?,priority?,owner?,blocked_by?})` | Pending TodoItem; P2 |
| Edit/triage task | `update_todo({todo_id,title?,description?,priority?,status?,owner?,blocked_by?})` | No project/revision field; P2 |
| Complete/delete task | `complete_todo({todo_id})`, `delete_todo({todo_id})` | Per-item operations; bulk loops client-side; P2 |
| Scoped memories | `get_memories({project?,user_id?,limit:100})`, `count_memories({project?,user_id?})` | List max 1000, has_more but no cursor; P3 |
| Memory/full indexed record detail | `export_collection({collection,filter:"id = '<escaped-id>'",limit:1,offset:0})` | Fixed ID equality builder; no single-record memory/code/standard getter; P3/P4 |
| Memory search | `search_memories({query,project?,user_id?,limit:50})` | Max 1000; increase/refine, no next page; P3 |
| Unified search | `search({query,collections:["memories","todos","code","standards"],project?,user_id?,limit:50,standard_type?,standard_id?,tech_stack?,file_path?})` | Max 100 global results, candidate-pool has_more, scope caveat above; P3 |
| Add/update/delete memory | `add_memory({memory,project?,user_id?,metadata?})`; `update_memory({memory_id,memory,project?,user_id?})`; `delete_memory({memory_id,project?,user_id?})` | Add/update async operation_id; deletion immediate; P3 |
| Code search | `search_code({query,project?,tech_stack?,file_path?,limit:50})` | Max 100, no offset, dedicated results omit record IDs; P4 |
| Standards search | `search_standards({query,standard_id?,standard_type?,ref_id?,tech_stack?,lang?,limit:50})` | Max 100, dedicated results omit record IDs; P4 |
| Code/standards browse and filter vocabulary | `export_collection({collection:"code"\|"standards",limit:500,offset,filter?})` | Literal fixed-column equality filters; no full repository browsing; P4 |
| Index code | `index_code_files({project,tech_stack,files:[{path,content}]})` | Async tracking via returned operation_id; P4 |
| Index/drop standards | `index_standards({content,standard_id,standard_type,version?,lang?,url?})`; `drop_indexed_standards({standard_id})` | Index async; drop irreversible; P4 |
| Delete code | `delete_code_records({project,extension?,file_path_exact?})` | Mandatory project, approximate count; P4 |
| Download/export | `export_collection({collection,limit:500,offset,filter?})` | Existing read tool, no vectors; P4 download UI |

API gaps requiring server changes for a stronger guarantee; none is a P1 dependency:

| Gap | Existing-client workaround / honest limit |
| --- | --- |
| No project/owner facet endpoint; counts expose no vocabularies | Derive from task export; label observed set, allow exact typed project; no proof of empty projects |
| No all-project list_todos or cursor; max 200 and core 10k scan ceiling | Paged export with explicit client budget; never claim list_todos is exhaustive |
| No stable cursor/snapshot, change feed, ETag or updated_at for tasks | Periodic replacement scans, dedup, timestamps; counts/relationships are best-effort observations |
| No blocked_by reverse query | Client reverse index with scope/coverage; lazy direct lookups for blockers |
| No authorization scopes, read-only key, token exchange/revocation, or Basic logout | RAM-only full key and UI allowlist; protection against XSS remains essential; Disconnect only forgets locally |
| No server filter counts, task history/author/due dates | Derive counts from loaded records; omit unsupported UI promises |
| No mutation revisions, transactions, idempotency keys, or bulk tools | Refresh/compare, per-item outcomes and explicit retry reconciliation; concurrent overwrite/duplicate creation risk remains |
| No get-memory/code/standard-by-ID tool | Fixed-ID filtered export; dedicated code/standard search result without ID opens inline content, not a fabricated detail URL; unified search includes IDs |
| No full semantic-search pagination; memory search scoped by default | Increase top-N to cap, refine query, literal browse export; no “next semantic page” |
| Queue list lacks IDs, durable history, cancellation, or retry | Known IDs from initiating response/paste only; show retained sample and expired state |
| No memory metadata/scope update or task project move | Content/allowed fields only; do not simulate move as destructive delete/recreate |
| Health has no live probe, metrics history or latency endpoint | Show observed breaker state/backend/model; no uptime, throughput, or inference-latency charts |

## 11. Phase 1 acceptance criteria

- [ ] Given an unauthenticated UI request, Basic Auth still protects HTML/assets; missing UI credentials leave routes unmounted. Only UI serving/CSP glue changes server behavior.
- [ ] Given a fresh browser at `/ui/tasks/<uuid>?project=memcan`, connecting opens that task before any task export completes; reload and browser Back work.
- [ ] Given a unique short task prefix, the detail resolves/canonicalizes it; missing/ambiguous/malformed IDs have distinct recoverable states.
- [ ] Given current stateless MCP, connection succeeds without `Mcp-Session-Id`; simulated session expiry causes one shared re-init/read retry, not a loop. Initialized 202 has no JSON parse error.
- [ ] Given a wrong key or later 401, polling stops, key/data clear, and the intended route survives; Disconnect prevents late responses restoring data. No key/session in persistent storage, URL, logging, or copied links.
- [ ] Given non-loopback HTTP, warning and HTTP-specific Connect wording are visible before entry; HTTPS has no warning. Clipboard rejection exposes manual copy.
- [ ] Given 0, 500, 501, and 10,001 exported tasks, pagination ends correctly, first results are usable early, and budgets/partial counts are explicit. Duplicates do not inflate counts; malformed rows and failed pages remain visible as incomplete coverage.
- [ ] Given a project name containing quotes and URL punctuation, filtering and deep links round-trip safely; no user-supplied raw SQL is accepted.
- [ ] Given search typing, IME input, status/priority/owner changes and saved defaults, results match the specified local rules; search emits no MCP calls; legacy query parameters retain their meaning.
- [ ] Given a blocked task, existing, missing and cross-project blockers show useful labels; reverse links declare scan coverage; cycles do not recurse; terminal blockers do not silently change task status.
- [ ] Given LLM failure with functioning LanceDB, task reads still work; Dashboard identifies backend/model and degraded dependency, absent fallback and never-observed health accurately.
- [ ] Given collection/queue failure, other panels remain useful; zero is never substituted for unknown. Queue summary is server-wide and makes no unsupported ID/history/percentage claims.
- [ ] Given a background scan while reading a list, results do not jump; updated data is announced for application. Hidden tabs pause polling; rapid Refresh clicks coalesce; stale/network/403/429/503 states offer appropriate recovery.
- [ ] Given malicious text in title/description/project/ID/error/metadata, it remains inert text; links stay intended; CSP blocks inline/remote scripts and no external resources load.
- [ ] Given keyboard-only use, Connect → filters → detail → blockers → Back → Disconnect works with visible focus; Commands/shortcuts have visible equivalents; screen-reader announcements do not spam on polling.
- [ ] Given 320/768/1280 px viewport, both themes, reduced motion and zoom, controls remain reachable, titles readable and statuses non-color-dependent; required contrast is measured.
- [ ] Network assertions show P1 calls only approved read tools plus MCP lifecycle; no task mutation, memory/indexing action or download-export UI appears.
- [ ] Pure-module tests, mocked browser scenarios and actual UI-router smoke checks pass without live data; production Docker build requires no Node or CDN.

## 12. Follow-up: authentication and authorization

**Design only; not Phase 1.** Replace the two-credential flow with a UI login and server-side authorization. The browser must never receive `MEMCAN_API_KEY`. Scope: one MemCan instance and a small trusted operator group; no account self-registration or identity-management product. Agents keep their existing Bearer `/mcp` integration.

### Decision and alternatives

| Option | Benefit | Cost / decision |
| --- | --- | --- |
| **BFF `/ui/api/mcp`, opaque cookie session, server tool allowlist** | No API secret or bearer token readable by browser JS; immediate logout/revocation; roles enforced before dispatch | Adds login/session/policy code; same-origin XSS can still act as the signed-in user. **Recommended:** fits the existing Axum UI boundary and small deployment. |
| Server-minted short-lived scoped tokens | Suitable for non-browser clients and independently hosted frontends | JS-held tokens remain stealable; refresh/revocation and audience/scope validation complicate `/mcp`. Not needed for a same-origin UI. |
| Keep Basic Auth, proxy with username → role mapping | Smallest intermediate server change; removes the master key from the browser | Retains browser prompt, repeated credential transmission and unreliable logout; shared credentials weaken attribution. Optional transition only, not the target UX. |
| OIDC / Traefik forward-auth | Central identity, MFA and account lifecycle when an IdP already exists | Additional service/configuration, callback/cookie policy and trusted-header boundary; identity alone does not authorize MCP tools. Defer; it can later supply a principal to the same BFF policy. |

Use an **in-process BFF adapter**, not an open HTTP relay: reuse the existing typed tool dispatcher through a narrow internal interface. It needs no injected master key. If implementation constraints force HTTP forwarding, the destination must be a fixed private loopback service, with server-owned Bearer credentials; never accept client URLs or forward client Authorization/cookies upstream. Do not call back through the public Traefik router. Prefer reuse of the current MCP implementation over a second protocol implementation; keep session/auth state separate from MCP session IDs.

### Identity, login and recovery

Default rollout is explicit opt-in `MEMCAN_WEBUI_AUTH_MODE=session`; absence keeps current `basic` behavior. In session mode, an optional root-owned, read-only JSON users file contains `{username,password_hash,role,disabled}` records; hashes use Argon2id PHC encoding with unique salts (minimum 19 MiB, two iterations, parallelism one; benchmark higher cost under bounded concurrency). Do not place plaintext multi-user passwords in environment variables. Username is a stable audit identity; shared accounts cannot identify a person. See [OWASP password storage](https://cheatsheetseries.owasp.org/cheatsheets/Password_Storage_Cheat_Sheet.html).

When no users file is configured, existing non-empty `MEMCAN_WEBUI_USERNAME/PASSWORD` bootstraps one **viewer** in session mode; hash the password at startup and warn that the plaintext environment secret should be replaced. Operators deliberately assign editor/admin in the users file. File presence takes precedence; unreadable/invalid file, duplicate users, unknown roles or missing credentials fail closed, never fall back silently to Basic/master-key access. Initial release reloads users on restart, invalidating all sessions; no live account-management UI. Basic mode retains its current mounting rules.

`GET /ui/login` renders an accessible username/password form: “Sign in to MemCan”, “Your account determines what you can view or change”, **Sign in**, and “You will return to: task 81cbe927…” (never disclose a task title before authentication). Support paste, password managers, `autocomplete=username/current-password`, visible password toggle, generic invalid-credentials message and Retry-After feedback; never reveal whether an account exists. No remember-me option initially; browser password saving is distinct from a persistent session. Session restoration by browsers never overrides server expiry.

Deep link flow (HTML mirror includes a sequence diagram):

1. Browser → `GET /ui/tasks/<id>`; server validates/normalizes the relative destination and redirects an unauthenticated navigation to `/ui/login?returnTo=…`. Accept only known `/ui/` application routes and query keys; reject authority URLs, backslashes, control characters and auth endpoints. Never include secrets in returnTo.
2. Browser → `GET /ui/auth/session`: obtain a short-lived pre-login CSRF challenge bound to an opaque `__Host-memcan_ui_pre` cookie with the same security flags as the session cookie. Login assets, login page and this bounded bootstrap endpoint are public; no task data is embedded. All application HTML/assets remain session-gated except the explicitly allowlisted login assets.
3. Browser → `POST /ui/auth/login` with credentials and CSRF proof. Server checks origin/rate limits, verifies hash, destroys pre-auth state and creates a fresh session/CSRF token. Respond with cookie and validated destination, never an API key; client replaces the login history entry and opens the intended task.
4. Browser → `POST /ui/api/mcp` with cookie and CSRF header. BFF authenticates, authorizes method/tool/arguments, then dispatches internally; return the normal MCP envelope. Unauthorized API requests return JSON 401 (not redirects/HTML/Basic challenges).
5. Browser → `POST /ui/auth/logout`; revoke session, clear cookie using the same Path/flags, return to login. Clear cached records/drafts and abort in-flight reads in the client. Server ignores cached browser Basic credentials in session mode, so they cannot silently log the user back in.
6. On expiry, stop polling, clear protected data, retain only the validated destination in RAM, and show “Session expired. Sign in to continue.” Warn before expiry while editing; no persistent draft storage, no automatic replay of writes. Other tabs clear on next 401 and may receive a credential-free logout signal; never share tokens/data across tabs.

### Authorization boundary

**Deny by default on every request.** `tools/list` returns only role- and phase-enabled tools, but discovery/UI visibility is not enforcement. Parse JSON-RPC once, reject batches/unknown methods, bound request size and concurrency, validate typed arguments before dispatch. Allow only `initialize`, `notifications/initialized`, `tools/list` and authorized `tools/call`; no arbitrary resources/prompts/import/admin escape hatch. Reject unsupported notifications and validate negotiated protocol/session ownership. UI role and enabled feature phase are intersected: enabling session auth does not enable P2–P4 writes.

| Role | Tool allowlist (cumulative; only when its phase is enabled) |
| --- | --- |
| Viewer | P1: `get_todo`, `list_todos`, `export_collection` (todos only), `list_collections`, `get_queue_status`. P3: `get_memories`, `count_memories`, `search_memories`, `search`, export memories. P4: `search_code`, `search_standards`, export code/standards. Validate collection names and fixed equality predicates; enforce pagination/size limits. |
| Editor | Viewer + P2 `add_todo`, `update_todo`, `complete_todo`; P3 `add_memory`, `update_memory`. No deletion/index administration. |
| Admin | Editor + P2 `delete_todo`; P3 `delete_memory`; P4 `index_code_files`, `index_standards`, `delete_code_records`, `drop_indexed_standards`. `_import_records` and every unlisted tool remain denied. |

Deletion requires admin, an exact scope/record confirmation and password reauthentication within five minutes, enforced by the BFF, not only a dialog. `POST /ui/auth/reauth` checks the same password/CSRF/rate controls and rotates the session; it grants no additional role. Issue via `POST /ui/auth/confirm` a short-lived, single-use server confirmation nonce bound to session, tool and canonical arguments; no replay after uncertain results. Bulk actions authorize each item and report individual outcomes. Audit success, denial and uncertain outcomes. Existing core restrictions (including destructive-tool configuration guards) remain effective. Role changes/revocation invalidate sessions rather than trusting a role cached forever in a cookie.

Project restrictions are an **optional later extension**, not a claim of isolation in the first auth release: roles initially see all projects/scopes. A project ACL must constrain reads, export/search arguments and results, ID lookups, dependency labels, queue, counts and writes on the server; verify record ownership before ID operations and prevent project/user_id overrides. Hide global counts/queue where safe scoping is unavailable. Do not treat a client project filter or raw export SQL as authorization; disable unsupported scoped tools until core query support exists.

### Session and deployment security

- Opaque 256-bit random session ID; store only its digest plus principal, role/policy version, CSRF secret, creation/last-activity timestamps and recent-reauth time. Start with a bounded in-memory store and per-user session cap: restart logs everyone out. A persisted shared store is deferred until multi-replica/availability needs justify it; it needs TTLs, atomic rotation/revocation and secret protection. Never use the existing queue/history as a session store.
- Cookie: `__Host-memcan_ui=<opaque>; Path=/; HttpOnly; Secure; SameSite=Strict`, no Domain or Max-Age. The `__Host-` prefix requires Path=/; it cannot be narrowed to `/ui`. `/mcp` ignores it and continues to require its existing Bearer auth. Use a dedicated origin; the cookie path does not isolate sibling apps. See [MDN Set-Cookie](https://developer.mozilla.org/en-US/docs/Web/HTTP/Reference/Headers/Set-Cookie).
- Proposed defaults: idle 30 minutes, absolute 8 hours; background polls do not extend idle lifetime. A rate-limited `POST /ui/auth/activity` endpoint extends idle only after foreground user interaction; absolute expiry remains fixed. Rotate at login, reauthentication and privilege changes; invalidate the old ID atomically and never reissue it from stale responses. BFF requests bind to the current principal; serialize reauth with mutation submission. Logout revokes future calls but cannot cancel a write already dispatched. These are project policy choices informed by [OWASP session management](https://cheatsheetseries.owasp.org/cheatsheets/Session_Management_Cheat_Sheet.html).
- Require exact configured Origin plus synchronizer CSRF token on **all UI POSTs**, including MCP reads, login, logout, activity, reauth and confirmation. Pre-login challenge expires after five minutes and is replaced on login. Send the CSRF token in `X-CSRF-Token`, keep it in RAM, never a URL; do not conflate it with the session cookie. Reject cross-site Fetch Metadata where supplied; JSON-only APIs, no permissive CORS, no GET mutations. SameSite is defense in depth, not the sole control. See [OWASP CSRF prevention](https://cheatsheetseries.owasp.org/cheatsheets/Cross-Site_Request_Forgery_Prevention_Cheat_Sheet.html).
- App login controls: five failed attempts per normalized username per 15 minutes with bounded backoff, generic responses and equivalent hash work for unknown users; additionally cap all login attempts and Argon2 concurrency before hashing. Avoid permanent account lockout. Traefik adds per-client limits using only trusted proxy-derived IPs. Existing 10 rps/burst 20 UI limiting alone is not sufficient brute-force protection. Rate-limit the bootstrap/session store too; return 429 and Retry-After.
- **TLS required in session mode**, including local testing via HTTPS. Plain HTTP serves a non-sensitive “HTTPS required” explanation without credential inputs and rejects login/API POSTs; no insecure-cookie fallback or “continue anyway”. Redirect only safe GET navigation to the configured HTTPS origin before secrets are entered. Behind Traefik, trust forwarding headers solely from configured proxy CIDRs, strip spoofed incoming identity/proto headers, block public access to the app port, and validate Host/Origin against the configured public origin. Do not infer TLS from arbitrary `X-Forwarded-Proto`. Deploy HSTS on the HTTPS host once operational.
- Emit structured UI audit records: UTC time, request ID, stable username, role, tool, target IDs/project/scope, authorization decision, outcome and returned operation ID. Log before dispatch and after result; record timeout as uncertain and correlate async completion where possible. Exclude passwords, cookies, CSRF/API keys, record contents and raw arguments. Restrict log access/retention; shared bootstrap login cannot attribute individual humans. Make audit-storage failure visible and fail closed for destructive actions if an audit event cannot be recorded.

Keep the higher-priority `/ui` Traefik router (including auth and `/ui/api/mcp`) free of the master-Bearer middleware; preserve IP allowlist and rate limits. `/mcp` retains both current application Bearer enforcement and proxy protection; `/health` retains its existing routing. Do not add a second Traefik Basic challenge in session mode. OIDC later requires trusted, stripped/replaced identity headers and a local role mapping; no browser-supplied identity headers. A top-level same-origin landing page can recover a session cookie withheld on an external deep link by Strict semantics; never expose data in that first unauthenticated response.

Preserve P1 CSP and text-only rendering: external same-origin JS/CSS, `connect-src 'self'`, no inline script/eval. Login is JS-fetch based, so `form-action 'none'` can stay; a future native form fallback needs an explicit change to `'self'` plus CSRF validation. Keep `frame-ancestors 'none'`, `no-store`, `nosniff`, no-referrer on login/API responses too. The standalone visual HTML is a documentation artifact with inline CSS, not a production CSP template.

### Change map and rollout

| Proposed location / configuration | Responsibility |
| --- | --- |
| `rs/memcan-server/src/ui/mod.rs`; new `auth.rs`, `session.rs`, `policy.rs`, `api.rs` | Route separation, credentials, cookies, sessions/CSRF, rate limits, role/argument enforcement, internal MCP adapter, audit hooks; UI-local typed errors. |
| `rs/memcan-server/src/serve.rs` | Expose/pass a narrow typed dispatcher/shared context into UI router; preserve `/mcp` Bearer middleware and protocol behavior. No generic bypass route. |
| `rs/memcan-core/src/config.rs` | Parse/validate/redact new settings only; no domain/auth changes to core. |
| `ui/static/mcp.js`, connection view and shell | Transport strategy, login/expiry/logout; unchanged data adapters and task views. |
| `docker-compose.yml`, deployment docs/tests | Users-file secret mount, HTTPS/trusted-proxy settings, explicit auth mode, router/CSRF/role tests. |
| `MEMCAN_WEBUI_AUTH_MODE=basic\|session` (default basic) | Explicit backward-compatible rollout; no per-request downgrade. |
| `MEMCAN_WEBUI_USERS_FILE` (optional) | Absolute path to read-only JSON credential/role file; existing username/password fallback only when unset. |
| `MEMCAN_WEBUI_PUBLIC_ORIGIN` (required in session mode) | Single canonical HTTPS origin for host/origin/return checks. |
| `MEMCAN_WEBUI_TRUSTED_PROXIES` (empty by default) | Explicit proxy CIDRs allowed to assert forwarding metadata. |
| `MEMCAN_WEBUI_SESSION_IDLE_SECS=1800`, `MEMCAN_WEBUI_SESSION_ABSOLUTE_SECS=28800` | Bounded, validated timeouts; idle must not exceed absolute. No remember-me setting initially. |

**P1 migration seam:** centralize endpoint, credential mode, auth headers and auth-error handling in `mcp.js` behind a transport interface (`connect`, `call`, `disconnect`, connection state). P1 uses Bearer `/mcp`, `credentials:'omit'` and RAM key. Session mode uses same-origin `/ui/api/mcp`, `credentials:'same-origin'`, CSRF header and no Authorization. An allowlisted, non-sensitive server bootstrap auth-mode value selects the strategy; never guess by probing credentials or downgrade to master-key Connect after a session 401. Preserve JSON-RPC envelopes, data normalization and route state. A browser-readable `GET /ui/auth/session` returns principal/role, expiry and CSRF token only with a valid session; its unauthenticated form returns only pre-login challenge state. If this same-origin bootstrap recovers an existing session after an external deep link, return directly to the validated destination without prompting for the password again.

1. **P1 now:** ship the current read-only design and transport seam; no new server auth or proxy dependency.
2. **Auth foundation:** implement/test session store, TLS/CSRF/login and BFF viewer allowlist behind opt-in mode. Verify login → original task, revocation/restart, no API key in network/DOM/storage, forged role/project/JSON-RPC requests, and unchanged agent Bearer calls. Do not offer writes yet.
3. **Deployment switch:** configure TLS/users, set session mode, restart and invalidate old browser state. Remove Connect key entry in this mode. Rollback is an explicit operator mode change/restart to Basic; users are told the old separate-key flow is restored. Never auto-fallback on failure.
4. **Before P2 writes:** enable editor/admin policy, reauth/confirmations, per-item audit and failure reconciliation; test forged CSRF, editor deletion, expiry mid-edit and ambiguous mutation timeouts. P3/P4 extend the explicit allowlist only with their feature rollout. Decide OIDC/project ACLs separately.

Open product decisions (recommended defaults above keep implementation unblocked): Is one shared viewer sufficient or are named users needed immediately? Which operators may edit/delete? Is all-project visibility acceptable? Is an IdP already maintained? Are restart logout and 30-minute/8-hour limits acceptable? Who owns TLS and audit retention?
