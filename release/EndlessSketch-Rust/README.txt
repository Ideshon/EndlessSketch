EndlessSketch Rust build

Run:
  endless-sketch.exe

Optional:
  endless-sketch.exe "path\to\document.esketch"

Notes:
  - Documents are stored as .esketch directories.
  - Running without a document path creates or opens local\default.esketch next to this executable.
  - Local app settings are stored in local\settings.json.
  - The toolbar Settings button controls input and interpolated point density (4..1024 px, logarithmic sliders), Fill fallback limits, zoom tile settle timing, and tile worker count.
  - Settings profiles manage only verified controls: Performance = 128 px / 1 worker / 250 ms, Balanced = 512 / 1 / 140, Quality = 1024 / 2 / 80; other combinations are detected as Custom.
  - Fast Fill gestures retain all queued pointer movement events before applying gap interpolation.
  - Tile resolution is selectable as 64, 128, 256, 512, 1024, or 2048 px; logical canvas tile bounds remain unchanged.
  - Pause tile generation stops new tile jobs and invalidates queued generations without pausing vector saves, drafts, undo/redo, or checkpoints.
  - Pause tile generation while drawing independently cancels queued tile work for active Brush/Eraser/Fill drafts and resumes current-view requests after the gesture.
  - Normal commits retain the last loaded visible tiles and overlay only operations newer than the retained snapshots while replacement tiles build; undo/redo clear retained tiles for correctness.
  - Opaque Paint-only deltas update tiles from the retained PNG base; Fill, Eraser, non-opaque Paint, and missing/invalid bases automatically use the full rebuild path.
  - Brush/Eraser strokes are smoothed in cached tile rendering.
  - Dense vector fallback uses raw stored points as one path plus two endpoint caps; final cached tiles keep full smoothing.
  - Full and retained vector fallback use visible-tile spatial culling, so off-screen operations are not projected every frame.
  - Visible operation indices and local f64 projected geometry are cached; same-region pan/zoom uses float translation/scaling, depth transitions rescale cached offsets, and distant movement re-anchors for precision.
  - PNG tiles are rebuildable cache files; vector data is authoritative.
  - Opaque Performance Mode is active: the color picker creates opaque colors and cached strokes use direct per-segment rasterization.
  - Existing alpha remains stored in vector operations, but display colors are flattened over the canvas background until accurate transparency returns.
  - Raster cache version 5 stores each physical resolution in a separate cache path, smooths closed Fill contours, and rebuilds stale PNG tiles automatically.
  - Pending tile results are polled every 50 ms when drawing and zoom are idle, reducing repeated full-vector fallback work.
  - The canvas overlay reports rolling FPS and frame time for active pointer/draft/zoom interaction without adding an idle repaint loop; the dense-canvas target is 60 FPS / 16.7 ms.
  - Fill tiles use cyclic closed-contour smoothing and retain 4x4 edge coverage using supersampled scanline spans.
  - This build was produced from the Rust branch release checkpoint.
