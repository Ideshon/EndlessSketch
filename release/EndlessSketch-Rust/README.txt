EndlessSketch Rust build

Run:
  endless-sketch.exe

Optional:
  endless-sketch.exe "path\to\document.esketch"

Notes:
  - Documents are stored as .esketch directories.
  - File contains New/Open and the current document path. F1 opens Russian/English Help with current controls, workflows, file safety, and detailed performance-setting guidance.
  - Editable translations are stored under help\ beside the executable. A valid additional JSON catalog with a unique id adds another language tab after restart; see help\README.txt.
  - Settings > Display persists Minimal, Standard, Diagnostics, or Custom canvas-overlay fields. The master switch and each Depth, Zoom, coordinate, operation, FPS, status, and tile-state field are independent.
  - Settings > Diagnostics controls JSON Lines session logging in local\logs\session-YYYYMMDD-HHMMSS.jsonl. F12 or Mark log writes a numbered marker; send the marker number, what happened near it, and the .jsonl when reporting FPS/zoom issues. Slow-frame logs split tile work and saved fallback work into projection, derived smoothing/sampling, clipping, and shape-paint phases, including saved fallback cache hit/miss counters.
  - Ctrl+Z performs undo; Ctrl+Y and Ctrl+Shift+Z both perform redo.
  - S opens Selection; repeated S in Object mode cycles Inside/Crossing. Q opens Selection from other tools, and repeated Q while Selection is active switches Object/Area. Object mode selects whole objects; Area mode captures a persistent Rectangle or Lasso contour that follows pan/zoom/depth navigation and clips newly committed Brush/Eraser/Fill/Area Erase operations. X activates Area Fill, then cycles Area Fill/Area Erase; L remains a direct Area Fill shortcut. Releasing a valid Area Erase contour commits one durable vector erase operation, while Escape cancels an unfinished contour.
  - Selection click chooses one whole object, preferring nearer geometry and smaller screen bounds. Each physical Alt+wheel detent cycles exactly one overlapping object without zoom; Alt+vertical drag in Object Selection cycles overlaps for stylus use, down for next and up for previous. Shift+click adds, and Ctrl+click toggles.
  - Eyedropper samples the rendered topmost visible color. Alt+left tap does the same temporary color pick without changing the current tool or adding history.
  - With Select active, the Selection popover contains Object/Area, Inside/Crossing for Object mode, Rectangle/Lasso for Area mode, Deselect, Cut, Copy, Paste, Paste in place, Delete, Flip Horizontal/Vertical, and Recolor selected. Existing keyboard shortcuts are unchanged.
  - With Select active, right-clicking the canvas opens the same Selection commands at the pointer without clearing the selection or starting rectangle, move, scale, or rotate gestures.
  - Palette is a movable and resizable window for the current RGB color; the picker width follows the window width, and the toolbar color swatch opens it. With Brush or Fill active, right-clicking the canvas still opens the current RGB color and shared Size controls at the pointer without creating a stroke, lasso, or history step.
  - Box Inside selects only fully enclosed stroke envelopes or Fill polygons; Box Crossing selects every intersected operation. Green hover previews the click candidate, and the drag frame disappears after release.
  - Delete or Delete selected persistently removes selected whole operations as one undoable UUID tombstone command; original vector payloads remain intact for undo/redo and reopen.
  - With S active, drag an already selected stroke or Fill to move the whole Object selection. Release commits one atomic tombstone/replacement transaction, preserves the original paint order relative to neighboring objects, and keeps the moved copies selected.
  - The Object selection has one bounding box with four square corner handles and one round rotate handle above it. Drag a corner handle without modifiers to scale uniformly around the opposite corner; drag the round handle to rotate around the center, holding Shift for 15-degree snapping. Flip Horizontal/Vertical mirror the selection around the bounds center. Release or Flip commits one atomic replacement transaction, while Escape cancels the live preview.
  - Recolor selected applies the current opaque RGB color to selected Paint/Fill operations as one atomic replacement transaction. Erase and EraseArea objects in the selection stay unchanged; undo/redo and reopen preserve the result.
  - With an Object selection, each Ctrl+wheel detent moves the selected objects one paint-order step inside the active layer: wheel up moves forward and wheel down moves backward. Undo/redo and reopen preserve the order and object identities.
  - Ctrl+C copies the Object selection to the internal clipboard, Ctrl+X cuts it, Ctrl+V pastes into the active layer with a cumulative 16px offset, and Ctrl+Shift+V pastes in place. Cut and Paste are atomic undo/redo steps; pasted objects receive new UUIDs and stay selected.
  - With an Object selection, Layers > Move selection to transfers it to another visible unlocked layer as one undo/redo step. The destination becomes active, and the moved replacements remain selected above its previous content.
  - Area Erase preview exists only during the gesture. A valid released contour erases older content inside the area and round-trips through undo, redo, and reopen; degenerate contours are ignored.
  - Layers lists the stack top-first. Its first checkbox controls visibility and the second controls edit locking. + creates a new top layer, Duplicate copies the active layer and its effective operations with new UUIDs, Delete removes the active layer, Rename changes its name, and Up/Down change render order.
  - Merge Down combines the active layer with its immediate lower visible unlocked layer as one undo/redo step. The lower layer becomes active, and the original two-layer state is restored by Undo.
  - Selection click, rectangle, modifiers, Alt+wheel cycling, and Alt+vertical drag cycling consider only the active visible unlocked layer. Switching the active layer clears the previous selection so Move/Delete cannot cross layer scope.
  - Hidden layers are excluded from tile and vector rendering. Hidden or locked active layers reject drawing, erasing, and Selection. Create/rename/reorder/visibility/lock share one undo/redo timeline with drawing and survive reopen.
  - The final layer cannot be deleted. Deleting a nonempty layer requires confirmation, and Undo restores the layer and its contents as one history step.
  - Moving selected content between layers is not part of this checkpoint.
  - Document schema version 3 unifies drawing and layer metadata history. Opening schema version 1 or 2 creates a verified backups\migrations\pre-schema-v3-*.sqlite3 copy before migration.
  - Running without a document path creates or opens local\default.esketch next to this executable.
  - Local app settings are stored in local\settings.json.
  - Brush input is 0.75..8 px (default 3) and Fill input is 1..4 px (default 2); these control accepted pointer sample density only.
  - Navigation shows current depth, zoom, exact Tile X/Y, and Local X/Y. It jumps directly to depth -10000..10000 with Go or Enter while preserving the visible center and cancelling stale tile jobs.
  - Navigation jumps to absolute BigInt tile X/Y at the current depth with Go XY or Enter. Decimal and scientific integer notation such as 1e100 are supported up to 10000 expanded digits; Origin returns to tile 0/0.
  - Full extreme BigInt coordinates are horizontally scrollable and selectable in Navigation, with a compact form such as 1e1000 shown as an orientation aid.
  - Bookmarks is available from both the main toolbar and Navigation. Add stores the current position from its name field; Open, Rename, and Delete remain in the same window.
  - Interpolation gaps remain fixed at 8 px for Brush/Eraser and 4 px for Fill, and the final release endpoint is always preserved.
  - On Windows, active drafts recover up to 64 system mouse-history samples between UI frames before spacing/interpolation; regular egui events remain the automatic fallback.
  - Minimum Brush input with Smoothing Off intentionally exposes every integer mouse sample and can produce sharp raw vector joins; use a higher input value or enabled Smoothing for normal drawing.
  - Settings profiles manage only verified controls: Performance = 128 px / 1 worker / 250 ms, Balanced = 512 / 1 / 140, Quality = 1024 / 2 / 80; other combinations are detected as Custom. Rebuild and prefetch remain independent.
  - Fast Fill gestures retain recovered and regular pointer movement events before applying gap interpolation.
  - Tile resolution is selectable as 64, 128, 256, 512, 1024, or 2048 px; logical canvas tile bounds remain unchanged.
  - Pause tile generation stops new tile jobs and invalidates queued generations without pausing vector saves, drafts, undo/redo, or checkpoints.
  - Pause tile generation while drawing independently cancels queued tile work for active Brush/Eraser/Fill drafts and resumes current-view requests after the gesture.
  - Deferred drawing preview is an optional weak-CPU mode: input collection, interpolation, and draft autosave continue, but projection, smoothing, and tessellation of the growing draft wait until release.
  - Rebuild policy can remain Immediate or wait for 180 ms of idle time after pointer interaction before requesting tiles.
  - Prefetch tiles selects a 0..2 tile radius; visible tiles are queued before prefetch rings, and missing off-screen tiles do not force full vector fallback.
  - Edge quality selects Performance, Balanced, or Quality raster coverage; Balanced preserves the previous behavior.
  - Smoothing selects Off, Light, Balanced, or Strong. Enabled levels first remove small mouse-history jitter within 0.45, 0.9, or 1.5 px, then apply adaptive quadratic curves in live drafts, saved fallback, and cached tiles.
  - The error-bounded jitter prefilter uses windows of at most 64 points or 32 px, never increases point count, and leaves Off exactly raw.
  - Straight sections and small direction changes are left raw, so dense stylus input changes minimally; stored points remain unchanged.
  - Saved no-tile fallback keeps point order stable during zoom/depth navigation: Brush is smoothed before clipping, saved Fill uses a derived fallback representation within Fill fallback points without screen-space simplification, and smoothing above 4096 input or 8192 output points stays raw to protect interaction FPS.
  - Every edge/smoothing combination uses a separate PNG cache namespace so incompatible tiles are never reused.
  - Cache size is configurable from 128 to 8192 MiB; periodic cleanup removes the oldest PNG files.
  - PNG compression selects Fast, Balanced, or Small for newly written tiles without changing pixels.
  - Preview FPS is configurable from 15 to 120 for active fallback repaint; pending tile polling remains capped at a 50 ms minimum interval.
  - Normal commits retain the last loaded visible tiles and overlay only operations newer than the retained snapshots while replacement tiles build; undo/redo clear retained tiles for correctness.
  - Undo/redo decode only the changed transaction group and update in-memory operations/spatial entries incrementally instead of reloading every document payload.
  - Opaque Paint-only deltas update tiles from the retained PNG base; Fill, Eraser, non-opaque Paint, and missing/invalid bases automatically use the full rebuild path.
  - Brush/Eraser strokes are smoothed in cached tile rendering.
  - Stroke fallback joins controls only temporary no-tile vector fallback: Auto preserves capsule joins and saved smoothing for short sparse saved strokes while viewing, but uses the faster raw polyline path without endpoint caps for background saved strokes during active drawing or tile waiting; Quality keeps capsule joins longer, and Performance always uses the fast raw path.
  - Fast dense vector fallback uses one path; Quality/calm fallback keeps the higher-quality saved appearance while final cached tiles keep full smoothing.
  - Full and retained vector fallback use visible-tile spatial culling, so off-screen operations are not projected every frame.
  - Multi-tile spatial queries group targets by depth and reuse each BigInt depth scale, reducing repeated work across extreme depth ranges.
  - Same-depth drawing/storage is covered at depth +/-1000; vector geometry beyond the representable f64 cross-depth scale is safely skipped and projection caches re-anchor.
  - BigInt navigation, projection cache, spatial queries, and storage are covered at lateral coordinates around 10^1000.
  - No-tile vector fallback no longer simplifies saved geometry with a 0.25 px screen tolerance, so distant objects do not change shape just because zoom/depth changes; stored operations, live drafts, and cached tiles are unchanged.
  - Visible operation indices and local f64 projected geometry are cached; same-region pan/zoom uses float translation/scaling, depth transitions rescale cached offsets, and distant movement re-anchors for precision.
  - PNG tiles are rebuildable cache files; vector data is authoritative.
  - Opaque Performance Mode is active: the color picker creates opaque colors and cached strokes use direct per-segment rasterization.
  - Existing alpha remains stored in vector operations, but display colors are flattened over the canvas background until accurate transparency returns.
  - Raster cache version 8 stores each physical resolution and render-quality combination in a separate cache path, clips dense geometry, prefilters small jitter, uses adaptive curves, and rebuilds stale PNG tiles automatically.
  - Pending tile results are polled every 50 ms when drawing and zoom are idle, reducing repeated full-vector fallback work.
  - The canvas overlay reports rolling FPS and frame time for active pointer/draft/zoom interaction without adding an idle repaint loop; the dense-canvas target is 60 FPS / 16.7 ms.
  - Fill tiles use cyclic closed-contour smoothing and retain 4x4 edge coverage using supersampled scanline spans.
  - This build was produced from the Rust branch release checkpoint.
