<img width="1280" height="777" alt="EndlessSketch_100202_1457" src="https://github.com/user-attachments/assets/34f585ee-f25b-4be6-a48d-2a74782eb9ef" />

# EndlessSketch

EndlessSketch — экспериментальное Windows-приложение для рисования на практически бесконечном холсте. Проект написан на Rust и использует `eframe/egui`, `wgpu`, SQLite/WAL и перестраиваемый PNG-кэш тайлов.

The English description is below.

## Описание

EndlessSketch хранит рисунок не как одну большую картинку, а как последовательность сжатых векторных операций. Камера работает с иерархической глубиной, `BigInt`-адресами тайлов и локальными координатами, поэтому у холста нет обычной прямоугольной границы и практического лимита по расстоянию.

PNG-тайлы являются только кэшем: их можно удалить и перестроить из векторного источника. Пока тайлы не готовы или генерация отключена, приложение показывает no-tile vector fallback, чтобы рисунок оставался видимым во время навигации и рисования.

Документ хранится как папка `.esketch` с `manifest.json`, `canvas.sqlite3`, `backups/`, `assets/` и восстанавливаемым `cache/`. Старые `.ess/.esp` файлы не импортируются и не изменяются.

## Основные функции

- Практически бесконечная навигация по глубине и координатам, включая прямой переход к `BigInt` Tile X/Y.
- Brush, Eraser, Lasso Fill, Eraser Lasso, Eyedropper.
- Object Selection: клик, rectangle Inside/Crossing, overlap cycling, move, scale, rotate, flip, copy/cut/paste/delete, reorder и перенос на слой.
- Area Selection: временная rectangle/lasso-маска, которая следует за pan/zoom/depth и обрезает новые Brush/Eraser/Fill/Area Erase операции при сохранении.
- Слои: создание, переименование, видимость, блокировка, порядок, duplicate/delete, merge down и перенос выделения между слоями.
- Undo/redo для рисования и metadata-операций в общей истории.
- Recolor выбранных Paint/Fill операций без изменения геометрии.
- CompactBlock/Freeze для упаковки старых объектов с сохранением векторного источника.
- Настройки производительности: профили Performance/Balanced/Quality, разрешение тайлов, workers, rebuild policy, prefetch, edge quality, smoothing, PNG compression, cache budget, fallback joins и storage commit mode.
- Диагностика FPS: JSON Lines session logs, маркеры `F12`, timings фаз кадра, счётчики fallback projection/cache/paint.
- Portable-сборка в `release/EndlessSketch-Rust`.

## Управление

- `B` — Brush, `E` — Eraser, `L` — Lasso Fill, `I` — Eyedropper.
- `X` — Area Fill, повторное `X` переключает Area Fill / Area Erase.
- `S` — Selection; повторное `S` в Object mode переключает Inside/Crossing.
- `Q` — Selection из других инструментов; повторное `Q` переключает Object/Area.
- Колесо мыши или `Z + ЛКМ` — zoom относительно курсора.
- Средняя кнопка мыши или `Space + ЛКМ` — перемещение холста.
- `Shift + ЛКМ` — прямая линия Brush/Eraser.
- `Ctrl + ЛКМ drag` — изменение размера кисти.
- `Alt + ЛКМ` — быстрый eyedropper без смены инструмента.
- `Alt + колесо` или `Alt + vertical drag` в Object Selection — перебор объектов под курсором.
- `Ctrl + колесо` при активном Object Selection — изменение paint order.
- `Ctrl+Z` — undo; `Ctrl+Y` или `Ctrl+Shift+Z` — redo.
- `F1` — справка; `F12` — маркер в session log.

## Настройки и данные

Настройки сохраняются рядом с приложением в `local/settings.json`. Документы `.esketch` содержат авторитетные векторные операции, а PNG-тайлы остаются перестраиваемым кэшем.

Для тяжёлых сцен полезны:

- `Performance profile`: быстро переключает большую часть speed/quality параметров.
- `Pause tile generation`: останавливает фоновые tile jobs, не мешая сохранению векторов.
- `Stroke fallback joins`: управляет качеством no-tile fallback joins.
- `Saved fallback operation limit`: может ограничить стоимость временного fallback в режиме Performance.
- `Storage commit`: `Full` для максимальной надёжности, `Fast` для меньшей задержки dense edit commits.
- `Settings > Diagnostics`: включает session logs для анализа FPS и задержек ввода.

## Сборка

Требуется Rust stable MSVC и Visual Studio Build Tools с C++ workload.

```powershell
cargo run --release
cargo run --release -- "D:\Drawings\canvas.esketch"
```

Проверки:

```powershell
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
cargo run --release --bin stress -- 1000000 100
```

Второй аргумент stress-теста задаёт симметричный радиус глубины. Пример выше создаёт миллион синтетических штрихов на 201 уровне (`-100..+100`) и измеряет spatial queries.

---

# EndlessSketch

EndlessSketch is an experimental Windows drawing application for a practically endless canvas. It is written in Rust and uses `eframe/egui`, `wgpu`, SQLite/WAL, and a rebuildable PNG tile cache.

## Overview

EndlessSketch does not store the canvas as one large bitmap. The authoritative document is a compressed vector operation log. The camera uses hierarchical depth levels, `BigInt` tile addresses, and local normalized coordinates, so there is no ordinary rectangular scene boundary or practical coordinate limit.

PNG tiles are disposable cache data. They can be deleted and rebuilt from the vector source. While tiles are missing, outdated, or paused, the app uses a no-tile vector fallback so the drawing remains visible during navigation and drawing.

A document is a `.esketch` directory with `manifest.json`, `canvas.sqlite3`, `backups/`, `assets/`, and a recoverable `cache/`. Legacy `.ess/.esp` files are not imported or modified.

## Main Features

- Practically endless navigation across depth and coordinates, including direct jumps to `BigInt` Tile X/Y.
- Brush, Eraser, Lasso Fill, Eraser Lasso, and Eyedropper.
- Object Selection: click, rectangle Inside/Crossing, overlap cycling, move, scale, rotate, flip, copy/cut/paste/delete, reorder, and move to layer.
- Area Selection: a temporary rectangle/lasso mask that follows pan/zoom/depth and clips newly committed Brush/Eraser/Fill/Area Erase operations.
- Layers: create, rename, visibility, lock, ordering, duplicate/delete, merge down, and moving a selection between layers.
- Unified undo/redo for drawing and layer/object metadata.
- Recolor selected Paint/Fill operations without changing geometry.
- CompactBlock/Freeze for packing older objects while preserving vector source data.
- Performance settings: Performance/Balanced/Quality profiles, tile resolution, worker count, rebuild policy, prefetch, edge quality, smoothing, PNG compression, cache budget, fallback joins, and storage commit mode.
- FPS diagnostics: JSON Lines session logs, `F12` markers, frame phase timings, and fallback projection/cache/paint counters.
- Portable build under `release/EndlessSketch-Rust`.

## Controls

- `B` — Brush, `E` — Eraser, `L` — Lasso Fill, `I` — Eyedropper.
- `X` — Area Fill; press `X` again to cycle Area Fill / Area Erase.
- `S` — Selection; pressing `S` again in Object mode cycles Inside/Crossing.
- `Q` — Selection from other tools; pressing `Q` again switches Object/Area.
- Mouse wheel or `Z + LMB` — zoom around the cursor.
- Middle mouse button or `Space + LMB` — pan.
- `Shift + LMB` — straight Brush/Eraser line.
- `Ctrl + LMB drag` — brush size adjustment.
- `Alt + LMB` — quick eyedropper without changing the active tool.
- `Alt + wheel` or `Alt + vertical drag` in Object Selection — cycle overlapping objects.
- `Ctrl + wheel` with an active Object Selection — change paint order.
- `Ctrl+Z` — undo; `Ctrl+Y` or `Ctrl+Shift+Z` — redo.
- `F1` — help; `F12` — session log marker.

## Settings and Data Safety

Settings are stored next to the executable in `local/settings.json`. `.esketch` documents keep the authoritative vector operations, while PNG tiles are rebuildable cache data.

Useful settings for heavy scenes:

- `Performance profile`: quickly changes most speed/quality settings.
- `Pause tile generation`: stops background tile jobs without blocking vector saves.
- `Stroke fallback joins`: controls no-tile fallback join quality.
- `Saved fallback operation limit`: can cap temporary fallback cost in Performance mode.
- `Storage commit`: `Full` for maximum durability, `Fast` for lower dense-edit commit latency.
- `Settings > Diagnostics`: enables session logs for FPS and input-latency analysis.

## Build

Requires Rust stable MSVC and Visual Studio Build Tools with the C++ workload.

```powershell
cargo run --release
cargo run --release -- "D:\Drawings\canvas.esketch"
```

Checks:

```powershell
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
cargo run --release --bin stress -- 1000000 100
```

The optional second stress argument is a symmetric depth radius. The example creates one million synthetic strokes across 201 depth levels (`-100..+100`) and measures spatial queries.
