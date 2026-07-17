# Настройки EndlessSketch

Обновлено: 2026-07-01

Настройки Rust-версии хранятся в `local/settings.json` рядом с исполняемым файлом. Изменения применяются сразу и сохраняются автоматически. Кнопка `Reset` возвращает все параметры к значениям по умолчанию.

Встроенная вкладка `File > Help` или `F1` содержит подробные русские и английские объяснения всех текущих параметров. Переводы загружаются из `help/*.json` рядом с `endless-sketch.exe`; файл с новым уникальным `id` добавляет языковую вкладку после перезапуска. Встроенные копии `ru/en` используются как fallback, если внешние файлы отсутствуют или повреждены.

## Профиль производительности

`Profile` одновременно меняет почти все параметры, которые влияют на скорость, качество, плотность ввода, временный vector fallback, tile work и частоту preview.

| Параметр | Performance | Balanced (по умолчанию) | Quality |
| --- | --- | --- | --- |
| Brush input px | `6` | `3` | `1.5` |
| Fill input px | `4` | `2` | `1` |
| Fill fallback points | `1024` | `4096` | `32768` |
| Fill fallback depth | `3` | `6` | `12` |
| Stroke fallback joins | `Performance` | `Auto` | `Quality` |
| Saved fallback ops | `1500` | `0` / Unlimited | `0` / Unlimited |
| Zoom settle ms | `250` | `140` | `80` |
| Tile workers | `1` | `1` | `2` |
| Tile resolution | `128 px` | `512 px` | `1024 px` |
| Pause tile generation while drawing | `on` | `off` | `off` |
| Deferred drawing preview | `on` | `off` | `off` |
| Rebuild policy | `After interaction` | `Immediate` | `Immediate` |
| Prefetch tiles | `0` | `1` | `2` |
| Edge quality | `Performance` | `Balanced` | `Quality` |
| Smoothing | `Light` | `Balanced` | `Strong` |
| PNG compression | `Fast` | `Fast` | `Small` |
| Storage commit | `Fast` | `Full` | `Full` |
| Preview FPS | `30` | `60` | `120` |

`Custom` определяется автоматически, если хотя бы одно управляемое поле не совпадает с полной матрицей выбранного профиля. Если вручную вернуть все управляемые поля к точным значениям одного профиля, UI снова покажет этот профиль.

Профиль намеренно не меняет:

- `Pause tile generation`: это оперативный ручной выключатель фоновой работы.
- `Cache size MiB`: это аппаратный/дисковый бюджет пользователя.
- `Keep latest objects`: это контроль редактируемости/уплотнения старых объектов.
- `Settings > Display`, overlay и session logging: это UI/diagnostics-настройки, а не качество рендера.

Новые параметры должны попадать в профильную матрицу, если они осмысленно влияют на скорость, качество, плотность ввода, fallback cost, tile work или preview cadence. Исключения нужно фиксировать явно.

## Геометрия ввода

### Brush input px

- Диапазон: `0.75..8 px`.
- По умолчанию: `3 px`.
- Управляет минимальным расстоянием между принятыми pointer samples Brush/Eraser.
- Меньшее значение точнее повторяет быстрые движения, но увеличивает документ и нагрузку.
- Большое значение уменьшает число входных точек, но не меняет максимальную длину интерполированного сегмента.
- Разрывы всегда делятся на участки не длиннее `8 px`, а release endpoint сохраняется независимо от выбранной плотности.
- Визуальная разница между значениями может быть небольшой: линейная интерполяция сохраняет траекторию, а видимая округлость управляется `Smoothing`.
- На Windows перед spacing и интерполяцией восстанавливается до `64` промежуточных mouse-history samples между UI-кадрами. При недоступности системной истории используются обычные egui events.
- Комбинация минимального `0.75 px` и `Smoothing: Off` намеренно сохраняет каждый целочисленный sample; на толстой диагональной линии это может давать острые vector joins. Для обычного рисования используйте больший Input либо включённый Smoothing.

Этот параметр не является силой сглаживания: внешний вид готового тайла настраивается отдельным `Smoothing`.

### Fill input px

- Диапазон: `1..4 px`.
- По умолчанию: `2 px`.
- Управляет минимальным расстоянием между принятыми точками замкнутого Fill/lasso-контура.
- Все восстановленные и обычные pointer events обрабатываются по порядку, а разрывы всегда делятся на участки не длиннее `4 px`.
- Fill сохраняется только при наличии минимум трёх различных точек; затем контур автоматически замыкается.
- Стилус обычно передаёт больше реальных точек, поэтому его контур естественно выглядит плавнее мыши при одинаковом input spacing.

При первой загрузке settings v8 и старше прежние `Brush/Fill spacing` сбрасываются к новым безопасным значениям `3/2 px`, потому что шкала `4..1024` имела несовместимую семантику.

## Временный vector fallback

### Fill fallback points

- Диапазон: `128..65536`.
- По умолчанию: `4096`.
- Ограничивает число экранных точек производного временного vector fallback для сохранённого Fill до готовности PNG-тайла.
- Меньшее значение снижает нагрузку интерфейса и делает временный fallback грубее, но не обрезает raw/live Fill contour и не меняет сохранённую геометрию.

### Fill fallback depth

- Диапазон: `0..32`.
- По умолчанию: `6`.
- Ограничивает разницу между текущей глубиной камеры и native depth Fill для временной векторной отрисовки.
- За пределом значения Fill ожидает тайл, что предотвращает дорогую или некорректную full-screen fallback-геометрию.

### Stroke fallback joins

- Значения: `Auto`, `Quality`, `Performance`.
- По умолчанию: `Auto`.
- Управляет joins только во временном no-tile vector fallback для сохранённых Brush/Eraser strokes. Cached PNG tiles, live input, сохранённые точки и формат `.esketch` не меняются.
- `Auto` рисует короткие сохранённые sparse strokes до `128` экранных точек segmented capsule-joins, когда нет активного draft. Во время рисования, ожидания тайлов или tile pause уже сохранённые фоновые strokes временно используют быстрый one-shape raw polyline path без saved smoothing и endpoint caps, чтобы снизить input latency.
- `Quality` сохраняет segmented capsule-joins для сохранённых strokes до `512` экранных точек даже во время рисования.
- `Performance` всегда использует быстрый one-shape raw polyline path без saved smoothing и endpoint caps.
- Dense strokes выше выбранного лимита всегда остаются на быстром polyline path.
- На перегруженной no-tile сцене действует защитный лимит segmented stroke shapes на кадр: когда Quality/Auto исчерпывают лимит, оставшиеся runs в этом кадре временно рисуются быстрым path. Это предотвращает вылеты/резкие memory spikes и не меняет сохранённые точки или PNG-тайлы. В session log это видно как `fallback_segmented_budget_fallbacks`.

### Saved fallback ops

- Диапазон: `0..100000`.
- По умолчанию: `0` (`Unlimited`).
- Управляет временным no-tile saved vector fallback: если значение больше нуля, fallback рисует только новейшие N видимых сохранённых операций и пропускает более старые до готовности тайлов.
- Это снижает projection/smoothing/clipping/egui-shape cost на перегруженной глубине, но старая часть рисунка может временно пропасть без тайлов.
- PNG-тайлы, live draft, сохранённые операции, `.esketch`, история, compact blocks и raster output не меняются.
- `Performance` preset выставляет `1500`. `Balanced` и `Quality` выставляют `0`, чтобы обычный просмотр оставался полным.

## Генерация тайлов

### Zoom settle ms

- Диапазон: `0..1000 ms`.
- По умолчанию: `140 ms`.
- Определяет паузу после последнего zoom-события перед постановкой качественных tile jobs.
- Большое значение уменьшает отменённую работу во время быстрого zoom, но тайлы появляются позже.

### Tile workers

- Диапазон: `1..4`.
- По умолчанию: `1`.
- Определяет число фоновых потоков растеризации.
- Большее значение ускоряет холодную генерацию на многоядерном CPU, но сильнее конкурирует с интерфейсом и другими приложениями.

### Tile resolution

- Значения: `64`, `128`, `256`, `512`, `1024`, `2048 px`.
- По умолчанию: `512 px`.
- Меняет физическое разрешение PNG, не меняя логический размер тайла на холсте.
- Малые значения быстрее и экономнее, но пикселизуются. Большие повышают детализацию и стоимость CPU, памяти, диска и загрузки текстур.

### Pause tile generation

- По умолчанию: выключено.
- Полностью запрещает новые tile jobs и отменяет устаревшую очередь.
- Не останавливает векторное сохранение, drafts, undo/redo или checkpoints.
- После выключения паузы запрашивается текущая область.

### Pause tile generation while drawing

- По умолчанию: выключено.
- Временно отменяет queued jobs при начале Brush, Eraser или Fill и разрешает их после завершения жеста.
- Не изменяет постоянную ручную паузу.
- При уже готовых тайлах визуальной разницы может не быть.

### Deferred drawing preview

- По умолчанию: выключено.
- Предназначено для слабых CPU, которые теряют часть быстрого жеста из-за обработки растущего live draft.
- Brush/Eraser/Fill продолжает собирать системную mouse history, применять Input spacing/interpolation, сохранять release endpoint и выполнять draft autosave.
- До отпускания кнопки растущий draft не проецируется, не проходит Smoothing и не тесселируется egui. После release сохранённая операция появляется целиком через обычный fallback/тайлы.
- Итоговые точки, Smoothing после release, undo/redo и формат `.esketch` не меняются.
- Режим не устраняет нагрузку от уже существующей сцены или SQLite autosave; он убирает только стоимость live preview текущего жеста.

### Rebuild policy

- `Immediate` (по умолчанию): недостающие или устаревшие тайлы запрашиваются сразу.
- `After interaction`: очередь отменяется при начале pointer-ввода и возобновляется после `180 ms` покоя.
- Во время ожидания последние готовые тайлы остаются видимыми, а новые операции показываются vector overlay.

### Prefetch tiles

- Диапазон: `0..2`.
- По умолчанию: `1`.
- Задаёт число колец тайлов, заранее создаваемых вокруг экрана.
- Видимые тайлы всегда ставятся в очередь раньше prefetch-колец.
- `0` снижает фоновую работу; `2` может сделать последующее перемещение плавнее ценой дополнительных CPU, диска и памяти.
- Отсутствующий off-screen prefetch-тайл не включает полный vector fallback.

### Cache size MiB

- Диапазон: `128..8192 MiB`.
- По умолчанию: `2048 MiB`.
- Ограничивает общий размер дискового PNG-кэша всех глубин, разрешений и вариантов качества текущего документа.
- После каждых 64 успешных записей выполняется проверка; при превышении лимита самые старые PNG удаляются до 90% бюджета.
- Уменьшение значения не запускает немедленное синхронное сканирование всего кэша: новый лимит применяется при очередной периодической очистке.

### PNG compression

- `Fast` (по умолчанию): минимальная нагрузка на CPU и более крупные файлы; соответствует прежнему поведению.
- `Balanced`: стандартный компромисс PNG encoder.
- `Small`: максимальное сжатие и более дорогая запись.
- Compression не меняет пиксели, исходные операции или cache identity.
- Уже существующие PNG не переписываются; режим применяется к новым и перестроенным тайлам.

### Storage commit

- Значения: `Full`, `Fast`.
- По умолчанию: `Full`.
- Управляет SQLite `PRAGMA synchronous` для commit-ов текущего документа.
- `Full` использует `synchronous=FULL`, то есть сохраняет прежнюю более осторожную запись на диск. Это медленнее на больших Selection/compact commit-ах, но лучше защищает от потери последних изменений при сбое ОС или питания.
- `Fast` использует `synchronous=NORMAL`. Это снижает задержку `transaction_commit_ms` в тяжёлых массовых правках, но слабее защищает последние commit-ы при системном сбое или отключении питания. Обычное закрытие приложения, `.esketch` формат, WAL, undo/redo и схема SQLite не меняются.
- `Performance` preset выставляет `Fast`. `Balanced` и `Quality` оставляют `Full`.
- Текущий режим пишется в session log как `context.storage_commit_mode`, чтобы сравнивать FPS/input lag с фактической долговечностью commit-ов.

### Preview FPS

- Диапазон: `15..120 FPS`.
- По умолчанию: `60 FPS`.
- Задаёт периодический repaint активного draft, zoom settle и ожидания `After interaction`.
- Pointer events по-прежнему вызывают кадры напрямую, поэтому настройка прежде всего ограничивает фоновое обновление preview.
- Polling готовых tile jobs не выполняется чаще прежнего интервала `50 ms` (`20 Hz`). При Preview FPS ниже 20 polling также замедляется до выбранного интервала.

## Качество растеризации

### Edge quality

- `Performance`: минимальное покрытие краёв; быстрее, но ступеньки заметнее.
- `Balanced` (по умолчанию): сохраняет прежний аналитический край Brush/Eraser и Fill coverage `4x4`.
- `Quality`: повышенное supersampling-покрытие; чище края, но дороже перестройка тайлов.

Изменение создаёт отдельный namespace PNG-кэша и не меняет исходные векторные операции.

### Smoothing

- `Off`: исходные точки без сглаживания.
- `Light`: jitter tolerance `0.45 px` и небольшой радиус кривой только на заметных углах sparse-ввода.
- `Balanced` (по умолчанию): jitter tolerance `0.9 px`, средний радиус и шаг адаптивной quadratic-кривой.
- `Strong`: jitter tolerance `1.5 px`, широкий радиус, более чувствительное обнаружение поворотов и наиболее частая тесселяция.

При включённом уровне сначала удаляются малые отклонения в пределах указанного screen-space допуска, затем строятся адаптивные кривые. Error-bounded RDP работает окнами не больше `64` точек или `32 px` пути и никогда не увеличивает число входных точек. Smoothing применяется при растеризации Brush, Eraser и замкнутого Fill, к единственному активному live draft и к сохранённому full/retained fallback до готовности тайла. Сохранённые точки не изменяются. Для ограничения нагрузки fallback остаётся исходным при более чем `4096` входных или `8192` сглаженных точках; Brush сглаживается до clipping, чтобы clipping не менял форму кривой при zoom/depth navigation.

## Фиксированная оптимизация геометрии

Это не пользовательская настройка. No-tile vector fallback больше не упрощает сохранённые операции по экранному `0.25 px` допуску, потому что такой допуск менял набор точек при zoom/depth navigation и мог визуально менять дальние объекты. Stroke clipping выполняется после fallback smoothing, а Fill fallback ограничивает точки стабильным сэмплингом исходного порядка. Live draft, cached tiles и точки в `.esketch` не изменяются.

## Внутренний clipboard Selection

`Ctrl+C` копирует выбранные целые операции без history step. `Ctrl+X` копирует и удаляет их одним undoable step. `Ctrl+V` вставляет новые UUID в active layer с нарастающим offset `16 px`; `Ctrl+Shift+V` вставляет in place. Locked/hidden layer отклоняет Paste. Clipboard не сохраняется в `.esketch` и очищается при смене документа.

## Запланированные настройки

Эти параметры ещё не реализованы; названия и диапазоны могут измениться после профилирования.

### Transparency quality

Возврат точной alpha-композиции после отдельного профилирования, предпочтительно через GPU rasterization. Текущий Opaque Performance Mode сохраняет исходную alpha в документе, но отображает результат непрозрачно.

### Configurable hotkeys

Назначение клавиш инструментов, undo/redo, навигации и служебных действий.
Планируемый default для создания нового верхнего слоя — `Shift+N`; `F1` открывает справку.
Для инструментов с несколькими режимами первое нажатие shortcut активирует инструмент, повторное циклически меняет mode: `S` — Selection `Inside/Crossing`, `X` — общий Area tool `Fill/Erase`. При фокусе в текстовом или числовом поле shortcut не обрабатывается.
Каждая wheel-команда получает stylus-альтернативу `key + vertical drag` и кнопку/menu fallback. Текущие defaults плана: `Z+drag` zoom, `Space+drag` pan, `Alt+drag` overlap cycle, `Ctrl+Alt+drag` paint-order steps. Будущие настройки: `Drag step px`, invert direction и переназначение modifier/key chord. Gesture фиксируется до release и не может случайно продолжиться как рисование.

### Undo history length

Планируемый лимит количества доступных для undo transaction groups: ориентировочные варианты `100/500/1000/5000/Unlimited`. Настройка не должна удалять активные векторные операции, потому что они являются содержимым рисунка; уменьшение размера файла потребует отдельного безопасного snapshot/compaction механизма.

### Auto-compact old objects

Планируемые варианты после профилирования: `Off/500/1000/5000 operations`. Порог запускает фоновое уплотнение только стабильного старого содержимого во время idle. Исходные векторы должны сохраняться в проверенном compressed snapshot, а объединённый блок использовать rebuildable multi-depth LOD cache; автоматическое необратимое превращение в bitmap запрещено.

### Area Fill/Erase mode

Планируется объединить Lasso Fill и Eraser Lasso в один Area tool с общей геометрией контура. `X` активирует инструмент и при повторном нажатии переключает `Fill/Erase`; toolbar и preview явно показывают destructive режим. Текущий `EraseArea` по-прежнему записывается с active `layer_id`, поддерживает undo/redo и reopen. Переключатель scope `Active layer` / `All unlocked layers` остаётся задачей M4.

### Canvas overlay / Display

Раздел `Settings > Display` появился в settings version 11. Он влияет только на текст поверх холста и не меняет документ, камеру, векторы или тайлы.

- `Show canvas overlay` полностью скрывает или показывает overlay.
- `Minimal`: только zoom и status.
- `Standard` (по умолчанию): zoom, operation count, FPS/frame time, status и активное состояние tile/rebuild.
- `Diagnostics`: все поля, включая depth, сокращённые Tile X/Y и Local X/Y.
- `Custom` определяется автоматически после любого ручного изменения.
- Независимые флаги: `Depth`, `Zoom`, `Tile X/Y`, `Local X/Y`, `Operation count`, `FPS / frame time`, `Status`, `Tile / rebuild state`.

Старые settings v10 и ниже получают безопасный профиль `Standard`. Выбор сохраняется в `local/settings.json`. Полные огромные BigInt-координаты всегда доступны в окне `Navigation`, даже если их компактное отображение в overlay выключено.

### Session logging / Diagnostics

`Settings > Diagnostics > Session logging` persists in `local/settings.json`.

- `Off`: disables session JSONL writes.
- `Basic` (default): writes launch/open/settings, markers, coalesced navigation, tile pause/resume/fallback/queue changes, save/undo/redo/compact/selection edits, FPS drops, and slow frame/phase snapshots.
- `Detailed`: also records navigation-caused tile generation invalidations.

Session files are written next to settings under `local/logs/session-YYYYMMDD-HHMMSS.jsonl`; only the newest 20 session logs are kept at startup. Each line is one JSON event with monotonic `t_ms`, local `wall_time`, camera depth/zoom/tile/local coordinates, document revision/counts, tile pause/fallback/pending state, performance metrics, active tool, selection count, smoothing, stroke fallback join mode, saved fallback operation limit, tile resolution, compaction limit, and status text. Slow-frame and phase events also include lightweight saved fallback counters: approximate fallback stroke shape count, segmented/fast stroke path counts, `fallback_projected_operations`, `fallback_painted_operations`, saved fallback projection/derived `fallback_cache_hits`/`fallback_cache_misses`, and `fallback_skipped_operations` when the temporary saved fallback budget omits old visible operations. Tile diagnostics split frame time into `tile_total`, `tile_collect_upload`, `tile_request_queue`, and `tile_draw`; fallback diagnostics split frame time into `fallback_project`, `fallback_derive`, `fallback_clip`, and `fallback_shape_paint`, with matching `perf` fields in milliseconds. The tile payload records per-frame `uploaded_textures` and `queued_jobs`. When an automatic FPS/phase event adds timing fields, they are merged into the existing `perf` object so fallback counters remain visible in the same JSON line.

Press `F12` or the `Mark log` button to write a manual marker. The status/overlay shows `Log marker #N`. For FPS/zoom analysis, send the marker number, the approximate action around it, and the `.jsonl` file.

### Persistent Area selection

`Area selection` работает как временный режим с формами `Rectangle/Lasso`. Контур хранится в canvas coordinates, следует за pan/zoom/depth и ограничивает новые Brush/Eraser/Fill operations при commit; сам Selection не записывается в документ и не меняет revision, пока пользователь не выполнит операцию. В текущей версии один Replace-контур; Add/Subtract/Intersect добавляются после устойчивого polygon clipping. Gradient остаётся планируемой операцией внутри активной Area selection.

### Gradient

Первая версия: непрозрачный linear gradient с двумя color stops и start/end handles внутри активной Area selection. Затем radial gradient. Alpha stops, feather/soft edge и opacity откладываются до нового compositing path. Gradient сохраняется как векторная operation и должен одинаково выглядеть в fallback, tiles и export.

### Manual layers

Управление именованными пользовательскими слоями дополнительно к автоматическим уровням глубины.
Окно `Layers` показывает stack сверху вниз, active layer и количество операций. Первый checkbox управляет visibility, второй — lock. `+` создаёт новый верхний слой, `Duplicate` создаёт копию active layer непосредственно над исходным с новыми UUID операций, `Delete` удаляет active layer, `Rename` меняет имя, `Up`/`Down` меняют порядок. `Move selection to` переносит Object selection в другой visible/unlocked layer. `Merge Down` объединяет active layer с непосредственным нижним. Последний слой удалить нельзя; для непустого слоя требуется подтверждение. Hidden layer исключается из рендера, hidden/locked active layer не принимает инструменты редактирования. Все команды входят в общую с рисованием undo/redo timeline.

Планируется multi-layer selection: обычный клик задаёт единственный active/selected layer, `Ctrl+клик` переключает отдельные слои, `Shift+клик` выбирает диапазон. Рисование всегда остаётся только в active layer. Bulk visibility/lock/reorder/delete/duplicate выполняются для selected layers атомарно; reorder сохраняет внутренний порядок блока. Позднее тот же selected set используется folders и `Merge Selected`.
