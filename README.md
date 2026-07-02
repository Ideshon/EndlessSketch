<img width="818" height="440" alt="EndlessSketch_101902_1342" src="https://github.com/user-attachments/assets/a060edf8-ba10-4036-8d44-b97f9bc50a44" />

# EndlessSketch

EndlessSketch is a Windows application written in Rust for drawing on a canvas that is virtually unlimited in distance and depth.

## How the canvas works

The camera uses hierarchical depth levels, BigInt tile addresses, and local normalized coordinates. There is no global rectangular scene or coordinate limit.

The source data is stored as compressed vector operations in SQLite/WAL. Schema version 3 adds one undo/redo timeline for drawing and layer metadata; opening an older document creates a verified pre-migration backup. PNG tiles use a selectable physical resolution from 64 to 2048 px and remain a disposable, automatically rebuilt cache.

When changing the depth, the app continues to show vectors until the entire visible area of a single revision is ready. An incomplete cache cannot replace a frame.

An opaque brush and eraser cover all older levels. Newer strokes remain visible; undo/redo is saved between launches.

The stroke draft is saved every 500 ms. SQLite is checked at checkpoint; two checked backups are stored.

The new document is a name.esketch directory with manifest.json, canvas.sqlite3, backups/, assets/, and a recoverable cache/. Old .ess/.esp files are not imported or modified.

## Management

- B — brush, E — eraser, L — lasso fill, I — eyedropper.

- Mouse wheel / Z+LMB — zoom relative to the cursor.

- Middle mouse button / spacebar + LMB — move.

- Ctrl+Z — undo; Ctrl+Y or Ctrl+Shift+Z — redo.

- S selects whole objects. Its Selection popover contains Inside/Crossing and Cut/Copy/Paste/Delete; the same clipboard shortcuts remain available. With a selection, Ctrl+wheel changes its paint order; the Layers window can move it to another visible unlocked layer.

- Shift+LMB draws a straight Brush/Eraser line; Ctrl+LMB drag changes brush size.

- Navigation shows current depth, zoom, exact Tile X/Y and Local X/Y. Its depth target accepts -10000 to 10000; press Go or Enter to jump while preserving the visible center.

- Navigation also accepts absolute BigInt tile X/Y coordinates at the current depth. Decimal and scientific integer notation such as `1e100` are supported; press Go XY or Enter to jump, or Origin to return to tile 0/0.

- Exact extreme BigInt coordinates are horizontally scrollable and selectable in Navigation; a compact form such as `1e1000` is shown as an orientation aid.

- The top panel contains tools, color, brush size, history, and buttons for File, Navigation, Bookmarks, Settings, and Layers. Navigation also opens Bookmarks. The Layers window can move a selection between layers and merge the active layer into its immediate lower layer.

- File opens the New/Open utility window. F1 opens its Help tab with Russian/English instructions for tools, gestures, selection, layers, navigation, files, and every current performance setting. Editable `help/*.json` catalogs are loaded beside the executable; another valid catalog adds a language tab without recompiling.

- Settings > Display controls the canvas overlay. Minimal, Standard, and Diagnostics presets plus independent Depth, Zoom, coordinates, operation count, performance, status, and tile-state switches persist in `local/settings.json`.

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

cargo run --release --bin stress -- 1000000 100

```

The optional second stress argument is a symmetric depth radius. The example builds a million synthetic strokes across 201 bands (`-100..+100`) and measures both individual tile queries and batched viewport queries at both edges and zero.

# EndlessSketch

EndlessSketch — Windows-приложение на Rust для рисования на практически неограниченном по расстоянию и глубине холсте.

## Как устроен холст

- Камера использует иерархические уровни глубины, `BigInt`-адреса тайлов и локальные нормализованные координаты. Глобальной прямоугольной сцены и предела координат нет.
- Исходные данные — сжатые векторные операции в SQLite/WAL. Schema version 3 объединяет историю рисования и метаданных слоёв. Перед автоматической миграцией schema v1/v2 создаётся проверенная копия в `backups/migrations/`. Физическое разрешение PNG-тайлов выбирается в Settings: `64/128/256/512/1024/2048 px`; логическая область тайла на холсте не меняется. Тайлы являются только удаляемым и автоматически восстанавливаемым кэшем.
- Версия растеризатора входит в имя PNG-кэша: после изменения алгоритма старые тайлы автоматически игнорируются и перестраиваются. Текущий Opaque Performance Mode использует прямую растеризацию сегментов без буфера прозрачного покрытия.
- Fill использует циклическое сглаживание замкнутого контура и сглаживание границ 4×4, но строится по supersampled scanline spans вместо полного обхода всех точек полигона для каждого субпикселя.
- При смене глубины приложение продолжает показывать векторы, пока не готова вся видимая область одной ревизии. После обычного сохранения последний готовый видимый тайл остаётся на экране, а поверх временно рисуются только операции с более новым `sequence`. Undo/redo очищают retained-тайлы для полного безопасного пересчёта.
- Undo/redo загружает из SQLite только изменённую transaction group: undo удаляет её из памяти/spatial index, redo добавляет обратно без повторной распаковки всего документа.
- Для последовательных непрозрачных Paint сохранений текущий PNG строится инкрементально из последнего готового PNG плюс новых Paint-операций. Fill, Eraser, non-opaque Paint, отсутствующий или несовместимый base PNG автоматически используют полный rebuild.
- Сохранённая геометрия плотнее 256 экранных точек временно упрощается с допуском `0.25 px` и обрезается по границам экрана/тайла. Исходные операции не изменяются. Raster cache version 8 отделяет jitter prefilter и адаптивные кривые от старых алгоритмов.
- Spatial query группирует видимые tile keys по target depth и вычисляет BigInt-масштаб один раз для каждой пары target/source depth. Это уменьшает повторную работу при больших диапазонах глубины без изменения результатов.
- Same-depth координаты и сохранение проверяются на depth `±1000`. Если разница native/camera depth превышает representable `f64` scale, vector projection безопасно пропускает такую геометрию, а projection cache сбрасывает старый anchor; невалидные координаты в egui не передаются.
- BigInt tile navigation, projection cache, spatial query и сохранение автоматически проверяются на боковых координатах порядка `10^1000`.
- Непрозрачная кисть и ластик перекрывают все более старые уровни. Более новые штрихи остаются видимыми; undo/redo сохраняется между запусками.
- Черновик штриха сохраняется каждые 500 мс. SQLite проверяется при checkpoint; хранятся две проверенные резервные копии.

Новый документ — каталог `name.esketch` с `manifest.json`, `canvas.sqlite3`, `backups/`, `assets/` и восстанавливаемым `cache/`. Старые `.ess/.esp` не импортируются и не изменяются.

При прямом переходе через очень грубый отрицательный depth видимый центр сохраняется на целевом уровне, но возврат произвольной тонкой позиции не гарантируется бит-в-бит из-за `f64` local coordinates. Для точного возврата сохраните bookmark перед переходом.

## Управление

- `B` — кисть, `E` — ластик, `L` — заливка лассо, `I` — пипетка, `S` — прямоугольное выделение, `X` — стирающее лассо.
- Rectangle Selection временно выделяет целые векторные операции только в active layer по режиму `Inside` или `Crossing`; `Esc` снимает выделение. Выделение не записывается в документ.
- В Selection обычный клик выбирает один целый объект; зелёный hover показывает будущий выбор. При перекрытии приоритет получает ближайший объект с меньшими экранными границами, поэтому маленькую линию можно выбрать поверх длинной. `Alt+клик` перебирает объекты под курсором, `Alt+колесо вниз/вверх` перебирает их в прямом/обратном направлении: одно физическое деление колеса переключает один объект без zoom. `Shift+клик` добавляет, `Ctrl+клик` переключает объект.
- При активном Selection ПКМ на холсте открывает возле курсора то же меню `Inside/Crossing`, Cut/Copy/Paste/Paste in place/Delete, что и кнопка `Selection` в toolbar. Открытие меню сохраняет текущее выделение и не запускает rectangle, Move или Scale.
- При активном Brush или Fill ПКМ открывает возле курсора текущую RGB-палитру и общий `Size`. Они изменяют те же значения, что toolbar; открытие и настройка меню не создают stroke, lasso или history step.
- `Ctrl+C` копирует Object selection во внутренний clipboard, `Ctrl+X` вырезает, `Ctrl+V` вставляет в active layer со смещением 16 px, `Ctrl+Shift+V` вставляет на прежнее место. Каждая следующая обычная вставка увеличивает offset ещё на 16 px. Cut и Paste являются отдельными undo/redo steps; вставленные объекты получают новые UUID и остаются выделенными. Clipboard очищается при смене документа.
- Режим рамки `Inside` выбирает только полностью заключённые операции с учётом толщины stroke. `Crossing` выбирает все операции, которых касается рамка. После release рамка исчезает, остаётся только подсветка выбранных объектов.
- `Delete` или кнопка `Delete selected` удаляет выбранные операции одной сохраняемой командой. Исходные векторы не перезаписываются; undo/redo и reopen восстанавливают состояние через UUID tombstone.
- Чтобы переместить Object selection, потяните инструментом `S` за любую выделенную линию или внутреннюю область выделенного Fill. Во время drag показывается preview; на release исходные UUID заменяются перемещёнными векторными копиями одной атомарной transaction. Копии сохраняют исходный порядок рисования относительно соседних объектов, а выделение остаётся активным для повторного перемещения.
- Выбранные объекты окружены общим bounding box с четырьмя квадратными угловыми handles. Потяните handle без модификаторов для пропорционального scale относительно противоположного угла: preview не меняет документ, release сохраняет одну атомарную replacement transaction, `Esc` отменяет жест.
- Eraser Lasso показывает красный контур только во время жеста. После release валидный замкнутый контур сохраняется как одна векторная операция `EraseArea`, стирает более старое содержимое внутри области и очищает preview; `Ctrl+Z`/redo и reopen восстанавливают результат. Клик, линия и другой вырожденный контур не записываются.
- Кнопка `Layers` открывает список слоёв сверху вниз. Два checkbox управляют visibility и lock; `+` создаёт верхний слой, `Duplicate` копирует active layer над исходным с новыми UUID операций, `Delete` удаляет active layer, `Rename` меняет имя, `Up`/`Down` меняют порядок. Последний слой удалить нельзя; непустой требует подтверждения. Hidden layer не рисуется, hidden/locked active layer нельзя редактировать. Все команды сохраняются после reopen и отменяются в общей с рисованием истории.
- Колесо мыши или `Z` + ЛКМ — масштабирование относительно курсора.
- Средняя кнопка мыши или `Space` + ЛКМ — перемещение.
- Клик кистью без движения — точка текущего размера и цвета.
- `Shift` + левая кнопка мыши — прямая линия кистью или ластиком.
- `Ctrl` + левая кнопка мыши — изменение размера кисти.
- Окно `Navigation` показывает current depth, zoom, точные tile `X/Y` и local `X/Y`. Для быстрого перехода введите depth от `-10000` до `10000` и нажмите `Go` или Enter; видимый центр сохраняется.
- В `Navigation` можно ввести абсолютные tile `X/Y` на текущей глубине и нажать `Go XY` или Enter. Поддерживаются целые десятичные значения и запись `1eN`, например `1e100` или `-1e100`; `Origin` возвращает камеру к tile `0/0`. Полные огромные BigInt доступны в прокручиваемой selectable-строке, рядом показана компактная форма.
- `Ctrl+Z` — undo; `Ctrl+Y` или `Ctrl+Shift+Z` — redo.
- Панель сверху содержит инструменты, цвет, размер кисти, историю и кнопки `File`, `Navigation`, `Bookmarks`, `Settings`, `Layers`. Закладки также открываются из `Navigation`.
- Color picker сейчас редактирует только RGB и создаёт непрозрачные операции. Alpha старых операций сохраняется в документе, но при отображении цвет сводится с фоном и обрабатывается как opaque; точная прозрачность отложена.
- `Settings > Display` управляет canvas overlay: presets `Minimal/Standard/Diagnostics`, master switch и отдельные Depth, Zoom, Tile/Local coordinates, operations, FPS, status и tile-state сохраняются в `local/settings.json`.

## Сборка и запуск

Требуются Rust stable MSVC и Visual Studio Build Tools с C++ workload.

```powershell
cargo run --release
cargo run --release -- "D:\Drawings\canvas.esketch"
```

При запуске без пути к документу приложение создаёт или открывает стандартный холст в `local/default.esketch` рядом с исполняемым файлом. Локальные настройки производительности/качества хранятся в `local/settings.json`.
Подробное описание всех текущих и запланированных параметров находится в [docs/settings-reference.md](docs/settings-reference.md).

Settings содержит профили проверенных параметров:

- `Performance`: tile resolution `128 px`, `1` worker, zoom settle `250 ms`.
- `Balanced`: `512 px`, `1` worker, `140 ms`.
- `Quality`: `1024 px`, `2` workers, `80 ms`.
- `Custom` определяется автоматически после ручного изменения любого из этих трёх параметров.

Профили не меняют input spacing, Fill limits, pause, rebuild policy, prefetch, raster quality, cache или preview FPS; выбранные значения сохраняются в обычном settings JSON, поэтому профиль восстанавливается после перезапуска без отдельного поля.

`Brush input px` (`0.75..8`, default `3`) и `Fill input px` (`1..4`, default `2`) задают только минимальное расстояние между принятыми pointer samples, поэтому визуальная разница между значениями намеренно невелика. Максимальный интерполированный разрыв фиксирован отдельно: `8 px` для Brush/Eraser и `4 px` для Fill. Release endpoint всегда сохраняется, поэтому разреженный input не превращает короткий stroke в точку. Settings v8 и старше автоматически получают новые безопасные defaults вместо несовместимых старых значений `4..1024`.
На Windows активный draft дополнительно читает до `64` системных mouse-history samples между UI-кадрами. Если `GetMouseMovePointsEx` недоступен или предыдущий маркер уже выпал из истории, используются обычные egui events. Это восстанавливает фактическую кривизну движения мыши до линейной интерполяции и не изменяет настройки Smoothing, стилус, тайлы или формат документа.
Во время быстрого Fill приложение обрабатывает восстановленные и обычные события движения указателя по порядку, заполняет оставшиеся промежутки и сохраняет только контуры минимум из трёх различных точек.
Brush/Eraser strokes сглаживаются в готовых PNG-тайлах. Временный vector fallback использует один polyline path и только две endpoint caps, чтобы сохранять FPS на насыщенных участках.
Полный и retained vector fallback используют тот же spatial index, что и tile rebuild: проецируются только операции, пересекающие набор видимых логических тайлов, с дедупликацией и сохранением исходного порядка.
Сохранённый fallback дополнительно упрощает только плотную геометрию и перед egui-тесселяцией обрезает stroke/Fill по экрану. Адаптивное сглаживание применяется и после завершения штриха: Brush сначала обрезается до экрана, Fill сглаживается до polygon clipping. При более чем `4096` входных или `8192` выходных точках fallback остаётся исходным для ограничения нагрузки. Live draft остаётся точным.
Набор видимых operation indices кэшируется по ревизии и TileKey-набору. Точки fallback также кэшируются как локальные `f64` offsets относительно camera anchor: pan использует общий сдвиг, zoom — масштаб, а переход depth переносит кэш умножением/делением на коэффициент глубины. После смещения более чем на 8 тайлов anchor пересоздаётся для сохранения точности.
`Tile workers` управляет числом потоков перестройки тайлов; по умолчанию используется `1`, чтобы сохранение и rebuild на тяжелых документах меньше грузили систему. Пока фоновые тайлы ожидаются без активного рисования или zoom, интерфейс проверяет их с интервалом 50 мс вместо постоянной перерисовки 60 FPS.
`Tile resolution` задаёт точный размер bitmap каждого тайла. `64–256 px` ускоряют перестройку ценой пикселизации, `512 px` используется по умолчанию, `1024–2048 px` повышают качество и нагрузку.
`Pause tile generation` останавливает новые tile jobs и отменяет queued generations, не останавливая сохранение векторных операций, drafts, undo/redo и checkpoints. После возобновления запрашивается только текущая видимая область.
`Pause tile generation while drawing` автоматически отменяет queued tile generations после начала Brush/Eraser/Fill draft и возобновляет запросы текущей области после завершения жеста. Эта настройка не включает и не выключает постоянную ручную паузу.
`Deferred drawing preview` предназначен для слабых CPU и по умолчанию выключен. При включении активный Brush/Eraser/Fill продолжает собирать mouse-history events, применять spacing/interpolation и выполнять draft autosave, но растущая линия не проецируется, не сглаживается и не передаётся egui для тесселяции. После release операция сохраняется и появляется целиком через обычный vector fallback/тайлы. Режим не меняет итоговую геометрию, undo/redo или формат документа.
`Rebuild policy` выбирает постановку tile jobs: `Immediate` сохраняет прежнее поведение, а `After interaction` отменяет старую очередь при начале ввода и ждёт 180 мс покоя после pan, drawing или другого pointer-ввода. Уже готовые тайлы остаются видимыми во время ожидания.
`Prefetch tiles` задаёт радиус предварительной генерации вокруг экрана от `0` до `2` тайлов. Видимые тайлы всегда ставятся в очередь первыми; отсутствие ещё не готового prefetch-тайла не включает полный vector fallback.
`Edge quality` независимо выбирает `Performance`, `Balanced` или `Quality`. `Balanced` сохраняет прежние аналитические края stroke и Fill coverage `4x4`; `Quality` использует более дорогое supersampling. Каждая комбинация качества хранится в отдельном PNG cache namespace.
`Smoothing` независимо выбирает `Off`, `Light`, `Balanced` или `Strong`. Перед адаптивными quadratic-кривыми включённые уровни удаляют малые отклонения mouse-history с error-bound `0.45/0.9/1.5 px`; RDP ограничен окнами `64` точки или `32 px`, поэтому не имеет неограниченной квадратичной стоимости. Затем кривые скругляют заметные редкие углы в live draft, сохранённом full/retained fallback и cached tiles. `Off` возвращает точные исходные точки. Настройка не меняет input spacing или сохранённые операции; для ограничения FPS-нагрузки слишком большая fallback-геометрия рисуется без дополнительного сглаживания.
`Cache size MiB` задаёт дисковый бюджет `128..8192 MiB`; периодическая очистка удаляет самые старые PNG. `PNG compression` выбирает `Fast`, `Balanced` или `Small` для новых и перестроенных тайлов без изменения пикселей.
`Preview FPS` задаёт `15..120 FPS` для периодического repaint активного fallback. Polling tile jobs не ускоряется выше интервала `50 ms`, чтобы не возвращать лишнюю нагрузку в ожидании тайлов.
При паузе уже загруженные тайлы остаются видимыми после новых штрихов, а vector fallback рисует только операции новее сохранённого снимка тайла.
В простое приложение не запускает постоянный repaint loop; периодический repaint включается только для активного черновика, генерации тайлов или zoom settle.
Canvas overlay показывает скользящие `fps` и frame time только для активного pointer/draft/zoom взаимодействия. После idle новое действие начинает отдельное окно измерения; текущая цель для насыщенной depth 0 — `60 FPS` (`16.7 ms`).

Проверки:

```powershell
cargo fmt -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
cargo run --release --bin stress -- 1000000 100
```

Второй необязательный аргумент задаёт симметричный радиус depth. Пример строит миллион синтетических штрихов на 201 уровне (`-100..+100`) и измеряет отдельные tile queries и сгруппированные viewport queries на обоих краях и в нуле.
