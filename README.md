<img width="818" height="440" alt="EndlessSketch_101902_1342" src="https://github.com/user-attachments/assets/a060edf8-ba10-4036-8d44-b97f9bc50a44" />

# EndlessSketch

EndlessSketch is a Windows application written in Rust for drawing on a canvas that is virtually unlimited in distance and depth.

## How the canvas works

The camera uses hierarchical depth levels, BigInt tile addresses, and local normalized coordinates. There is no global rectangular scene or coordinate limit.

The source data is stored as compressed vector operations in SQLite/WAL. PNG tiles use a selectable physical resolution from 64 to 2048 px and remain a disposable, automatically rebuilt cache.

When changing the depth, the app continues to show vectors until the entire visible area of a single revision is ready. An incomplete cache cannot replace a frame.

An opaque brush and eraser cover all older levels. Newer strokes remain visible; undo/redo is saved between launches.

The stroke draft is saved every 500 ms. SQLite is checked at checkpoint; two checked backups are stored.

The new document is a name.esketch directory with manifest.json, canvas.sqlite3, backups/, assets/, and a recoverable cache/. Old .ess/.esp files are not imported or modified.

## Management

- B — brush, E — eraser, L — lasso fill, I — eyedropper.

- Mouse wheel / Z+LMB — zoom relative to the cursor.

- Middle mouse button / spacebar + LMB — move.

- Ctrl+Z / Ctrl+Y — undo/redo.

- Shift+LMB draws a straight Brush/Eraser line; Ctrl+LMB drag changes brush size.

- The top panel contains color, brush size, document controls, bookmarks, and performance settings.

## Build and run

Requires Rust stable MSVC and Visual Studio Build Tools with C++ workload.

```powershell

cargo run --release

cargo run --release -- "D:\Drawings\canvas.esketch"

```

Checks:

```powershell

cargo fmt -- --check

cargo clippy --all-targets -- -D warnings

cargo test --all-targets

cargo run --release --bin stress -- 1000000

```

The stress profile builds and queries a hierarchical index of a million synthetic strokes distributed over 12 levels of depth.

# EndlessSketch

EndlessSketch — Windows-приложение на Rust для рисования на практически неограниченном по расстоянию и глубине холсте.

## Как устроен холст

- Камера использует иерархические уровни глубины, `BigInt`-адреса тайлов и локальные нормализованные координаты. Глобальной прямоугольной сцены и предела координат нет.
- Исходные данные — сжатые векторные операции в SQLite/WAL. Физическое разрешение PNG-тайлов выбирается в Settings: `64/128/256/512/1024/2048 px`; логическая область тайла на холсте не меняется. Тайлы являются только удаляемым и автоматически восстанавливаемым кэшем.
- Версия растеризатора входит в имя PNG-кэша: после изменения алгоритма старые тайлы автоматически игнорируются и перестраиваются. Текущий Opaque Performance Mode использует прямую растеризацию сегментов без буфера прозрачного покрытия.
- Fill использует циклическое сглаживание замкнутого контура и сглаживание границ 4×4, но строится по supersampled scanline spans вместо полного обхода всех точек полигона для каждого субпикселя.
- При смене глубины приложение продолжает показывать векторы, пока не готова вся видимая область одной ревизии. После обычного сохранения последний готовый видимый тайл остаётся на экране, а поверх временно рисуются только операции с более новым `sequence`. Undo/redo очищают retained-тайлы для полного безопасного пересчёта.
- Для последовательных непрозрачных Paint сохранений текущий PNG строится инкрементально из последнего готового PNG плюс новых Paint-операций. Fill, Eraser, non-opaque Paint, отсутствующий или несовместимый base PNG автоматически используют полный rebuild.
- Непрозрачная кисть и ластик перекрывают все более старые уровни. Более новые штрихи остаются видимыми; undo/redo сохраняется между запусками.
- Черновик штриха сохраняется каждые 500 мс. SQLite проверяется при checkpoint; хранятся две проверенные резервные копии.

Новый документ — каталог `name.esketch` с `manifest.json`, `canvas.sqlite3`, `backups/`, `assets/` и восстанавливаемым `cache/`. Старые `.ess/.esp` не импортируются и не изменяются.

## Управление

- `B` — кисть, `E` — ластик, `L` — заливка лассо, `I` — пипетка.
- Колесо мыши или `Z` + ЛКМ — масштабирование относительно курсора.
- Средняя кнопка мыши или `Space` + ЛКМ — перемещение.
- Клик кистью без движения — точка текущего размера и цвета.
- `Shift` + левая кнопка мыши — прямая линия кистью или ластиком.
- `Ctrl` + левая кнопка мыши — изменение размера кисти.
- `Ctrl+Z` / `Ctrl+Y` — undo/redo.
- Панель сверху содержит цвет, размер кисти, открытие/создание документов, закладки текущей позиции и Settings.
- Color picker сейчас редактирует только RGB и создаёт непрозрачные операции. Alpha старых операций сохраняется в документе, но при отображении цвет сводится с фоном и обрабатывается как opaque; точная прозрачность отложена.

## Сборка и запуск

Требуются Rust stable MSVC и Visual Studio Build Tools с C++ workload.

```powershell
cargo run --release
cargo run --release -- "D:\Drawings\canvas.esketch"
```

При запуске без пути к документу приложение создаёт или открывает стандартный холст в `local/default.esketch` рядом с исполняемым файлом. Локальные настройки производительности/качества хранятся в `local/settings.json`.

Settings содержит профили проверенных параметров:

- `Performance`: tile resolution `128 px`, `1` worker, zoom settle `250 ms`.
- `Balanced`: `512 px`, `1` worker, `140 ms`.
- `Quality`: `1024 px`, `2` workers, `80 ms`.
- `Custom` определяется автоматически после ручного изменения любого из этих трёх параметров.

Профили не меняют spacing, Fill limits или `Pause tile generation`; выбранные значения сохраняются в обычном settings JSON, поэтому профиль восстанавливается после перезапуска без отдельного поля.

В Settings параметры `Brush spacing px` и `Fill spacing px` управляют плотностью входных и интерполированных точек stroke/lasso в диапазоне `4..1024 px`: входные точки прореживаются с порогом в четверть выбранного spacing, а большие разрывы делятся на отрезки не длиннее выбранного spacing. Меньшее значение точнее сохраняет жест, большее заметно упрощает геометрию. Ползунки используют логарифмическую шкалу.
Во время быстрого Fill приложение обрабатывает все накопленные события движения указателя по порядку, а интерполяция заполняет только оставшиеся большие промежутки.
Brush/Eraser strokes сглаживаются в готовых PNG-тайлах. Временный vector fallback использует исходные точки, один polyline path и только две endpoint caps, чтобы сохранять FPS на насыщенных участках.
Полный и retained vector fallback используют тот же spatial index, что и tile rebuild: проецируются только операции, пересекающие набор видимых логических тайлов, с дедупликацией и сохранением исходного порядка.
Набор видимых operation indices кэшируется по ревизии и TileKey-набору. Точки fallback также кэшируются как локальные `f64` offsets относительно camera anchor: pan использует общий сдвиг, zoom — масштаб, а переход depth переносит кэш умножением/делением на коэффициент глубины. После смещения более чем на 8 тайлов anchor пересоздаётся для сохранения точности.
`Tile workers` управляет числом потоков перестройки тайлов; по умолчанию используется `1`, чтобы сохранение и rebuild на тяжелых документах меньше грузили систему. Пока фоновые тайлы ожидаются без активного рисования или zoom, интерфейс проверяет их с интервалом 50 мс вместо постоянной перерисовки 60 FPS.
`Tile resolution` задаёт точный размер bitmap каждого тайла. `64–256 px` ускоряют перестройку ценой пикселизации, `512 px` используется по умолчанию, `1024–2048 px` повышают качество и нагрузку.
`Pause tile generation` останавливает новые tile jobs и отменяет queued generations, не останавливая сохранение векторных операций, drafts, undo/redo и checkpoints. После возобновления запрашивается только текущая видимая область.
`Pause tile generation while drawing` автоматически отменяет queued tile generations после начала Brush/Eraser/Fill draft и возобновляет запросы текущей области после завершения жеста. Эта настройка не включает и не выключает постоянную ручную паузу.
При паузе уже загруженные тайлы остаются видимыми после новых штрихов, а vector fallback рисует только операции новее сохранённого снимка тайла.
В простое приложение не запускает постоянный repaint loop; периодический repaint включается только для активного черновика, генерации тайлов или zoom settle.
Canvas overlay показывает скользящие `fps` и frame time только для активного pointer/draft/zoom взаимодействия. После idle новое действие начинает отдельное окно измерения; текущая цель для насыщенной depth 0 — `60 FPS` (`16.7 ms`).

Проверки:

```powershell
cargo fmt -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
cargo run --release --bin stress -- 1000000
```

Стресс-профиль строит и запрашивает иерархический индекс миллиона синтетических штрихов, распределённых по 12 уровням глубины.
