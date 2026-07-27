# EndlessSketch project guidance

## Scope and authority

This file is the project-local source of working rules and current status for
EndlessSketch. System, developer, and direct user instructions take precedence.
Within this file, earlier "Working rules" are durable constraints; the later
status sections describe the current checkpoint and may be updated after work.

Do not use `status_worker.json` as the primary startup source after this
migration. Keep this file and `docs/project-implementation-plan.md` current
instead. The JSON file remains historical/user-owned unless the user explicitly
asks to remove or update it.

## Working rules

- Answer the user in Russian unless the user explicitly requests another
  language.
- Keep tasks short and bounded. Split larger work into checkpoints that the
  user can test and redirect; do not bundle unrelated interaction changes.
- Before behavior-changing edits, update "Active request" below with the user
  request, assumptions, scope, and validation plan.
- After a completed checkpoint, update this file's current-status sections and
  `docs/project-implementation-plan.md`, then use them as the handoff context
  for the next checkpoint.
- Never modify or commit user-owned untracked files unless the user explicitly
  places them in scope. In particular, preserve `.idea`, `.ess`, `.esp`,
  `desktop-shot.png`, local runtime data, and existing `.7z` archives.
- Vector operations and SQLite metadata are authoritative. PNG tiles and other
  caches must always remain safe to delete and rebuild.
- After behavior, interface, dependency, or persistence changes, run
  `cargo fmt --all -- --check`, relevant tests, full all-target tests when
  proportionate, `cargo clippy --all-targets -- -D warnings`, and
  `cargo build --release`.
- Every implementation handoff must describe what changed, visible behavior,
  persistence/data-safety effects, automated validation, known limitations,
  and one concise minimum manual GUI check.
- Put the minimum manual GUI check in the user-facing response. Do not spend
  the user's limited session running GUI scenarios unless explicitly asked.
- Keep existing documents under `docs/` aligned with behavior and commands.
  Keep `docs/project-implementation-plan.md` as concise checkbox/status items,
  and prefer an existing suitable document over creating a new one.
- When the user adds a future item without choosing its location, place it in
  the technically most convenient bounded checkpoint. Ask only when placement
  would materially change requested behavior.
- When adding a setting, decide explicitly whether it belongs in Performance,
  Balanced, and Quality presets. Add settings that materially affect speed,
  visual quality, input density, fallback cost, tile work, or preview cadence.
  Manual pause, Display, logging, cache budget, and data/editability controls
  can remain independent when documented.
- If `release/EndlessSketch-Rust/endless-sketch.exe` is locked during a portable
  refresh, first offer to close the running application and retry the copy.

## Project snapshot

- Name/status: EndlessSketch, active.
- Purpose: Windows deep-zoom drawing application with effectively unbounded
  hierarchical coordinates, durable vector operations, and rebuildable PNG LOD
  tiles.
- Stack: Rust 1.96, eframe/egui, wgpu, SQLite/WAL.
- Architecture: GPU desktop UI; BigInt hierarchical coordinates and a
  depth-normalized camera; append-only/persistent editing history; spatial
  operation indexes; asynchronous raster cache and verified recovery backups.
- Document format: directory-based `.esketch`. SQLite/WAL vector data is the
  source of truth. Content revisions are monotonic so stale tiles cannot become
  current after restart.
- Important paths: `Cargo.toml`, `src/app.rs`, `src/coords.rs`,
  `src/storage.rs`, `src/raster.rs`, `README.md`,
  `docs/project-implementation-plan.md`.
- Branch context: feature commit `42b6a6e` is published on
  `agent/depth-tiles-vector-eraser` in draft PR
  `https://github.com/Ideshon/EndlessSketch/pull/19`, targeting default branch
  `Rust` at `5c41662`.

## Validated product baseline

The detailed completed-checkpoint history lives in
`docs/project-implementation-plan.md`. The current validated baseline includes:

- Hierarchical depth and extreme lateral navigation, bookmarks, drawing,
  Fill, undo/redo, reopen, coordinate overlay, and direct depth/tile jumps.
- Persistent object Selection with ranked click/Alt cycling, Inside/Crossing
  rectangle behavior, grouped move/scale/rotate/flip/recolor/reorder,
  clipboard operations, Eraser Lasso, and append-only replacement/tombstone
  history.
- Photoshop-like persistent layers with create/rename/reorder,
  visibility/lock, duplicate/delete, move selection to layer, and Merge Down.
- External bilingual Help, File/Navigation/Bookmarks/Selection UI separation,
  configurable Display overlays, settings profiles, tile controls, cache
  budget/encoding, preview FPS, and storage durability modes.
- CompactBlock/Freeze foundations, dense-selection optimizations, spatial
  filtering, retained fallback, asynchronous tiles, and performance/session
  diagnostics.
- F-01 persistent operation-bounds cache: warmed bounds hit rate 99.44%; the
  conservative slow-frame median `fallback_project_ms` improved 44.1% from
  57.192 ms to 31.971 ms.
- F-02 interaction frame timing rejects idle/window-resume gaps and records
  valid active-frame measurements only in the 0..=250 ms window.
- F-03a/F-03b move fallback cache/budget/projection lifecycle and visible
  operation projection/culling behind `FallbackRenderer` without changing
  painter ordering or document authority.
- F-04a splits visible-operation lookup/query time from the projection loop.
  Visible-cache misses measured 12.27 ms median, 28.23 ms p95, and 37.79 ms
  max versus 0.0036 ms median on hits.
- F-04b caches visible render indices and reuses document-owned immutable
  `Arc<EditOperation>` render sources. Cache misses no longer deep-clone
  visible operations or nested `CompactBlock` sources; focused tests preserve
  paint/layer order and verify shared allocation reuse.
- The ponytail audit removed redundant spatial UUID/layer mirrors, dead
  affected-tile and selection APIs, duplicate visible-cache and
  PNG-compression types, the stale root settings reference, and unused direct
  dependencies.
- Deep-zoom raw wheel diagnostics found short opposite-sign events at a
  stationary pointer. `WheelZoomDirectionLatch` suppresses reversals inside a
  continuous 250 ms burst while preserving raw, produced, and applied values
  for diagnosis. This correction is automatically validated but the migrated
  status still marks its focused manual confirmation as pending.
- Cross-depth projected-point anchors now reset on every depth boundary.
  Same-depth reuse and the independent `OperationBoundsCache` survive. The user
  manually confirmed continuous positive/negative deep navigation, lateral
  pan, immediate line visibility, and curved drawing.
- The ordinary Eraser now snapshots editable Paint targets on pointer down and
  commits width-aware vector subtraction as one append-only history
  transaction. It no longer stores new white Erase operations or recoverable
  Erase drafts; legacy Erase/EraseArea rendering remains compatible.
- Eraser Lasso now shares the pointer-down Paint target snapshot and atomic
  `commit_paint_subtraction` path with the ordinary Eraser. Area selection clips
  the polygon before subtraction; new EraseArea operations are no longer
  created, while legacy EraseArea reopen/render remains compatible.

## Active request

- Request: record manual confirmation of VE-01e, prepare a detailed continuation
  plan, and publish the completed working set to GitHub.
- Result: VE-01e manual GUI confirmation recorded; VE-02 is split into bounded
  geometry, rendering, persistence, tool-wiring, CompactBlock, and optional
  multi-layer checkpoints in `docs/project-implementation-plan.md`.
- Publication status: GitHub authentication was verified through the Windows
  keyring; the completed scope is published from
  `agent/depth-tiles-vector-eraser` toward default branch `Rust` as draft PR
  `#19`.
- Intended Git scope: all tracked implementation/documentation/portable changes
  plus new `AGENTS.md`, excluding protected `status_worker.json`,
  `desktop-shot.png`, `.7z` archives, and `release/EndlessSketch-Rust/local/`.

## Current next checkpoint

Begin VE-02a: implement and focused-test pure polygon difference for Fill,
without persistence or UI changes.

Constraints:

- First represent the result as non-overlapping simple Fill fragments using the
  existing `points` payload.
- Reuse current normalization/triangulation/clipping helpers and add no
  dependency.
- Add a measured fragment-count safety limit; fail without a document change.
- Cover disjoint, crossing, contained hole, concave, winding, tangent,
  degenerate, multiple-mask, and no-op cases.
- Use the explicit decision gate in the plan before adding `area_holes` or any
  format field. Keep persistence, UI, CompactBlock, and `All unlocked layers`
  out of VE-02a.

## Current backlog order

1. VE-02 add Fill polygon difference/replacements, then CompactBlock and
   legacy-erase compatibility using the detailed VE-02a..VE-02f checkpoints.
2. Finish the nearest Selection/Object-transform and Area-selection work in
   short checkpoints; persistent Area selection is a canvas-coordinate clipping
   mask for Brush/Eraser/Fill/Gradient.
3. Input/FPS stabilization before export: ordered multi-stroke event handling,
   latched gestures, wheel accumulation, artificial-stall regressions,
   subsystem timings, and interaction budgets.
4. Selection performance follow-up: hover hit testing, projected overlays,
   egui tessellation, spatial culling, and caching without visual changes.
5. Settings-profile expansion across speed/quality/input-density controls.
6. Shared offscreen renderer, PNG/JPEG still export, camera-path recording, and
   offline animated GIF encoding.
7. Windows stylus pressure capture, per-point persistence, pressure curves, and
   variable-width Brush rendering.
8. Save As with safe SQLite/WAL and asset handling.
9. Image import and asset placement.
10. General configurable hotkeys, Deferred preview guide, installer/signing/
    packaging, and secondary UI polish.

Deferred/future items include layer folders and multi-layer selection, exact
transparency/layer opacity/masks on a shared compositing path, optional
auto-compaction of stable objects, slow-HDD stress diagnosis, zoom FPS
profiling, large raw Fill profiling, optional rounded visual caps at Paint
replacement ends created by vector subtraction, and an exploratory 3D canvas
inspector.

## Latest validation snapshot

Recorded on `2026-07-27` after VE-01e:

- `cargo fmt --all -- --check` passed.
- Focused Paint path/polygon subtraction, multi-fragment Eraser Lasso
  persistence/Undo, ordinary Eraser, Help, legacy EraseArea reopen/raster, and
  non-recoverable Erase-draft tests passed.
- `cargo test --all-targets` passed: 279 tests.
- `cargo clippy --all-targets -- -D warnings` passed.
- `cargo build --release` passed.
- Release executable SHA-256:
  `C9E137E0C4A496F51E343A0155D9A7217F9E9D0B8A1DB6FEF034FDB6E567FE64`.
- Portable executable refreshed from the validated release target; SHA-256
  matches the release executable:
  `C9E137E0C4A496F51E343A0155D9A7217F9E9D0B8A1DB6FEF034FDB6E567FE64`.
- Source and portable Russian/English Help SHA-256 pairs match.
- Release-target tests were not run separately; debug all-target tests passed.
- DT-01 depth capture, corrected DT-02 `distant_tile`, and DT-03 Auto/manual
  coupling, VE-01c ordinary Eraser, and VE-01e Eraser Lasso are manually
  confirmed. DT-02 read-only tools, the focused F-04b paused-tile comparison,
  and million-stroke stress were not run manually.

The migrated JSON contains one stale `gui_smoke` sentence saying the corrected
portable build awaited depth +20/-25 validation. Its newer execution result and
latest notes explicitly record that the user manually confirmed the correction;
this file treats that newer confirmation as authoritative.

## Working tree at migration

The tree already contained user changes before `AGENTS.md` was added:

- Modified: `docs/project-implementation-plan.md`,
  `release/EndlessSketch-Rust/endless-sketch.exe`, `src/app.rs`,
  `src/document.rs`, `src/projection_cache.rs`, `src/session_log.rs`,
  `status_worker.json`.
- Untracked/user-owned: `desktop-shot.png`,
  `release/EndlessSketch-Rust/EndlessSketch-Rust-1.7z`,
  `release/EndlessSketch-Rust/local.7z`,
  `release/EndlessSketch-Rust/local/`.

Preserve these changes and do not stage, overwrite, delete, or commit them
unless the user explicitly places them in scope.
