# План EndlessSketch

Обновлено: 2026-06-29

## Выполнено

- [x] Бесконечный иерархический холст с большими координатами и уровнями глубины.
- [x] Формат `.esketch`: SQLite/WAL, восстановление черновиков, undo/redo и проверенные резервные копии.
- [x] Portable-хранилище рядом с exe: `local/default.esketch` и `local/settings.json`.
- [x] Brush, Eraser, Fill/lasso, Picker и закладки.
- [x] Непрерывный Fill при быстрых движениях.
- [x] Brush click-to-dot.
- [x] `Shift` + ЛКМ для прямых Brush/Eraser линий.
- [x] `Ctrl` + ЛКМ + drag для изменения размера кисти.
- [x] Перемещение через Space + ЛКМ и среднюю кнопку мыши.
- [x] Zoom колесом и через `Z` + ЛКМ.
- [x] Исправлены швы тайлов на depth 25.
- [x] Ограничены tile queue и GPU textures для защиты от wgpu Out of Memory.
- [x] Fast zoom: отмена устаревших задач и задержка качественных тайлов до завершения zoom.
- [x] Settings с сохранением Brush/Fill input density, Fill limits, zoom settle и tile workers.
- [x] Сглаживание Brush/Eraser и ускоренная scanline-растеризация Fill.
- [x] Адаптивный repaint без постоянного idle-цикла 60 FPS.
- [x] Opaque Performance Mode: точная прозрачность отключена, исходная alpha остаётся в документе.
- [x] Быстрый прямой opaque raster path; текущая версия raster cache 7.
- [x] Разрешения тайлов `64/128/256/512/1024/2048 px` с сохранением выбора.

## Текущий приоритет: FPS на насыщенном холсте

- [x] Добавить `Pause tile generation`.
  Пауза сохраняется, отменяет queued generations, не блокирует векторное сохранение и после возобновления запрашивает только видимую область. Ручная проверка пройдена.
- [x] Ускорить временный vector fallback:
  использовать исходные точки без Chaikin smoothing, один polyline path и две endpoint caps на stroke. Ручная проверка FPS пройдена.
- [x] Обрабатывать все накопленные события движения указателя при Fill, чтобы быстрые окружности не превращались в многоугольники из нескольких точек. Ручная проверка пройдена.
- [x] Сохранять последний готовый тайл и накладывать только новые операции, пока строится замена. Ручная проверка пройдена.
- [x] Добавить spatial culling для полного и retained vector fallback. Ручная проверка пройдена.
- [x] Добавить cache visible operation indices и projected geometry. Ручная проверка пройдена: depth `+` вырос до 40–50 FPS с редкими падениями до 30, depth `-` — около 50 FPS.
- [x] Добавить в canvas overlay скользящие FPS и frame time активного взаимодействия; целевой показатель — 60 FPS. Значения проверены вручную.
- [x] Проверить pan, zoom, Brush, Eraser и Fill на насыщенной depth 0:
  после projection cache Brush/Eraser/Fill и остальные действия работают нормально; depth `-` — около 50 FPS; depth `+` — 40–50 FPS с редкими падениями до 30.

## Затем: настройки производительности

- [x] Добавить профили `Performance`, `Balanced`, `Quality` и автоматически определяемый `Custom`. Ручная проверка пройдена.
- [x] Добавить инкрементальное обновление тайлов для непрозрачного Paint; Fill, Eraser и недоступный base PNG используют полный rebuild. Ручная проверка пройдена.
- [x] Добавить `Pause tile generation while drawing` с автоматическим возобновлением после завершения штриха. Ручная проверка пройдена: отсутствие визуальной разницы при уже готовых тайлах ожидаемо, постоянная ручная пауза остаётся независимой.
- [x] Добавить настройки rebuild policy и prefetch: `Immediate`/`After interaction`, ожидание 180 мс и радиус `0..2` с приоритетом видимых тайлов. Ручная проверка пройдена.
- [x] Добавить независимые edge quality и smoothing с отдельными cache namespaces и без изменения input density/сохранённых точек. Ручная проверка пройдена.
- [x] Добавить cache size `128..8192 MiB`, PNG compression `Fast/Balanced/Small` и preview `15..120 FPS`. Ручная проверка пройдена.
- [x] Добавить render-time clipping и упрощение плотной геометрии с допуском `0.25 px` без изменения сохранённых операций. Ручная проверка пройдена.
- [x] Переработать Brush/Fill quality settings: Brush input `0.75..8 px`, Fill input `1..4 px`, фиксированные interpolation gaps `8/4 px`, независимый Smoothing и обязательный release endpoint. Ручная проверка подтвердила независимость input, но мышь сохраняет угловатые кривые.
- [x] Добавить адаптивное quadratic curve smoothing для разреженного mouse-ввода в live draft и cached tiles, почти не меняя плотный stylus-ввод. Ручная проверка показала, что после отпускания saved fallback оставался raw.
- [x] Применить bounded adaptive Smoothing к сохранённому full/retained fallback до готовности тайла: Brush обрезается до экрана перед сглаживанием, Fill сглаживается до polygon clipping, обработка отключается свыше `4096` входных или `8192` выходных точек. Нужна ручная проверка мышью с паузой тайлов.
- [x] На Windows восстанавливать до `64` промежуточных mouse samples через системную историю `GetMouseMovePointsEx` для активного draft. Использовать существующие egui events при недоступной истории или потерянном маркере; не менять сглаживание, тайлы или формат сохранения. Ручная проверка: траектория мыши стала намного лучше; в активном vector draft при `Smoothing: Off` остаются мелкие ступени между целочисленными screen-coordinate samples.
- [x] Перед adaptive curve smoothing удалять малый mouse-history jitter с допуском `0.45/0.9/1.5 px` для `Light/Balanced/Strong`. `Off` сохраняет исходные точки; error-bounded RDP работает окнами максимум `64` точки или `32 px`, не увеличивает геометрию и не меняет сохранённые операции. Raster cache version `8`. Ручная проверка: сглаживание заметно улучшает контур, высокий Input также устраняет артефакт; минимальный Input с `Off` ожидаемо показывает острые raw vector joins.
- [x] Добавить persisted `Deferred drawing preview` для слабых CPU: во время активного Brush/Eraser/Fill собирать и autosave-ить input без проекции, Smoothing и тесселяции растущего draft; показать обработанную операцию после release. Settings version `10`, по умолчанию выключено. Функциональная ручная проверка пройдена; замер производительности на слабом ПК отложен.

## Следующий этап

- [x] Проверить и оптимизировать depth `-100..+100`: camera/tile projection, spatial index, Brush width, SQLite operation/bookmark round-trip и ручные Brush, Fill, undo/redo, bookmark/reopen прошли. Spatial query переиспользует BigInt scale по target/source depth; на stress `1M/201 bands` три viewport queries ускорились примерно с `2.41 s` до `1.72 s`. Toolbar jump `-10000..10000` также вручную проверен на depth `1000` и `10000`.
- [x] Проверить и оптимизировать depth `-1000..+1000`: stress `1M/2001 bands` прошёл (`1.33 s` index, `1.65 s` на три viewport query). Same-depth camera/tile/spatial/raster/SQLite coverage на ±1000, projection-cache reset/cull через 2000 уровней и ручные Brush, Fill, undo/redo, bookmark/reopen на обоих краях прошли.
- [x] Проверить большие боковые смещения от origin: toolbar jump по абсолютным BigInt tile X/Y с decimal/`1eN`, быстрый возврат к origin и автоматические projection-cache/spatial/SQLite проверки на `10^1000` готовы; `113/113` тестов и ручная проверка прошли.
- [x] Повторить на экстремальных координатах рисование, Fill, undo/redo, bookmarks и восстановление после перезапуска. Ручная проверка пройдена.
- [x] Добавить постоянно обновляемое отображение текущих tile X/Y и local X/Y в canvas overlay с компактным форматом огромных BigInt. Ручная проверка пройдена.

## Позже

- [ ] Вернуть точную прозрачность после отдельного профилирования, предпочтительно через GPU rasterization.
- [ ] Save As.
- [ ] Прямоугольное и lasso-выделение.
- [ ] Удаление и редактирование выделенного содержимого.
- [ ] Настраиваемые горячие клавиши.
- [ ] Eraser fill mode.
- [ ] Ручные именованные слои.
- [ ] Импорт изображений.
- [ ] GPU-native tile rasterization.
- [ ] Installer, signing и финальная release packaging.
- [ ] Добавить лёгкую визуальную trailing-линию во время `Deferred drawing preview`, не запуская обработку и тесселяцию полного растущего draft.

## Ближайший пункт

Начать возврат точной прозрачности с отдельного профилирования CPU/GPU-пути и определить вариант композитинга без прежних штрихов и падения FPS.
