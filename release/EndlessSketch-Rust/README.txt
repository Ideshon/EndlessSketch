EndlessSketch Rust build

Run:
  endless-sketch.exe

Optional:
  endless-sketch.exe "path\to\document.esketch"

Notes:
  - Documents are stored as .esketch directories.
  - Running without a document path creates or opens local\default.esketch next to this executable.
  - Local app settings are stored in local\settings.json.
  - Brush input is 0.75..8 px (default 3) and Fill input is 1..4 px (default 2); these control accepted pointer sample density only.
  - The second toolbar row jumps directly to depth -10000..10000 with Go or Enter while preserving the visible center and cancelling stale tile jobs.
  - The third toolbar row jumps to absolute BigInt tile X/Y at the current depth with Go XY or Enter. Decimal and scientific integer notation such as 1e100 are supported up to 10000 expanded digits; Origin returns to tile 0/0.
  - The canvas overlay continuously displays camera-center tile X/Y and normalized local X/Y. Extreme BigInt values are abbreviated, and their formatted text is cached until the camera crosses a tile boundary.
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
  - Saved fallback smoothing is bounded: Brush runs are clipped first, and geometry above 4096 input or 8192 output points stays raw to protect interaction FPS.
  - Every edge/smoothing combination uses a separate PNG cache namespace so incompatible tiles are never reused.
  - Cache size is configurable from 128 to 8192 MiB; periodic cleanup removes the oldest PNG files.
  - PNG compression selects Fast, Balanced, or Small for newly written tiles without changing pixels.
  - Preview FPS is configurable from 15 to 120 for active fallback repaint; pending tile polling remains capped at a 50 ms minimum interval.
  - Normal commits retain the last loaded visible tiles and overlay only operations newer than the retained snapshots while replacement tiles build; undo/redo clear retained tiles for correctness.
  - Opaque Paint-only deltas update tiles from the retained PNG base; Fill, Eraser, non-opaque Paint, and missing/invalid bases automatically use the full rebuild path.
  - Brush/Eraser strokes are smoothed in cached tile rendering.
  - Dense vector fallback uses one path plus two endpoint caps; over-budget geometry stays raw while final cached tiles keep full smoothing.
  - Full and retained vector fallback use visible-tile spatial culling, so off-screen operations are not projected every frame.
  - Multi-tile spatial queries group targets by depth and reuse each BigInt depth scale, reducing repeated work across extreme depth ranges.
  - Same-depth drawing/storage is covered at depth +/-1000; vector geometry beyond the representable f64 cross-depth scale is safely skipped and projection caches re-anchor.
  - BigInt navigation, projection cache, spatial queries, and storage are covered at lateral coordinates around 10^1000.
  - Saved geometry above 256 projected points is simplified with a 0.25 px render-only tolerance and clipped to screen/tile bounds; stored operations and live drafts are unchanged.
  - Visible operation indices and local f64 projected geometry are cached; same-region pan/zoom uses float translation/scaling, depth transitions rescale cached offsets, and distant movement re-anchors for precision.
  - PNG tiles are rebuildable cache files; vector data is authoritative.
  - Opaque Performance Mode is active: the color picker creates opaque colors and cached strokes use direct per-segment rasterization.
  - Existing alpha remains stored in vector operations, but display colors are flattened over the canvas background until accurate transparency returns.
  - Raster cache version 8 stores each physical resolution and render-quality combination in a separate cache path, clips dense geometry, prefilters small jitter, uses adaptive curves, and rebuilds stale PNG tiles automatically.
  - Pending tile results are polled every 50 ms when drawing and zoom are idle, reducing repeated full-vector fallback work.
  - The canvas overlay reports rolling FPS and frame time for active pointer/draft/zoom interaction without adding an idle repaint loop; the dense-canvas target is 60 FPS / 16.7 ms.
  - Fill tiles use cyclic closed-contour smoothing and retain 4x4 edge coverage using supersampled scanline spans.
  - This build was produced from the Rust branch release checkpoint.
