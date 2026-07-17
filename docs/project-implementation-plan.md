# План EndlessSketch

Обновлено: 2026-07-07

## Обозначения

- `[x]` — выполнено и проверено.
- `[ ]` — не выполнено.
- **Средняя** — локальное изменение без перестройки базовой модели документа.
- **Высокая** — несколько подсистем, новый UI и обязательные изменения persistence/cache.
- **Очень высокая** — изменение модели документа или рендера с миграцией, производительностью и большим набором регрессий.

Оценка сложности не является оценкой времени. Каждый крупный этап разбивается на короткие проверяемые checkpoints.

## Выполненная база

- [x] Бесконечный иерархический холст с BigInt-координатами и прямыми переходами по depth и tile X/Y.
- [x] Формат `.esketch`: SQLite/WAL, UUID операций, undo/redo, drafts и проверенные резервные копии.
- [x] Brush, Eraser, Fill/lasso, Picker, bookmarks и восстановление после перезапуска.
- [x] Быстрый mouse input через `GetMouseMovePointsEx`, адаптивное сглаживание и deferred preview.
- [x] Физические tile resolution `64..2048 px`, cache budget, PNG compression, workers, prefetch и rebuild policy.
- [x] Opaque Performance Mode, incremental Paint tiles, retained tiles и vector overlay новых операций.
- [x] Spatial/projection caches и приемлемый FPS на насыщенном холсте.
- [x] Проверки depth `-1000..+1000` и боковых координат порядка `10^1000`.
- [x] Отображение текущих depth, tile X/Y, local X/Y, FPS и состояния тайлов.
- [x] Быстрый grouped undo/redo; redo доступен через `Ctrl+Y` и `Ctrl+Shift+Z`.

## Основная задача. Выделение, редактирование, Eraser Lasso и Layers

**Сложность: очень высокая.**

Это одна связанная задача с тремя параллельными потоками. Операции сейчас являются последовательным авторитетным журналом, поэтому selection/editing, destructive lasso и layers должны использовать общие UUID, grouped history, spatial queries и cache invalidation.

### Общий фундамент

- [x] Добавить общий transaction ID: новые одиночные операции используют собственный UUID, а операции с общим ID отменяются/возвращаются одним undo/redo step.
- [ ] Зафиксировать append-only replacement/tombstone model вместо небезопасного изменения старых SQLite rows.
- [x] Добавить `layer_id` и schema v1→v2 migration старых документов в один default layer с отдельным проверенным backup.
- [x] Вынести rectangle/lasso geometry, polygon bounds, point-in-polygon, stroke radius и polygon intersection в общий модуль.
- [x] Добавить UUID spatial results и фильтрацию по набору допустимых layer IDs рядом с существующим индексным API.
- [x] Добавить bounded/broad affected-tile coverage по operation bounds как основу точечной инвалидации старой и новой геометрии.
- [x] Устранить полную перезагрузку документа при undo/redo: декодировать только изменённую transaction group, удалять её хвостовые spatial entries при undo и инкрементально возвращать при redo.
- [ ] Сохранить recovery при незавершённой grouped transaction.
- [x] Общие контракты покрыть model/storage/geometry/spatial тестами до подключения UI.

После фиксации этих контрактов потоки A, B и C можно выполнять одновременно и соединять после каждого рабочего checkpoint.

### Поток A. Выделение и редактирование

**Сложность потока: очень высокая.**

#### A.1. Правила и модель выделения

- [ ] Выделять целые векторные операции по UUID, а не отдельные пиксели готового тайла.
- [ ] Хранить активное выделение как временное состояние UI; не записывать его в документ в первой версии.
- [x] Реализовать два режима попадания: `Inside` полностью заключает операцию, `Crossing` выбирает пересечения.
- [x] Обычный клик выбирает один целый объект под курсором. Кандидаты ранжируются по расстоянию до геометрии, затем по меньшему screen-space bounding box и sequence, чтобы маленькая линия не терялась под длинной.
- [x] `Alt+колесо` циклически переключает перекрывающиеся объекты под курсором в прямом/обратном направлении; каждое сырое физическое деление колеса даёт ровно один шаг даже при быстрой прокрутке. `Alt+ЛКМ` tap переиспользован для быстрого выбора цвета без смены инструмента, а `Alt+вертикальный drag` в Object Selection циклически перебирает overlap candidates для стилуса.
- [x] `Shift+клик` добавляет объект к выделению, `Ctrl+клик` переключает его состояние.
- [x] Rectangle по умолчанию работает в `Inside`; toolbar позволяет явно включить `Crossing`.
- [ ] Первое `S` активирует Selection, каждое повторное `S` при уже активном Selection Object mode переключает `Inside`/`Crossing`; первое `Q` активирует Selection, повторное `Q` переключает `Object`/`Area`.
- [ ] Toolbar, status и курсор сразу показывают текущий Selection mode; переключение не очищает уже выбранные объекты.
- [x] Сохранить текущий `Object selection` для выбора операций целиком и добавить независимый `Area selection`, который задаёт постоянную clipping-область Rectangle/Lasso. M5.2a реализовал временный контур, M5.2b подключил commit-time clipping для новых Brush/Eraser/Fill.
- [x] Object selection и Area selection не активны одновременно; явное переключение режима очищает несовместимое временное состояние.
- [ ] Учитывать полную Brush/Eraser stroke width при clipping; M5.2b пока режет stroke по центральной линии, а Fill по polygon.
- [x] Selection-фильтр принимает только active layer; hidden/locked active layer не возвращает кандидатов, а смена active layer очищает выделение.
- [ ] Зафиксировать поведение выделения объектов с разных depth без потери BigInt-точности.

#### A.2. Общая геометрия выбора

- [ ] Добавить broad phase через существующий spatial index.
- [ ] Добавить точный hit test для stroke, Fill polygon и destructive операций.
- [ ] Вынести rectangle/lasso capture, bounding box и point-in-polygon в общий модуль для повторного использования стирающим лассо.
- [ ] Ограничить сложность hit test на очень плотных операциях и больших документах.
- [ ] Покрыть тестами пересечения на разных depth и экстремальных tile X/Y.

#### A.3. UI выделения

- [ ] Добавить инструмент Selection с иконкой и отдельным shortcut.
- [ ] Сначала реализовать прямоугольное выделение.
- [ ] Затем добавить свободное lasso-выделение.
- [ ] Показать bounding box и количество выбранных операций отдельным компактным UI.
- [x] Показывать hover-подсветку объекта, который будет выбран кликом, до нажатия.
- [ ] Добавить `Shift` для добавления и `Ctrl` для исключения/переключения элементов.
- [ ] Добавить `Esc`/клик вне области для снятия выделения.
- [ ] Не запускать tile rebuild при одном только изменении UI-выделения.
- [ ] Убрать Cut/Copy/Paste/Delete из общей верхней панели в компактное всплывающее меню `Selection` рядом с режимами `Object/Area`, `Inside/Crossing` и `Rectangle/Lasso`.
- [x] При активном инструменте Selection ПКМ на холсте открывает компактное окно `Selection` возле курсора; оно переиспользует те же команды и состояние, что toolbar popover, и не создаёт отдельную command/history логику.

#### A.4. Подключение к общей grouped history

- [ ] Добавить transaction/group ID для нескольких изменений как одного шага undo/redo.
- [ ] Не изменять старые SQLite rows на месте: записывать replacement/tombstone-команды или эквивалентную безопасную ревизию.
- [ ] Сохранить recovery после аварийного завершения посередине операции.
- [ ] Инвалидировать только тайлы, затронутые старой и новой геометрией.
- [ ] Проверить undo/redo и reopen после каждого вида группового редактирования.
- [x] Базовый лимит undo history: хранить только последние 1000 активных history transaction groups для обычного Undo, не удаляя активные векторные операции документа.
- [ ] Добавить в Settings длину undo history в transaction groups, например `100/500/1000/5000/Unlimited`.
- [ ] Ограничение должно менять только глубину доступной отмены, не удаляя активные векторные операции, которые остаются авторитетным содержимым рисунка.
- [ ] Уменьшение размера самого документа через snapshots/compaction проектировать отдельно и только с проверенным backup.

#### A.5. Базовое редактирование

- [x] Удаление Object selection через `Delete`/`Delete selected`: append-only UUID tombstone, reopen и атомарный undo/redo.
- [ ] После базового Selection-окна по ПКМ из checkpoint 5.0e расширить его object-sensitive командами Duplicate, Move и переноса на слой без отдельной логики history.
- [ ] Дублирование выделенного как одна grouped transaction: новые UUID, та же геометрия/стили/порядок и небольшой экранный offset.
- [ ] Перекрашивание Object selection через текущую RGB-палитру: заменить color всех выбранных `Paint`/`Fill` одной append-only tombstone/replacement transaction, сохранив geometry, width, layer и paint order.
- [ ] `Erase`/`EraseArea` не перекрашивать; при смешанном selection команда меняет только цветные операции, оставляет destructive operations и весь selection активными, а Undo/Redo/Reopen восстанавливают результат одним шагом.
- [ ] Для `Area selection` разрезать Brush/Eraser polylines по границе рамки: внешние части остаются исходным содержимым, внутренние становятся отдельными редактируемыми фрагментами.
- [ ] `Copy Area` и `Cut Area` работают с текущей Area selection, а не с Object selection: копируется/вырезается только геометрия внутри маски, при необходимости создавая обрезанные фрагменты и оставляя внешние части на месте.
- [ ] Для Fill выполнять polygon clipping и сохранять все валидные внутренние/внешние контуры без разрывов и самопересечений.
- [ ] Записывать split/replacement как одну grouped transaction, чтобы undo полностью восстанавливал исходную операцию.
- [x] Drag-перемещение Object selection с live preview и сохранением как одной tombstone/replacement transaction; replacement наследует исходный `paint_order`, поэтому объект не поднимается над соседями после Move, reopen, undo или redo.
- [x] `Ctrl+колесо` при непустом Object selection меняет порядок относительно соседних объектов active layer: вверх — на один шаг вперёд, вниз — на один шаг назад.
- [x] Каждое физическое деление колеса даёт ровно один reorder step; обычное колесо продолжает zoom, `Alt+колесо` продолжает cycle объектов.
- [x] Добавить stylus-доступный drag-эквивалент overlap wheel: `Alt+вертикальный drag` в Object Selection перебирает overlap candidates без zoom, stroke, rectangle или history; `Alt+tap` остаётся пипеткой.
- [x] Дискретный drag накапливает signed distance и даёт ровно один selection-cycle step на порог около 28 px; отпускание сбрасывает остаток и завершает gesture.
- [ ] Отдельно добавить stylus-доступный paint-order drag: `Ctrl+Alt+вертикальный drag` меняет порядок выбранных объектов, а более специфичный chord не запускает обычный `Alt` cycle.
- [x] Для группы сохраняется внутренний порядок; объекты не переносятся через границу layer.
- [x] Reorder записывается append-only metadata-командой `UUID → paint_order` без копирования геометрии; undo/redo и reopen сохраняют UUID, точки и слой.
- [ ] `Ctrl+Shift+колесо` дополнительно рассмотреть как `Bring to front`/`Send to back`, если это не конфликтует с будущими настраиваемыми shortcuts.
- [ ] Точное перемещение выделения числовыми X/Y-значениями.
- [ ] Перемещение на очень больших BigInt-координатах без преобразования всей позиции в `f64`.
- [ ] Перенос выделенного на другой depth с явно определённым сохранением визуального размера.
- [x] `Ctrl+C` сохраняет immutable snapshot выбранных векторных операций во внутренний clipboard без изменения документа.
- [x] `Ctrl+X` сначала сохраняет тот же snapshot, затем удаляет выбранные операции одной grouped transaction; Undo возвращает вырезанное, clipboard остаётся доступным.
- [x] `Ctrl+V` создаёт новые UUID одной grouped transaction в текущем active layer; для вставки на другой слой пользователь выбирает его перед Paste.
- [x] `Ctrl+Shift+V` выполняет Paste in place; обычный Paste добавляет нарастающий offset `16 px` и сохраняет взаимный paint order.
- [x] Copy не создаёт history step; Cut и каждое Paste отменяются/возвращаются одним undo/redo, Paste переживает reopen.
- [x] После Paste новые операции остаются выделенными; locked/hidden destination layer отклоняет Paste без изменения документа.
- [ ] Системный clipboard и перенос между разными документами оставить отдельным подпунктом после стабильной внутренней версии.

#### A.6. Постоянная Area selection как clipping mask

**Сложность: очень высокая.**

- [x] Rectangle и свободное Lasso после release преобразуются из screen-space в canvas/depth geometry и остаются на месте при pan/zoom/depth navigation.
- [x] Показать постоянный пунктирный контур и bounds; `Esc`/Deselect снимает область, не изменяя документ.
- [ ] Первая версия использует один Replace-контур; затем добавить `Shift Add`, `Alt Subtract` и `Intersect` с валидной multi-contour geometry.
- [x] Area selection является временной UI mask, а не layer mask и не отдельным объектом документа; M5.2b подключил clipping новых Brush/Eraser/Fill при commit.
- [ ] Brush, Eraser, Fill и Gradient не создают содержимое за пределами активной области.
- [ ] Clipping выполнять при commit: stroke может стать несколькими векторными fragments одной transaction, Fill/Erase/Gradient получают валидный polygon clip.
- [x] Изменение одной только Area selection не меняет document revision и не запускает tile rebuild; document revision меняется только при commit операции, которая попала в Area clipping.
- [ ] Ограничить область active visible unlocked layer; режим `All unlocked layers` рассматривать отдельно для destructive команд.
- [ ] Проверить Rectangle/Lasso на разных depth, огромных BigInt tile X/Y, self-intersection, holes, undo/redo и reopen результата операций.

#### A.7. Операции внутри Area selection и Gradient

**Сложность: высокая.**

- [ ] `Fill selection` создаёт одну непрозрачную polygon Fill operation текущим цветом.
- [ ] `Gradient` первой версии: непрозрачный linear gradient с двумя color stops и интерактивными start/end handles.
- [ ] Затем добавить radial gradient; angle/conical и произвольное число stops оставить будущим расширением.
- [ ] Gradient хранится как векторная operation: clip polygon, тип, start/end canvas points и color stops; не сохранять результат готовым bitmap.
- [ ] Fallback, tile rasterizer и будущий export должны давать одинаковое направление, цвета и clipping на любом depth.
- [ ] Добавить `Erase inside`, `Stroke boundary` и `Invert selection`; Copy/Cut Area реализовать после надёжного clipping исходных векторов.
- [ ] Export image может использовать bounds активной Area selection как готовую область экспорта.
- [ ] Opaque gradient реализовать независимо от alpha; прозрачные stops, feather/soft edge и layer masks отложить до нового compositing path.
- [ ] Для снижения banding рассмотреть bounded dithering в raster/export без изменения авторитетной gradient operation.

#### A.8. Transform

- [ ] Показать общий transform bounding box с угловыми/боковыми handles и отдельной ручкой поворота.
- [ ] Масштабирование за handles; `Shift` сохраняет пропорции, `Alt` масштабирует относительно центра.
- [ ] Поворот вокруг центра или перемещаемой опорной точки; `Shift` включает дискретный шаг угла.
- [ ] Добавить Flip Horizontal/Vertical без растеризации в bitmap.
- [ ] Числовые поля position, width/height и angle; единицы явно привязать к текущему camera depth.
- [ ] Live preview не изменяет исходные операции; release записывает одну replacement transaction с новыми UUID и сохранённым paint order.
- [ ] Locked/hidden layer отклоняет transform, а выделение с нескольких layers не допускается до отдельного cross-layer режима.
- [ ] Ограничить и тестировать transform для очень плотных операций.
- [ ] Undo/redo, reopen и extreme BigInt coordinates дают идентичный результат без накопления ошибок после повторных transform.

#### A.9. Критерии готовности

- [ ] Rectangle и lasso корректно выбирают Brush/Fill/Eraser.
- [ ] Delete, duplicate, move, scale и rotate являются отдельными атомарными undo/redo steps.
- [ ] Результат сохраняется после reopen и восстановления backup.
- [ ] На большом документе простое выделение не запускает массовый rebuild и не блокирует UI.

### Поток B. Стирающее лассо

**Сложность потока: высокая.**

Этот этап использует capture/hit-test/preview из Приоритета 1, но создаёт destructive polygon operation вместо выбора объектов.

#### B.1. Семантика

- [x] Добавить отдельный `Eraser Lasso`/`EraseArea`, не смешивая его с обычным stroke Eraser.
- [ ] Объединить Lasso Fill и Eraser Lasso в один `Area` tool с общей capture/preview-геометрией и двумя режимами `Fill`/`Erase`.
- [x] Первое `X` активирует Area Fill, повторное `X` циклически переключает `Fill`/`Erase`; прежний `L` оставлен alias для прямого входа в Fill.
- [ ] Явно показывать текущий Area mode цветом preview, toolbar-сегментом и status, чтобы destructive Erase нельзя было спутать с Fill.
- [x] Замкнутый контур стирает более старое содержимое внутри области.
- [ ] До появления layers воздействовать на весь видимый документ; после layers по умолчанию только на active layer.
- [ ] Определить переключатель `Active layer` / `All unlocked layers`.
- [x] Не разрезать исходные векторы в первой версии: использовать воспроизводимую destructive mask operation.

#### B.2. Input и preview

- [ ] Повторно использовать непрерывный lasso input, mouse history и bounded interpolation.
- [ ] Показывать полупрозрачный preview области без запуска полного tile rebuild во время жеста.
- [x] Очищать временный контур Eraser Lasso сразу после release.
- [x] Отменять незавершённое лассо через `Esc`.
- [x] Не сохранять контур короче трёх различных точек или с вырожденной площадью.

#### B.3. Рендер и persistence

- [x] Добавить polygon erase в vector fallback и tile rasterizer.
- [ ] Поддержать extreme depth/coordinates и clipping по tile bounds.
- [x] Сохранить polygon erase как один атомарный undo/redo step, проверить reopen и cache revision.
- [x] Автотестом проверить перекрытие старого содержимого polygon erase в тайле.

#### B.4. Критерии готовности

- [ ] Brush, Fill и предыдущие erase-операции корректно стираются внутри контура.
- [ ] Быстрое круговое движение не превращается в треугольник.
- [ ] Undo/redo и reopen дают идентичный результат.

### Поток C. Слои, близкие к Photoshop

**Сложность потока: очень высокая.**

Depth остаётся пространственным масштабом и не заменяется слоями. Layers становятся отдельной осью организации и compositing.

#### C.1. Модель и миграция

- [x] Добавить стабильный `layer_id` и отдельную таблицу layers.
- [x] Старые документы автоматически получают один `Background`/`Layer 1` без изменения геометрии.
- [x] Каждая новая Brush/Eraser/Fill/Eraser Lasso операция записывается в active layer.
- [x] Layer stack управляет порядком рендера между слоями, `paint_order`/sequence — порядком операций внутри слоя.
- [x] Добавить версию формата и проверяемую миграцию с backup.

#### C.2. Базовая панель Layers

- [x] Компактное окно со списком слоёв сверху вниз и числом операций.
- [x] Создание нового верхнего слоя и переименование active layer.
- [x] Дублирование слоя: новый layer ID и новые UUID всех эффективных операций, сохранение внутреннего порядка и один undo/redo step.
- [x] Удаление пустого слоя; запрет удаления последнего оставшегося слоя.
- [x] Выбор active layer.
- [x] Reorder кнопками `Up`/`Down`; drag-and-drop оставить улучшением UI.
- [x] Visibility и lock: скрытые слои исключаются из fallback и tile render, hidden/locked active layer нельзя редактировать.
- [x] Перемещение Object selection на другой видимый незаблокированный слой через `Move selection to`: одна tombstone/replacement transaction, новый active layer и сохранённое выделение replacements.
- [x] Подтверждение удаления непустого слоя с удалением содержимого; Undo восстанавливает слой и операции.
- [ ] Перенос содержимого удаляемого слоя на другой слой добавить вместе с cross-layer Move в M4.
- [x] При удалении active layer детерминированно активировать ближайший верхний, иначе ближайший нижний слой.
- [ ] Добавить drag-and-drop reorder и перенос выделения на конкретный слой через компактную команду `Move to layer`.
- [ ] Для длинного плоского списка добавить прокрутку/виртуализацию и возможность свернуть сам список без изменения visibility слоёв.
- [ ] Разделить `active layer` и `selected layers`: active всегда один и принимает новые Brush/Eraser/Fill operations, selected set используется только bulk-командами Layers.
- [ ] Обычный клик выбирает одну строку и делает её active; `Ctrl+клик` переключает отдельные строки, `Shift+клик` выбирает непрерывный диапазон от anchor.
- [ ] Active layer имеет отдельный явный индикатор и всегда входит в selected set; пустой selected set не допускается, пока существует хотя бы один слой.
- [ ] Up/Down и будущий drag-and-drop перемещают selected layers как стабильный блок, сохраняя их внутренний порядок.
- [ ] Bulk visibility/lock применяются ко всему набору; Delete/Duplicate выполняются одной compound history transaction и соблюдают запрет удаления последнего слоя.
- [ ] Destructive bulk-команда отклоняется до commit, если набор содержит неподходящий locked/hidden layer; частичное выполнение запрещено.
- [ ] Первая версия Merge Selected объединяет только непрерывный диапазон в нижний слой диапазона; несмежный набор требует явного подтверждения или остаётся недоступным.
- [ ] Undo/redo, reopen и замена redo-ветки полностью восстанавливают selected-layer bulk changes; само временное selected UI state в документ не записывается.

#### C.3. History, spatial index и tiles

- [x] Create/rename/reorder/visibility/lock имеют общую undo/redo timeline вместе с рисованием.
- [x] Spatial query исключает hidden layers перед fallback и tile render.
- [ ] Tile cache identity должна учитывать порядок, visibility, opacity и blend state.
- [ ] Изменение одного слоя инвалидирует только затронутые тайлы.
- [ ] Bookmarks и camera остаются независимыми от active layer.

#### C.4. Photoshop-подобные операции

- [x] `Merge Down`: active layer объединяется с непосредственным нижним, визуальный порядок сохраняется, верхний слой удаляется одним compound undo/redo step; автоматические проверки пройдены.
- [x] Merge недоступен для hidden/locked source или destination и для самого нижнего слоя.
- [x] После Merge active становится итоговый нижний слой; reopen/undo/redo и замена redo-ветки проверены автоматически.
- [x] Базовая cross-layer Move transaction реализована отдельно и готова к переиспользованию в `Merge Down`.
- [ ] Flatten visible copy без потери исходного документа до подтверждения.
- [ ] Группы/folders: отдельный стабильный group ID, вложенный порядок и перенос слоёв внутрь/наружу группы.
- [ ] Сворачивание folder скрывает только строки дочерних слоёв в UI и не меняет их visibility или результат рендера.
- [ ] Visibility/lock группы применяются предсказуемо к дочерним слоям; undo/redo и reopen сохраняют иерархию и состояние раскрытия.
- [ ] Ограничить первую версию одним уровнем folders; произвольную глубокую вложенность добавлять только при реальной необходимости.
- [ ] Опциональные thumbnails, создаваемые в фоне.
- [ ] Layer masks отложить до отдельного этапа после стабильных transform, lasso selection и нового alpha/compositing path.
- [ ] Для masks потребуются отдельный edit target, чёрно-белая mask geometry, preview, history, локальная tile invalidation и одинаковый результат в fallback/tile/export.

#### C.5. Отложенная прозрачность

- [ ] Объединить в один будущий этап прозрачность палитры/кисти, точный alpha-compositing и opacity слоя в режиме `Normal`.
- [ ] Layer masks выполнять на той же проверенной compositing-инфраструктуре, а не возвращать отдельный медленный CPU path.
- [ ] Blend modes исключены из текущего плана; возвращаться к ним только по отдельному запросу после стабильной прозрачности.
- [ ] Не возвращать старый CPU transparent path со штрихами и падением FPS.

#### C.6. Критерии готовности

- [ ] Старые и новые документы открываются без потери содержимого.
- [ ] Visibility, lock, reorder, merge и перенос выделенного переживают reopen.
- [ ] Layer changes не вызывают rebuild всего бесконечного документа.
- [ ] Opacity/blending совпадают в live fallback, готовых тайлах и export.

### Интеграционные checkpoints основной задачи

#### M1. Общие контракты без изменения поведения

- [x] Default layer migration, grouped transaction metadata, reusable lasso geometry, UUID spatial queries и affected-tile coverage.
- [x] Вручную подтверждено: schema v1 документ после миграции открывается и рисуется без визуальных изменений, backup создан, исправленный undo/redo работает без прежней сильной задержки.

#### M2. Первый общий UI

- [x] Rectangle selection overlay выбирает видимые незаблокированные операции по UUID через spatial broad phase и точный screen-space hit test.
- [x] Базовая read-only Layers panel показывает порядок, активный слой, visibility, lock и число операций.
- [x] Eraser Lasso собирает ограниченный по плотности замкнутый preview без destructive commit.
- [x] Все три функции являются временным UI-состоянием и не изменяют revision, SQLite-операции или tile generation.
- [x] Ручная проверка M2: rectangle selection, перемещение preview вместе с видом, Eraser Lasso и окно `Layers` работают.

#### M3. Первые сохраняемые операции

- [x] Checkpoint 1: persisted Delete selected через append-only tombstone без перезаписи исходных operation payloads.
- [x] Checkpoint 2: drag Move selected как одна атомарная tombstone/replacement transaction.
- [x] Corrective checkpoint: single-object click selection, overlap cycling, modifiers и Inside/Crossing rectangle без разрезания операций.
- [x] Checkpoint 3: persisted polygon `EraseArea` реализован; автоматические проверки UI-контура, raster, reopen и undo/redo пройдены, ожидается ручная проверка.
- [x] Checkpoint 4.1: create/rename/Up/Down, active-layer drawing, layer-aware render order и reopen реализованы; ожидается ручная проверка.
- [x] Checkpoint 4.2: schema v3, visibility/lock и единый undo/redo layer metadata реализованы и проверены вручную.
- [x] Checkpoint 4.3: duplicate/delete layer поверх общей layer-history инфраструктуры реализован; ожидается ручная проверка.
- [ ] Каждая команда атомарна для undo/redo и reopen.

#### M4. Совместная работа

- [x] Selection учитывает active/visible/locked layer и не затрагивает операции других слоёв.
- [x] Checkpoint 4.4: внутренние Cut/Copy/Paste для Object selection и вставка в active layer реализованы и проверены вручную.
- [x] Checkpoint 4.5: изменение `paint_order` Object selection через `Ctrl+колесо` реализовано и проверено вручную.
- [x] Checkpoint 4.6a: перенос Object selection между слоями на общей grouped-history модели реализован и проверен вручную.
- [x] Checkpoint 4.6b: `Merge Down` реализован через расширенный backward-compatible layer-history payload и проверен вручную.
- [ ] Eraser Lasso поддерживает `Active layer` и `All unlocked layers`.
- [ ] Выделенные операции можно переносить между слоями.
- [ ] Copy/Paste выделенных операций вставляет копии в active layer, включая сценарий копирования с одного слоя и вставки на другой.
- [ ] Tile invalidation остаётся локальной.

#### M5. Расширенное редактирование

- [x] Checkpoint 5.0: toolbar разгружен, окна `File/Help`, `Navigation`, обновлённые `Bookmarks`, `Settings > Display` и всплывающее меню `Selection` реализованы и проверены вручную.
- [x] Checkpoint 5.0a: Inside/Crossing и Cut/Copy/Paste/Paste in place/Delete перенесены в `Selection` popover; Add с полем имени перенесён в Bookmarks. Проверено вручную.
- [x] Checkpoint 5.0b: New/Open перенесены в окно `File` и проверены вручную; F1 открывает Help без дублирующих кнопок. Подробные вкладки `Русский/English` загружаются из внешних JSON-каталогов с встроенным fallback и проверены вручную.
- [x] Checkpoint 5.0c: окно `Navigation` проверено вручную; быстрый доступ `Bookmarks` оставлен и в toolbar, и внутри Navigation.
- [x] Checkpoint 5.0d: `Settings > Display` и настраиваемый canvas overlay проверены вручную.
- [x] Checkpoint 5.0e: при активном Selection ПКМ открывает существующее окно команд `Selection` в позиции курсора; текущий selection сохраняется, клик по команде не запускает rectangle/move/scale. Проверено автоматически и вручную.
- [x] Checkpoint 5.0f: при активном Brush или Fill ПКМ открывает компактное tool menu с inline RGB-палитрой и общим `Size`; controls используют те же `color`/`brush_size`, что toolbar, и вторичная кнопка не начинает stroke или lasso. Вложенный color popup заменён inline picker и проверен вручную.
- [ ] Checkpoint 5.1: общий transform box, scale, rotate и Flip для Object selection.
  - [x] M5.1a: bounding box, corner-scale, live preview и одна grouped history transaction. Проверено автоматически и вручную.
    - [x] Общий bounding box учитывает stroke width и показывает четыре фиксированных corner handles.
    - [x] Scale пропорционален относительно противоположного угла; preview не меняет документ до release, `Esc` отменяет жест.
    - [x] Release создаёт одну append-only tombstone/replacement transaction с сохранением layer, paint order, kind, color и Fill/EraseArea closure.
    - [x] Автотесты покрывают Undo/Redo/reopen, extreme BigInt, locked/mixed-layer rejection и отказ без частичного commit; полный набор: `185 passed`.
    - [x] Вручную подтверждены увеличение/уменьшение, handles, `Esc`, Move, Undo/Redo и reopen.
  - [x] M5.1b: rotate handle, Flip Horizontal/Vertical и общие transform-команды. Проверено автоматически и вручную.
    - [x] Верхний круглый rotate handle поворачивает выделение вокруг центра; `Shift` включает шаг 15°.
    - [x] Flip Horizontal/Vertical доступны в toolbar Selection и ПКМ Selection menu и используют ту же grouped replacement transaction без bitmap-растеризации.
    - [x] Preview не меняет документ; release/Flip сохраняют layer, paint order, kind, color, width и selected replacement UUID.
    - [x] Автотесты покрывают rotate handle/snap/bounds, rotate Undo/Redo/reopen и Flip H/V без изменения width; полный набор: `189 passed`.
    - [x] Минимальная ручная проверка rotate/Flip подтверждена пользователем.
  - [x] M5.1c: `Recolor selected` через текущую палитру для Paint/Fill как одна grouped replacement transaction с сохранением geometry/layer/paint order и поддержкой Undo/Redo/Reopen. Проверено автоматически и вручную.
    - [x] Команда доступна в toolbar Selection и ПКМ Selection menu.
    - [x] Paint/Fill получают текущий непрозрачный RGB цвет; Erase/EraseArea остаются без изменений и сохраняются в выделении.
    - [x] Recolor пишет одну append-only tombstone/replacement transaction, сохраняет geometry, width, layer, paint order и selected replacement UUID.
    - [x] Автотесты покрывают Paint/Fill recolor, Erase exclusion, отсутствие history для erase-only selection, locked/stale/mixed-layer rejection и Undo/Redo/Reopen; полный набор: `192 passed`.
    - [x] Минимальная ручная проверка Recolor selected подтверждена пользователем.
- [ ] Checkpoint 5.2: постоянная Rectangle/Lasso Area selection и clipping Brush/Eraser/Fill.
  - [x] M5.2a: постоянный Area selection контур без clipping-коммита. `Object/Area` и `Rectangle/Lasso` доступны в Selection menu и ПКМ; контур хранится как временная canvas/depth geometry, следует за pan/zoom/depth navigation, очищается через `Esc`/Deselect и не меняет document revision/tile rebuild. Проверено вручную.
  - [x] M5.2b: clipping Brush/Eraser/Fill по активной Area selection как отдельный grouped commit. Brush/Eraser режутся по screen-space centerline; Fill/EraseArea после correction сохраняют один чистый Area/intersection polygon для покрытых/выпуклых случаев, чтобы не получать feather/fan fragments. Saved fallback и raster tiles для Fill/EraseArea больше не сглаживают committed polygon corners, поэтому Area-углы остаются острыми. Area selection остаётся временным UI-состоянием и не записывается в документ. Проверено вручную.
- [ ] Checkpoint 5.3: opaque linear Gradient, Fill/Erase/Stroke boundary внутри Area selection; radial gradient вторым подпунктом.
- [ ] Checkpoint 5.4a: выбор нескольких слоёв через Ctrl/Shift и атомарные bulk visibility/lock/reorder/delete/duplicate.
- [ ] Checkpoint 5.4b: folders/groups, сворачивание дерева и drag-and-drop Layers поверх multi-layer selection.
- [ ] Checkpoint 5.5: Select All/Deselect/Invert на active layer, свободное Object-lasso и Bring to Front/Send to Back.
- [ ] Layer masks и opacity подключаются только на отложенном новом alpha/GPU path; blend modes не входят в текущий scope.

#### M5.0. Разгрузка интерфейса перед новыми инструментами

- [ ] Оставить в верхней панели инструменты, brush size/color, Undo/Redo и кнопки открытия основных окон; не добавлять туда новые длинные ряды команд.
- [x] Первая версия `Selection` popover содержит `Inside/Crossing` и Cut/Copy/Paste/Paste in place/Delete; `Object/Area`, `Rectangle/Lasso`, Move to layer и wheel-free buttons добавляются вместе с соответствующими инструментами.
- [x] `Add bookmark` и поле имени перенесены внутрь окна `Bookmarks`; там же сохранены Open/Rename/Delete.
- [x] Создано окно `File` с вкладками `File` и `Help`: New/Open и текущий document path работают, а F1 открывает прокручиваемую справку по инструментам, жестам, Selection, слоям, навигации, истории, файлам и всем текущим настройкам производительности.
- [x] Справка вынесена в `help/*.json` рядом с exe: `ru/en` имеют встроенный fallback, внешний файл заменяет язык по `id`, новый уникальный `id` автоматически добавляет вкладку.
- [ ] Save As/Export/Recent добавлять в File только одновременно с рабочей реализацией; текущие JSON-описания позже подключить к общему command/setting registry.
- [x] Создано отдельное перемещаемое окно `Navigation`: current depth/zoom, прямой Depth jump, Tile X/Y jump, Origin, точные прокручиваемые Tile/Local coordinates и переход в Bookmarks. Старые navigation rows и координаты удалены из постоянного toolbar/overlay; Bookmarks также оставлен отдельной toolbar-кнопкой.
- [x] Управление canvas overlay перенесено в `Settings > Display`: master switch и независимые toggles для Depth, Zoom, Tile X/Y, Local X/Y, operation count, FPS/frame time, status и tile/rebuild state.
- [x] Добавлены presets `Minimal`, `Standard`, `Diagnostics`; пользовательские комбинации определяются как `Custom` и сохраняются в `local/settings.json`.
- [ ] Все окна перемещаемые и сохраняют разумное положение/открытое состояние; на маленьком экране текст и controls не перекрывают canvas.
- [ ] Wheel-команды в Selection popover получают кнопки Up/Down или Previous/Next, чтобы ими можно было пользоваться стилусом даже без клавиатуры.

### Организация параллельной работы

- Поток A может развивать selection UI и transforms после стабилизации UUID hit results.
- Поток B может параллельно подключать тот же lasso capture к destructive polygon rasterization.
- Поток C может параллельно делать schema migration и Layers panel.
- После каждого milestone выполняется общая проверка A+B+C; несовместимые локальные модели history или layer scope не допускаются.
- Реализация всё равно поставляется короткими checkpoints, чтобы каждую рабочую часть можно было проверить вручную.

## Расширение профилей настроек

- [ ] Расширить `Performance/Balanced/Quality`, чтобы они управляли почти всеми настройками качества, производительности и плотности input, а не только tile resolution/workers/zoom settle.
- [ ] Включить Brush/Fill input spacing, Fill fallback limits, automatic drawing pause, deferred preview, rebuild policy, prefetch, PNG compression, preview FPS, edge quality и smoothing.
- [ ] Оставить независимыми ручной `Pause tile generation`, `Cache size MiB` и весь `Settings > Display`, поскольку это оперативное состояние, аппаратный бюджет и UI-предпочтения.
- [ ] До реализации записать точную матрицу значений `Performance/Balanced/Quality`; `Balanced` сохраняет текущие безопасные defaults.
- [ ] Определять `Custom` по всем управляемым полям и автоматически возвращать preset при полном совпадении.
- [ ] Переиспользовать существующие runtime reactions для workers, tile generation и cache namespaces; применение профиля не меняет `.esketch`.
- [ ] Покрыть preset matrix, migration, normalization, JSON round-trip и сохранение неуправляемых полей автоматическими тестами.
- [ ] После реализации обновить Settings Help и `docs/settings-reference.md`, затем вручную проверить каждый preset и переходы `preset → Custom → preset`.

## Стабилизация ввода и FPS под нагрузкой

**Сложность: высокая. Выполнить после ближайшего блока Layers и до экспорта.**

На насыщенном холсте с высоким качеством подтверждены подвисания UI. Во время подвисания Z + ЛКМ может кратко изменить направление, не сработать или начать Brush; колесо иногда не даёт видимого шага; pan иногда пропускает движение. Несколько быстрых отдельных штрихов могут объединиться в «галочку», потому что текущая обработка одного кадра сохраняет движение мыши, но сводит несколько `press/release`-циклов к одному draft.

### S1. Измеряемое воспроизведение

- [ ] Добавить раздельные p50/p95/p99/max для frame, input, fallback paint, tile submit/result upload и UI tessellation.
- [ ] Сделать debug stress mode с искусственной задержкой UI-потока `50/100/200/300 ms`, не меняя release defaults.
- [ ] Считать входные `press/move/release`, wheel events, созданные strokes и случаи смены gesture mode.
- [ ] Воспроизвести и измерить Fill-регрессию: при быстрых движениях контур/preview Lasso Fill снова может перестать дорисовываться, хотя Area selection работает нормально.
- [ ] Сохранить контрольный насыщенный документ и одинаковые High Quality настройки для повторных замеров.

### S2. Последовательная очередь событий

- [ ] Заменить один агрегированный `primary_pressed/primary_released` на обработку `PointerButton` и `PointerMoved` строго в порядке `egui::Event`.
- [ ] Поддержать несколько полных `press -> move -> release` циклов за один UI-кадр; каждый цикл создаёт отдельный stroke.
- [ ] Для Fill проверить, что быстрые `PointerMoved`/mouse-history samples не отбрасываются preview-веткой и всегда попадают в текущий lasso draft до release.
- [ ] Mouse history использовать только для восстановления промежуточных координат внутри текущего нажатия, не для границ нажатия.
- [ ] Не соединять release одного stroke с press следующего линейной интерполяцией.

### S3. Зафиксированный режим жеста

- [ ] На каждом primary press выбрать один режим: Draw, Z Zoom, Space Pan, brush resize или Selection, и удерживать его до соответствующего release.
- [ ] Учитывать Key Z/Space/Ctrl и PointerButton в порядке событий одного кадра, а не только итоговое `key_down`.
- [ ] Z Zoom и Space Pan всегда потребляют свой жест и не могут оставить случайный Brush draft.
- [ ] Zoom/pan считать из последовательных event positions; большая задержка кадра не должна терять delta или менять его знак.

### S4. Колесо без пропусков

- [x] Raw wheel zoom checkpoint manually confirmed: обычный canvas zoom теперь обрабатывает сырые `MouseWheel` events по порядку вместо одного `smooth_scroll_delta` кадра; line-wheel применяет каждое физическое деление отдельно, point/trackpad delta накапливает дробный остаток между кадрами.
- [x] Накапливать дробные trackpad delta отдельно; каждое физическое деление обычного колеса применять ровно один раз.
- [ ] Не терять wheel input во время отмены устаревшей tile generation и zoom settle.

### S5. Снижение нагрузки взаимодействия

- [ ] Профилировать High Quality отдельно для активных tile workers, vector fallback, texture upload и egui tessellation.
- [x] Tile phase diagnostics checkpoint: session log frame phases теперь разделяют `tile_total`, `tile_collect_upload`, `tile_request_queue` и `tile_draw`, а `tiles` payload содержит lightweight counters `uploaded_textures` и `queued_jobs`; это помогает отличить texture upload/queue pressure от vector fallback и input latency без изменения рендера.
- [x] Session log perf merge follow-up: after manual log review, `slow_phase`, `fps_drop`, and aggregate perf payloads now merge with the existing frame `perf` object instead of replacing it. Future slowdown events keep fallback shape/segmented/fast counters next to phase or frame timing, making vector fallback cost easier to attribute.
- [x] Bounded tile queue follow-up: after the next manual log showed `tile_request_queue` stalls while enqueueing 42 jobs in one UI frame, new tile job submission is capped to a small per-frame batch while preserving visible-first order. Missing tiles continue over later frames and final tile quality/cache identity are unchanged.
- [x] Incremental commit indexing follow-up: after the bounded-queue log showed repeated `input` stalls around stroke save/commit events, ordinary append-only `CanvasDocument::commit` operations now update spatial/render indexes incrementally. Grouped edits, metadata, compact blocks, replacements, and out-of-order paint order still use the full rebuild path.
- [x] Selection transform indexing follow-up: after the next log showed `selection_transform` input stalls, one-to-one non-CompactBlock move/scale/rotate replacements now update existing spatial/render index slots in place. Unsafe cases still fall back to the full rebuild path, so document storage and render output stay unchanged.
- [x] Auto fallback interaction FPS follow-up: `Stroke fallback joins = Auto` now uses segmented joins only for calm no-interaction/no-pending-tile viewing. During drawing, navigation, selection interaction, or while tile jobs are pending, Auto temporarily uses the fast polyline fallback path; `Quality` and `Performance` keep their explicit behavior.
- [x] Auto no-tile fallback FPS correction: the next log showed manual `Pause tile generation` still counted as calm viewing, so Auto kept segmented joins and full fallback still generated very large shape counts. Auto now also uses the fast polyline path whenever tile generation is paused; `Quality` remains the explicit no-tile quality override.
- [x] Eraser Lasso input-density correction: fast Area Erase gestures now recover Windows mouse-history samples like Brush/Fill, and lasso preview point appending interpolates large remaining gaps while still filtering tiny jitter. This keeps the EraseArea contour less speed-dependent without changing `.esketch`, raster cache identity, tiles, or selection transforms.
- [x] Performance profile expansion: Settings `Performance/Balanced/Quality` now manage the full meaningful speed/quality matrix: Brush/Fill input density, Fill fallback limits, Stroke fallback joins, zoom settle, tile workers/resolution, automatic pause while drawing, deferred preview, rebuild policy, prefetch radius, edge quality, smoothing, PNG compression, and preview FPS. Manual tile pause, cache budget, Display, diagnostics logging, and object-compaction/editability controls remain independent.
- [x] Saved fallback operation budget checkpoint: temporary no-tile saved-vector fallback can skip oldest visible operations before projection when `Saved fallback ops` is set above `0`. The first hidden version was too visually destructive in Performance/Balanced, so settings v15 exposes it explicitly: `0` means Unlimited/full saved fallback, `Performance` preset sets `1500`, and `Balanced`/`Quality` set Unlimited. Session logs include both `saved_fallback_operation_limit` and `fallback_skipped_operations`.
- [x] Fast saved-stroke fallback shape-count checkpoint: `Performance` and `Auto` pressure paths now draw saved strokes as one polyline shape without endpoint caps instead of polyline plus two cap circles. This reduces egui shape count for full no-tile fallback without hiding operations; calm Auto, segmented sparse joins, Quality behavior, stored geometry, and PNG tiles are unchanged.
- [x] Fast saved-stroke fallback smoothing checkpoint: `Performance` and `Auto` pressure paths now also skip saved-stroke smoothing before clipping and draw raw projected saved points. This removes repeated smoothing CPU cost on dense no-tile fallback without hiding operations; Quality, calm Auto, Fill/EraseArea fallback, selection highlights, stored geometry, and PNG tiles are unchanged.
- [x] Fallback phase timing diagnostics checkpoint: session logs now split saved fallback cost into `fallback_project`, `fallback_derive`, `fallback_clip`, and `fallback_shape_paint`, and record projected/painted fallback operation counters. Rendering, input, settings, presets, `.esketch`, tile cache identity, and raster output are unchanged; the next optimization should be selected from these measured costs.
- [x] Saved fallback derived-geometry cache checkpoint: unchanged repaint frames now reuse derived saved fallback points by document revision, camera, viewport, smoothing/fallback settings, fill fallback point budget, and Auto fast-path state. This reduces repeated smoothing/sampling while waiting for tiles without changing clipping, painter submission, visual output, input, settings, `.esketch`, tile cache identity, or raster output. Session logs include derived fallback cache hit/miss counters.
- [x] Saved fallback projection-bypass checkpoint: when derived saved fallback points are already cached for the unchanged frame, full/overlay fallback now skips raw projection and goes straight to clipping/paint. CompactBlock top-level bounds are no longer projected for fallback, and cached compact sources also bypass projection. Visual output, input, settings, `.esketch`, tile cache identity, and raster output are unchanged.
- [x] Saved fallback cache clone-reduction checkpoint: derived fallback cache entries now store shared `Arc<[Pos2]>` point arrays, so cache hits pass slices into clipping/paint instead of cloning full `Vec<Pos2>` arrays. Visual output, input, settings, `.esketch`, tile cache identity, and raster output are unchanged.
- [x] Visible render operation clone-reduction checkpoint: fallback frames now reuse the cached visible render operation list as `Arc<[EditOperation]>` and pass operation indices through the fallback paint queue, avoiding deep operation/CompactBlock source clones on unchanged frames. Visual output, input, settings, `.esketch`, tile cache identity, and raster output are unchanged.
- [x] Saved fallback clipped-geometry cache checkpoint: unchanged saved fallback frames now reuse clipped stroke runs and clipped area polygons from `SavedFallbackRenderCache`, so cache-hit repaints avoid recomputing viewport clipping for every saved operation. Visual output, input, settings, `.esketch`, tile cache identity, and raster output are unchanged.
- [x] Fill fallback shape diagnostics checkpoint: session logs now include `fallback_fill_shape_count`, counting Fill/EraseArea scanline rectangle shapes separately from stroke fallback shapes. Rendering, input, settings, `.esketch`, tile cache identity, and raster output are unchanged; the next paint optimization can distinguish stroke shape pressure from fill scanline pressure.
- [x] Convex Fill fallback single-shape checkpoint: saved Fill/EraseArea fallback now draws convex clipped polygons as one filled egui polygon instead of thousands of scanline rectangles, while non-convex polygons keep the existing scanline path. Stored geometry, input, settings, `.esketch`, tile cache identity, and raster output are unchanged.
- [x] Corrective rollback: convex Fill/EraseArea single-shape fallback caused cross-canvas line artifacts and zoom-dependent red-line thickness, so saved area fallback is back on the proven scanline renderer. `fallback_fill_shape_count` diagnostics remain; stored geometry, input, settings, `.esketch`, tile cache identity, and raster output are unchanged.
- [x] Saved fallback revision-survival checkpoint: derived/clipped saved fallback cache entries now survive document revision-only changes when camera, viewport, smoothing/fallback settings, fill fallback budget, and Auto fast-path state are unchanged. Old unchanged operations reuse their operation-keyed cache entries after a new commit; new/replaced operations miss and populate normally. Rendering, input, settings, `.esketch`, tile cache identity, and raster output are unchanged.
- [ ] Во время Draw/Zoom/Pan приостанавливать новые тяжёлые jobs и texture upload по frame budget; готовое качество восстанавливать после idle.
- [ ] Добавить cooperative cancellation длинной растеризации устаревшего поколения, если profiling подтвердит конкуренцию CPU.
- [ ] Отдельно профилировать Selection на слоях с большим числом объектов: hover hit test, projection, полную подсветку выбранной геометрии и egui tessellation.
- [ ] Пересчитывать hover-кандидатов только при движении курсора/камеры/revision, кешировать projected selection overlay и отсекать невидимые операции spatial index.
- [x] Исправить деградацию saved vector fallback после cross-depth навигации: projected fallback cache теперь сбрасывается при zoom/depth changes и depth-смена в `ProjectedGeometryCache` пересчитывает геометрию из исходных операций, а не масштабирует уже кешированные projected points. Автотесты покрывают `-4 -> -20 -> -4`; ручная проверка показала нормальное поведение с `Smoothing=Strong` и `Pause tile generation`.
- [x] Проверить, почему при zoom/depth transitions линии на дальних глубинах заметно меняют форму: причина зафиксирована как navigation-history dependent projected cache; depth/zoom transitions больше не должны менять экранный контур через перенос старых projected points.
- [x] Профилировать падение FPS до ~4 при отдалении/приближении: первый corrective step выполнен и проверен вручную — same-depth zoom больше не очищает projected fallback cache на каждом кадре, depth-change reset остаётся внутри `ProjectedGeometryCache`. Если просадки вернутся, отдельно замерить full vector fallback, tile request cancellation, retained texture drawing, texture upload и egui tessellation.
- [x] Corrective checkpoint pending manual confirmation: raw/live Fill draft больше не ограничен `Fill fallback points`; append/interpolation продолжают принимать новые точки сверх fallback budget и не мутируют уже собранный контур. После release saved Fill получает производное fallback-представление в пределах `Fill fallback points`, поэтому большой быстрый Fill должен быть виден до готовности PNG-тайла без изменения сохранённого raw contour. Автотесты покрывают raw append сверх budget и bounded saved fallback; M5.2a Area selection не менялась.
- [x] Corrective checkpoint pending manual confirmation: no-tile saved vector fallback больше не применяет screen-space `0.25 px` simplification к сохранённым объектам и не сглаживает Brush после viewport clipping. Это стабилизирует форму дальних объектов при быстрых zoom/depth changes; Fill fallback теперь сэмплирует исходный порядок точек без предварительного screen-space simplify. Автотесты покрывают стабильный clipping/source-point path и Fill sampling без screen-space simplification.
- [ ] Добавить low-end/slow-HDD stability check: на слабом компьютере с медленным диском пользователь наблюдал зависание и системное окно `WerFault.exe` с ошибкой запуска приложения `0xc000012d`; поскольку на этой машине зависало и остальное, не считать это подтверждённым багом приложения, но при повторе собрать Windows Event Viewer/WER details, параметры документа/настроек, состояние диска/CPU/RAM и поведение под искусственным I/O starvation.
- [ ] Для очень больших selection рассмотреть упрощённый bounding-box preview вместо полной подсветки каждой линии во время взаимодействия.
- [ ] Не снижать итоговое качество тайлов: допустимо только временное упрощение интерактивного preview.
- [ ] Нативную timestamped очередь `WM_INPUT`/`WM_POINTER` добавлять только если упорядоченные egui events всё ещё теряются.

### S6. Критерии готовности

- [ ] Серия из 100 быстрых отдельных линий даёт 100 strokes без «галочек» при искусственных задержках до 300 ms.
- [ ] 100 жестов Z + ЛКМ не создают Brush operations, не меняют направление и не теряют release.
- [ ] Wheel и pan дают воспроизводимый суммарный сдвиг при той же задержке.
- [ ] Deferred drawing остаётся дополнительным режимом снижения нагрузки, а не условием корректного press/release.
- [ ] Быстрый Lasso Fill сохраняет и показывает непрерывный контур preview без пропавших участков при обычных и восстановленных mouse-history событиях.
- [ ] High Quality после завершения взаимодействия даёт тот же результат, что и до оптимизации.
- [ ] При `Pause tile generation` сохранённый vector fallback со `Smoothing=Strong` после переходов между далёкими depth совпадает с результатом свежей raster tile generation и не требует ручного переключения Smoothing.
- [ ] Во время изменения масштаба дальняя геометрия сохраняет форму в пределах ожидаемой screen-space погрешности; pan/zoom/depth history не меняет кривизну сохранённых линий.
- [ ] На слабом ПК/медленном HDD приложение либо остаётся управляемым при I/O starvation, либо отказ фиксируется диагностически: WER/Event Viewer, app logs при наличии, параметры документа/настроек и системная нагрузка позволяют отделить системный сбой от дефекта EndlessSketch.
- [x] Timeline session logging checkpoint: `Settings > Diagnostics > Session logging` writes retained JSONL files under `local/logs`, `F12`/`Mark log` creates numbered markers, and automatic events capture navigation, tile/fallback state, document edits, FPS drops, and frame phase costs for FPS/zoom debugging.
- [x] Marker 3 follow-up: Selection highlight/bounds now reuse saved fallback render geometry instead of raw points, so selected sparse lines should not visually disagree with full fallback during zoom; expensive hover hit-testing is skipped during pan/zoom settle and recomputed after navigation.
- [x] Marker 1 compacted-old-lines follow-up: saved vector fallback smoothing now uses scale-stable corner sampling for stored sparse strokes, so sharp bends from early point/smoothing experiments should keep the same shape while zoom changes; stored document geometry and raster tile smoothing remain unchanged.
- [x] Old sparse line raster follow-up: compact/selection are not required for the flicker; cached tile rasterization now keeps sparse Paint/Eraser stroke geometry exact before scale-stable smoothing, only oversized strokes use screen-space simplification, and tile renderer cache version 9 forces affected PNG tiles to rebuild.
- [x] No-tile sparse fallback join follow-up: when tile generation is off, sparse vector fallback now renders stored strokes as per-segment capsule shapes with filled vertex joins instead of one egui polyline join, reducing sharp-corner protrusion/clipping changes during zoom; dense strokes keep the fast single-polyline path.
- [x] Sparse fallback FPS follow-up manually confirmed: `Settings > Stroke fallback joins` now persists `Auto/Quality/Performance` in settings v14. `Auto` is the default: saved sparse strokes use segmented capsule joins up to 128 screen points only when no draft is active, while active drawing forces background saved strokes onto the fast polyline path. `Quality` keeps segmented joins up to 512 points, `Performance` always uses the fast path, and session logs record the join mode plus fallback segmented/fast counters.

### S7. Уплотнение старых объектов

**Сложность: очень высокая; приоритет поднят пользователем до profiling/optimization.**

Технический план:

- [x] C0: зафиксировать контракт `CompactBlock`: это не bitmap flatten. Блок хранит compressed snapshot исходных `EditOperation` и работает как обычный оптимизированный объект; PNG/LOD остаются rebuildable cache. Разбор через пользовательский Unfreeze не входит в продуктовый сценарий, а Undo разъединяет блок только пока сама Freeze transaction ещё находится в обычной истории.
- [x] C0.5: обычный Undo ограничен последними 1000 history transaction groups; старые активные операции остаются содержимым документа, но перестают быть доступными для бесконечного Ctrl+Z.
- [x] C1: добавлен persistable `CompactBlock` operation как совместимое payload-расширение `EditOperation` без schema bump. Команда Freeze создаёт одну grouped transaction: tombstone исходных операций + один compact block replacement с tight bounds/слоем/paint order. Пока Freeze transaction находится в undo history, Undo удаляет block и реактивирует исходные UUID без потери геометрии; после выхода за лимит undo block считается стабильным обычным объектом.
- [x] C2: ручная команда `Freeze/Compact selected` добавлена в Selection menu только для текущего Object selection в active visible unlocked layer. Текущий checkpoint отклоняет пустое, mixed-layer, locked/hidden и metadata/tombstone commands. Разнесённые объекты, mixed-depth объекты и уже compact blocks можно объединять в один block; вложенные blocks при этом flatten в один snapshot.
- [x] C3 partial: vector fallback и tile rasterizer раскрывают `CompactBlock` во внутренние операции; raster equivalence покрыта тестом. Future export должен переиспользовать тот же expansion path.
- [x] C4: spatial/index поведение: top-level index видит block как один объект с tight union bounds; hit test/selection выбирает block как один объект. Внутреннее редактирование содержимого не планируется для обычного пользовательского workflow.
- [x] C5: CompactBlock ведёт себя как обычный объект для основных операций: Move/Scale/Rotate/Flip трансформируют snapshot, Copy/Cut/Paste и Move selection to layer работают с блоком целиком, повторный Freeze flatten-ит вложенные blocks.
- [ ] C6: покрыть оставшимися тестами schema compatibility, layer guards, hidden/locked rejection, cache invalidation revision, delete cache rebuild и selection/hit behavior on block. Уже покрыто: Freeze/Undo/Redo/Reopen, tile raster equivalence, whole-block Move/Scale, Paste, Move-to-layer, повторное уплотнение existing CompactBlock, объединение двух existing CompactBlocks и объединение разнесённых non-contiguous объектов.
- [ ] C7: после ручного режима измерить размер SQLite, open time, top-level operation count, spatial query time, fallback projection time, egui tessellation, tile rebuild. Только после этого рассматривать idle auto-compact thresholds (`Off/500/1000/5000`).
- [ ] C8: auto-compact разрешать только для стабильных операций вне активной истории/selection/draft и только после отдельного проверенного checkpoint.
- [ ] C8a: глобальное уплотнение по счётчику: настройка `Keep latest regular objects` (`100/1000/5000/Unlimited`) оставляет последние N обычных объектов редактируемыми, а более старые операции упаковывает в CompactBlock-группы по layer/depth/spatial region/paint-order range. Не делать один бесконечный block на весь документ; несоединённые объекты объединять через non-rendered bounds geometry внутри CompactBlock, а не видимым helper stroke.
- [x] C8b: первый UI checkpoint для счётчика: `Settings > Keep latest objects` (`Unlimited/5000/1000/100`) и ручная команда `Compact older now`; старые операции сжимаются только непрерывными layer/depth/paint-order runs. Render path раскрывает CompactBlock в source-операции и сортирует их в общем paint order, поэтому уже созданные blocks не должны менять порядок наложения относительно обычных объектов. Раскрытый render-only spatial index хранится в `CanvasDocument` и перестраивается только при изменении операций, а не при каждом zoom/tile query. Auto/idle trigger включать только после ручной проверки.
- [x] C9: улучшить точность пипетки. Picker теперь семплирует верхнюю видимую render-operation под курсором с учётом layers, CompactBlock expansion, Fill/EraseArea, Eraser/background, текущих depth/zoom и stroke radius; `Alt+ЛКМ` берёт цвет без смены текущего инструмента и без history step. Полностью пиксельное семплирование сглаженного tile/fallback можно добавить позже, если ручная проверка выявит расхождение на краях strong smoothing.

## Приоритет 2. Экспорт

### 2.1. Общий offscreen renderer

**Сложность: высокая.**

- [ ] Рендерить документ независимо от размера окна и текущей готовности экранных тайлов.
- [ ] Фиксировать revision документа на время export.
- [ ] Поддерживать camera/depth/BigInt coordinates и тот же порядок слоёв, что на холсте.
- [ ] Рендерить большие изображения полосами/тайлами, не выделяя весь bitmap несколько раз.
- [ ] Добавить progress, cancel, временный файл и атомарное переименование после успеха.
- [ ] Общий renderer использовать и для статической картинки, и для кадров GIF.

### 2.2. Экспорт картинки текущего вида

**Сложность: высокая.**

- [ ] Команда `Export image`.
- [ ] Область: текущий viewport, пользовательский crop rectangle, selection bounds или точные canvas bounds.
- [ ] Размер: width/height в пикселях, сохранение aspect ratio, presets `1x/2x/4x`.
- [ ] Качество: edge quality, supersampling/antialiasing и ограничение памяти.
- [ ] Форматы первой версии: PNG и JPEG; JPEG quality настраивается.
- [ ] Фон: текущий canvas background; transparent PNG включить после корректного alpha engine.
- [ ] Preview итоговой области и оценка размера/памяти до запуска.
- [ ] Сохранять последние export settings отдельно от документа.

### 2.3. Запись перемещения камеры

**Сложность: высокая.**

- [ ] Записывать camera state и timestamps, а не готовые экранные кадры.
- [ ] Записывать pan, zoom, depth jump и Tile X/Y jump на огромных координатах.
- [ ] Зафиксировать revision документа на старте, чтобы анимация не менялась между кадрами.
- [ ] Состояния: idle, countdown, recording, rendering, completed/cancelled.
- [ ] Показать заметный индикатор записи и elapsed time.
- [ ] Добавить кнопки Start/Stop в UI.
- [ ] Добавить команды и горячие клавиши Start/Stop через общий command registry.
- [ ] Проверять конфликты shortcuts; пользовательская перенастройка войдёт в отдельный общий hotkey UI.
- [ ] Режим без горячих клавиш: delay перед стартом, заданная duration и автоматическая остановка.
- [ ] Дополнительный ручной режим: старт кнопкой и остановка кнопкой.

### 2.4. Экспорт анимированной GIF

**Сложность: очень высокая.**

- [ ] После записи офлайн пересчитать кадры с фиксированным FPS, чтобы нагрузка во время движения не давала пропуски.
- [ ] Настройки: width/height, FPS, duration, loop, palette quality и dithering.
- [ ] Корректно обрабатывать 256-цветное ограничение GIF.
- [ ] Интерполировать camera path только там, где это безопасно; сохранять точные BigInt camera samples при больших переходах.
- [ ] Preview первого/последнего кадра и оценка размера файла.
- [ ] Progress/cancel и удаление незавершённого временного файла.
- [ ] Проверить движение в ширину, глубину, комбинированный zoom/pan и возврат по bookmark.
- [ ] После GIF отдельно рассмотреть animated WebP/MP4 как более качественные форматы.

### 2.5. Критерии готовности

- [ ] PNG/JPEG совпадают с выбранной областью и не зависят от размера окна.
- [ ] GIF воспроизводит camera path с фиксированным FPS без пропущенных кадров.
- [ ] Start/Stop работает кнопками и shortcuts.
- [ ] Timed recording полностью работает без клавиатуры.
- [ ] Export можно отменить без повреждённого итогового файла.

## Приоритет 3. Нажим стилуса для кисти

**Сложность: очень высокая.**

Причина: требуется не только получить pressure, но и хранить его в каждой точке, мигрировать старые документы и рисовать variable-width stroke одинаково в live preview, fallback, tiles и export.

### 3.1. Pen input

- [ ] Проверить pressure, pointer type и device ID из текущего winit/egui backend.
- [ ] Если pressure теряется, добавить Windows Ink/`WM_POINTER` adapter рядом с mouse-history adapter.
- [ ] Собирать промежуточные pen samples с timestamp и нормализованным pressure `0..1`.
- [ ] Mouse fallback всегда использует pressure `1.0`.
- [ ] Игнорировать ложные hover-pressure samples.

### 3.2. Формат документа

- [ ] Добавить pressure к точке с backward-compatible default `1.0`.
- [ ] Не менять старые `.esketch` при одном открытии; миграция выполняется безопасно при следующей записи/checkpoint.
- [ ] Проверить размер payload и при необходимости применить bounded quantization.

### 3.3. Brush dynamics

- [ ] Первая версия: pressure управляет только шириной Brush.
- [ ] Настройки: enable, minimum size, maximum size и pressure curve.
- [ ] Сглаживать pressure отдельно от координат без заметной задержки пера.
- [ ] Не менять Fill и mouse behavior.
- [ ] Pressure opacity отложить до точного alpha engine.
- [ ] Pressure для Eraser добавить после стабильной Brush-версии.

### 3.4. Variable-width rendering

- [ ] Live draft без разрывов и острых самопересечений.
- [ ] Saved vector fallback с теми же width samples.
- [ ] Tile rasterizer и offscreen export должны совпадать с live preview.
- [ ] Ограничить tessellation на длинных плотных strokes.
- [ ] Обновить raster cache version.

### 3.5. Проверка

- [ ] Автотесты pressure migration, curve и variable-width geometry.
- [ ] Ручная проверка минимум на одном Windows Ink планшете.
- [ ] Сравнить медленный нажим, быстрый штрих, отрыв пера и mouse fallback.
- [ ] Измерить FPS и размер документа.

## Группировка основной задачи и следующих этапов

Эти задачи удобнее выполнять вместе, чтобы не переписывать одну подсистему несколько раз:

1. **Selection + Eraser Lasso + Layers — одна основная задача.**
   Общие geometry, transaction history, layer scope, spatial filters и tile invalidation создаются один раз, а UI-потоки A/B/C развиваются параллельно.
2. **Layers opacity + accurate transparency + GPU rasterization.**
   Реализация opacity на старом CPU alpha path приведёт к повторной переделке и вернёт проблемы производительности.
3. **Offscreen renderer + image export + GIF.**
   Сначала один детерминированный renderer, затем PNG/JPEG, после этого тот же renderer используется для кадров GIF.
4. **Recording shortcuts + общий command registry.**
   Start/Stop не следует привязывать напрямую к клавишам в export-коде; команды должны позже поддержать общий UI переназначения shortcuts.
5. **Stylus pressure + variable-width GPU path.**
   Pressure capture/storage можно сделать отдельно, но финальную tessellation выгоднее объединить с будущим GPU-native renderer.
6. **Export file dialog + Save As.**
   Общие file picker, overwrite confirmation, progress и atomic output следует вынести в переиспользуемую инфраструктуру.

## Остальной backlog

### Приоритет 4. Save As

**Сложность: средняя.**

- [ ] Копирование активного `.esketch` в новый каталог.
- [ ] Безопасная обработка SQLite/WAL, backups, assets и очищаемого cache.
- [ ] Проверка overwrite, progress, cancel и переключение активного документа.

### Приоритет 5. Импорт изображений

**Сложность: высокая.**

- [ ] PNG/JPEG/WebP как assets документа.
- [ ] Placement, scale, rotate, selection и layers.
- [ ] LOD/cache для больших bitmap.

### Приоритет 6. Общие настраиваемые горячие клавиши

**Сложность: средняя.**

- [ ] Command registry для tools, navigation, selection, layers и export.
- [ ] Проверка конфликтов, reset и сохранение в settings.
- [ ] Default `Shift+N` создаёт и активирует новый верхний слой; команда проходит через registry и может быть переназначена.
- [ ] Общий контракт tool shortcut: первое выполнение активирует tool, повторное выполнение при уже активном tool вызывает `Cycle mode`.
- [ ] `S` циклически меняет Selection `Inside/Crossing`, `Q` циклически меняет Selection `Object/Area`, `X` циклически меняет Area `Fill/Erase`. Локальные `X` и `S/Q` checkpoints реализуются без полного command registry.
- [ ] Тот же механизм использовать для будущих инструментов с конечным набором режимов, без отдельных hardcoded обработчиков клавиш.
- [x] Не обрабатывать tool shortcuts и cycle mode, когда фокус находится в text/numeric input.
- [ ] Command registry предоставляет названия текущего и следующего mode для toolbar, status, context menu и Help.
- [ ] Ни одна wheel-команда не остаётся wheel-only: registry связывает её с wheel binding, `key + pointer drag` gesture и кнопкой/menu fallback.
- [ ] Сохранить существующие `Z+drag` для zoom и `Space+drag` для pan; добавить `Alt+vertical drag` для overlap cycle и `Ctrl+Alt+vertical drag` для selected paint-order steps.
- [ ] Gesture mode фиксируется на pointer down и потребляет движение до release; отпускание modifier в середине не может превратить жест в Brush/Selection.
- [ ] Для дискретных действий использовать настраиваемые `Drag step px`, direction/invert и визуальный счётчик шагов; continuous zoom/pan остаются непрерывными.
- [ ] Любая будущая функция на колесе обязана получить stylus drag и UI fallback в том же checkpoint.
- [ ] Pen barrel button mappings рассмотреть как дополнительный binding после стабильной Windows Ink pressure версии, не как обязательное условие.

### Приоритет 7. Deferred preview guide

**Сложность: средняя.**

- [ ] Лёгкая trailing-линия без обработки и тесселяции полного растущего draft.

### Приоритет 8. Packaging

**Сложность: средняя.**

- [ ] Installer, version metadata, signing и финальная release packaging.
- [ ] Автоматическая сборка portable/release artifacts.

### Приоритет 9. Второстепенный UI polish

**Сложность: средняя.**

- [x] Вынести палитру цвета в отдельное перемещаемое и растягиваемое окно с тем же поведением позиционирования, что у `Settings` и `Layers`; picker внутри окна автоматически следует за шириной окна.
- [x] Сохранить текущий быстрый доступ к активному цвету и не менять color/persistence модель в рамках этой UI-задачи.
- [x] После Palette выполнить tool shortcut cycles: `X` переключает общий Area tool `Fill/Erase`, `S` циклически меняет Selection `Inside/Crossing`, а `Q` — `Object/Area`.
- [ ] После shortcut cycles перейти к уплотнению старых объектов, затем к profiling/optimization.
- [x] Selection-версия контекстного окна по ПКМ выполнена в checkpoint 5.0e и переиспользует команды общего popover.
- [x] В checkpoint 5.0f добавлены для Brush и Fill по ПКМ inline RGB-палитра и общий `Size`, связанные с теми же значениями toolbar; открытие/редактирование menu не создаёт operation или history step.
- [ ] После 5.0f расширять тот же tool-sensitive command surface без дублирования обработчиков: Eraser — `Size`; Selection — общий popover; пустой canvas — Paste/New layer.
- [ ] Не показывать больше 4-6 основных действий одновременно; расширенные параметры остаются в Settings/Layers/File/Navigation.
- [x] `F1` открывает вкладку Help общего окна `File/Help`.
- [ ] Help строить из тех же command/setting descriptions, чтобы горячие клавиши и диапазоны не расходились с UI.
- [ ] В Controls явно описать правило повторного нажатия shortcut и показать текущие циклы `S: Inside/Crossing`, `Q: Object/Area`, `X: Fill/Erase`.

## Ближайший checkpoint

**Palette — вручную подтверждено:** текущая RGB-палитра вынесена в отдельное перемещаемое и растягиваемое окно с компактным доступом из toolbar; picker внутри окна автоматически следует за шириной окна. Color persistence, документ и Brush/Fill/Recolor поведение не менялись.

**X cycle — вручную подтверждено:** первое `X` активирует Area Fill, повторное `X` переключает Area Fill/Area Erase, прежний прямой вход `L` в Fill остаётся alias. Реализация переиспользует существующие Fill и EraseArea capture/commit пути без изменения документа.

**S/Q cycles — вручную подтверждено:** первое `S` активирует Selection, повторное `S` в Object mode циклически меняет Selection `Inside/Crossing` без очистки текущего Object selection. Первое `Q` активирует Selection из других инструментов, повторное `Q` при активном Selection переключает `Object/Area` через существующую логику очистки несовместимого состояния.

**Undo history / compaction base — реализовано:** обычный Undo теперь ограничен последними 1000 активными history transaction groups, при этом активные векторные операции остаются авторитетным содержимым документа. Это база для стабильного `CompactBlock`: после выхода Freeze за лимит обычный Ctrl+Z уже не должен разъединять compact object.

**CompactBlock C1/C2 — реализовано:** `Freeze/Compact selected` в Selection menu создаёт один persistable `CompactBlock` из выбранного Object set active visible unlocked layer, включая разнесённые объекты, mixed-depth объекты и existing CompactBlocks. Блок хранит vector snapshot исходных операций на их исходных depth, а служебный bounds строится на anchor depth для selection/spatial index; fallback/tiles раскрывают snapshot при рендере, Undo/Redo/Reopen работают пока Freeze находится в undo history.

**CompactBlock C1/C2 — вручную подтверждено с follow-up:** Freeze/Compact работает, но получившийся block пока нельзя изменять/перемещать, а рамка выделения заметно больше видимого объекта из-за грубых tile bounds. Следующий corrective checkpoint: tight bounds по исходной geometry с учётом local coordinates/stroke width и whole-block Move/Scale/Rotate/Flip через трансформацию `compact_sources` + пересчёт block bounds одной replacement transaction.

**CompactBlock normal-object checkpoint — реализовано pending validation/manual confirmation:** bounds считаются по реальной source geometry с local coordinates и stroke width. Selected CompactBlock можно Move/Scale/Rotate/Flip как один объект; Copy/Cut/Paste и Move selection to layer работают с блоком целиком; повторный Freeze с другими объектами или другими blocks flatten-ит nested `compact_sources` в один snapshot без требования contiguous paint-order range.

После release рамка rectangle-selection исчезает; остаётся подсветка выбранных объектов, которая корректно следует за навигацией.

## Идеи на потом

- [ ] Возможно не делать: экспериментальный 3D-просмотр холста, где глубины/слои/тайлы можно осматривать в объёмной сцене отдельно от основного 2D-редактора; сначала оценить полезность, производительность и риск усложнения навигации.

## Текущий performance checkpoint

- [x] Saved fallback fast-path cache churn: `Auto` fast-path state no longer clears the whole saved fallback derived/clipped cache; stroke entries use an operation `render_variant` keyed by effective smoothing passes, while Fill/EraseArea entries can survive fast/quality toggles. Session logs now include `auto_fast_stroke_fallback` and the last canvas rect values/bits for future cache-miss diagnosis. No `.esketch`, raster tile, tile cache identity, selection, or compact-block data format changes.

- [x] Frame phase diagnostics checkpoint: fps_drop, marker, and normal session events now include the last frame phase timings (`input_ms`, tile total/subphases, fallback total/subphases, `overlay_ms`, `total_frame_ms`, measured `frame_ms`, and `last_fps_ema`). Rendering, input, settings, presets, saved operations, `.esketch`, tile cache identity, and raster output are unchanged.

- [x] ProjectedGeometryCache cross-depth reuse checkpoint: projected reference-point caches now survive representable zoom depth-boundary crossings by using a depth-aware screen scale. The cache still resets for unrepresentable centers/scales and when the current center moves more than 8 anchor tiles. Rendering math is covered against direct `canvas_to_screen`; `.esketch`, saved operations, tile cache identity, raster output, settings, and input are unchanged.

- [x] ProjectedGeometryCache anchor-radius checkpoint: projection anchor reuse now allows up to 64 anchor tiles instead of 8 before resetting. This preserves cached reference projections across ordinary zoom-out navigation that previously crossed the small radius and caused full fallback projection spikes, while unrepresentable centers/scales and larger moves still reset safely. `.esketch`, saved operations, tile cache identity, raster output, settings, and input are unchanged.

- [x] Selection top-layer cache-preservation checkpoint: Selection recolor, scale/rotate/flip, move, and reorder now use the top-layer document revision path instead of full tile-render invalidation when the active layer is already topmost. This keeps saved fallback derived/clipped caches warm for unchanged visible operations after selection edits, while non-top-layer edits, layer reorder, and layer merge still fall back to full invalidation. Visual output, `.esketch`, saved operations, raster tiles, tile cache identity, input, and settings are unchanged.

- [x] Selection replacement fallback warm-up checkpoint: after Selection recolor, move, scale, rotate, or flip creates replacement operations, the saved fallback derived-point cache is warmed from the previous operation keys to the new operation keys. Move/recolor reuse exact translated/unchanged screen points; scale/rotate/flip apply the same `ScreenAffine` used for persisted geometry. Clipped runs/polygons are intentionally not copied because clipping depends on the new viewport intersection. This targets the remaining 541 replacement-operation misses seen in `session-20260715-125125`; document format, raster tiles, tile cache identity, settings, presets, and input sampling are unchanged.

- [x] OperationIndex direct-removal checkpoint: the spatial operation index now records reverse tile membership for each indexed operation. Replacing selected operations can remove only the affected operation indices from their recorded buckets instead of scanning every indexed bucket in the document. This targets the remaining Selection-transform `input_ms` spikes seen after cache misses were eliminated; query behavior, document format, saved operations, raster tiles, tile cache identity, fallback rendering, settings, presets, and input sampling are unchanged.

- [x] Storage prepared-statement checkpoint: grouped operation commits now prepare the repeated operation `INSERT` and draft cleanup `DELETE` statements once per `insert_operation_rows` call instead of rebuilding SQL execution state for every operation. This targets dense Selection move/scale/rotate commits of hundreds of replacement operations; transaction boundaries, sequence assignment, JSON/zstd payload format, history, `.esketch`, raster tiles, tile cache identity, rendering, input, settings, and presets are unchanged.

- [x] Selection commit subphase diagnostics checkpoint: `selection_transform` session events now include `selection_commit` timings for document target collection, replacement construction, storage commit, storage encode/compress/insert/history subphases, in-memory replacement/index update, fallback-cache warm-up, byte counts, and affected operation counts. Rendering, input behavior, persistence format, settings, presets, tile cache identity, and raster output are unchanged.

- [x] Selection in-memory replacement lookup checkpoint: `replace_operations_in_place` now builds UUID-to-position maps for the authoritative operations list and render operations list once per replacement batch, instead of linearly searching both lists for every replaced object. This targets the `memory_replace_ms` portion measured at roughly 190-213 ms for dense Selection commits; operation order, replacement IDs, paint order, spatial indexes, render indexes, storage format, history, rendering, settings, presets, tile cache identity, and input behavior are unchanged.

- [x] Operation payload compression-level checkpoint: newly written operation and draft payloads now use zstd level 1 instead of level 3, while retaining the same zstd payload format and decoder compatibility for old and new operations. This targets `selection_commit.document.storage.compress_ms`, measured around 188-215 ms for dense Selection commits; SQLite schema, transaction boundaries, `synchronous=FULL`, layer-history compression, undo/redo, rendering, settings, presets, tile cache identity, and input behavior are unchanged. Session diagnostics now include the active operation `compression_level`.

- [x] OperationIndex metadata direct-removal checkpoint: `OperationIndex::remove_indices` now removes known operation indices directly from `operation_ids` and `operation_layers` instead of retain-scanning both full maps for every replacement batch. This targets the remaining `memory_replace_ms` portion of dense Selection commits after compression was reduced; broad-operation cleanup, tile-bucket removal/pruning, query ordering, document format, history, rendering, settings, presets, tile cache identity, and input behavior are unchanged.

- [x] OperationIndex precomputed-bounds checkpoint: replacement operations now compute spatial bounds once in `replace_operations_in_place` and reuse those bounds when inserting the same replacement into both the authoritative operation index and render operation index. This avoids scanning the same replacement geometry twice during dense Selection commits; broad/indexable behavior, tile-bucket insertion, query ordering, document format, history, rendering, settings, presets, tile cache identity, and input behavior are unchanged.

- [x] Selection memory-replace subphase diagnostics checkpoint: `selection_commit.document` now includes `memory_replace_fallback` plus nested `memory_replace` timings for replacement count, UUID position map construction, position lookup, replacement bounds, authoritative index removal, render index removal, assignment/index insertion, and total time. This targets the remaining broad `memory_replace_ms` cost in dense Selection commits; operation replacement behavior, ordering, document format, history, rendering, settings, presets, tile cache identity, and input behavior are unchanged.

- [x] Selection memory-replace fallback-reason diagnostics checkpoint: `selection_commit.document` now reports `memory_replace_fallback_reason`, and nested `memory_replace` includes counts for compact replacements, metadata replacements, missing authoritative-operation UUIDs, and missing render-operation UUIDs. This preserves the existing full-rebuild fallback behavior while making the next dense Selection log identify why the fast in-memory replacement path was skipped; document format, history, rendering, settings, presets, tile cache identity, and input behavior are unchanged.

- [x] Selection CompactBlock memory-replace fast path checkpoint: `replace_operations_in_place` now supports top-level `CompactBlock` replacements by removing the old block's expanded render-source indices, replacing the authoritative top-level block in place, and reusing/reindexing expanded render-source slots for the replacement block. This targets dense Selection commits where one compact replacement forced a full in-memory rebuild of the whole document; storage, history, `.esketch`, operation ordering, raster tiles, tile cache identity, rendering output, settings, presets, and input behavior are unchanged.

- [x] Dense Selection overlay single-pass bounds checkpoint: selected-object overlay painting now accumulates the same screen-space bounds while it paints highlights and reuses those bounds for the Selection transform box, instead of projecting/smoothing the selected operations a second time in the same frame. This targets the 25-29 ms `overlay_ms` frames seen with roughly 923 selected objects; selection hit testing, transform math, storage, history, `.esketch`, settings, presets, tile cache identity, and rendered selection semantics are unchanged.

- [x] Storage commit durability setting checkpoint: settings v16 adds `Storage commit` with `Full` as the default/current safety behavior and `Fast` mapping to SQLite `synchronous=NORMAL` for lower dense-edit commit latency. The setting is applied on document open and Settings save, is included in performance profiles (`Performance=Fast`, `Balanced/Quality=Full`), and is written to session log context. `.esketch` format, WAL mode, transactions, undo/redo, tile cache identity, rendering, and SQLite schema are unchanged.

- [x] Segmented fallback frame-budget checkpoint: saved no-tile stroke fallback now has a per-frame segmented-shape budget. `Quality` and `Auto` still use segmented capsule joins while the scene is moderate, but overloaded dense scenes downgrade over-budget runs to the existing fast polyline path and report `fallback_segmented_budget_fallbacks` in session logs. This targets the Performance-to-Quality crash/shape spike where Quality generated roughly 828k-868k stroke shapes per fallback frame; saved vectors, PNG tiles, `.esketch`, selection, presets, SQLite, and tile cache identity are unchanged.

- [x] Large Selection overlay fast-interaction checkpoint: when 512+ objects are selected and an interaction is active, the overlay temporarily skips per-object highlight contours and keeps the common transform bounds/handles. Full object highlights return while idle. Session logs include `large_selection_fast_overlay` so the next dense-scene check can separate overlay savings from fallback and commit costs. `.esketch`, selection membership, hit testing, transforms, storage, raster tiles, settings, presets, and tile cache identity are unchanged.

- [x] Large Selection overlay cached-bounds checkpoint: the active large-selection fast overlay now reuses aggregate canvas-space bounds keyed by document revision and selected UUID signature, then projects only cached per-depth rectangles each frame. This removes the remaining per-frame `selection_screen_bounds` geometry walk from the fast overlay path while preserving exact hit testing, transform start bounds, small selections, idle full highlights, `.esketch`, storage, raster tiles, settings, presets, and tile cache identity.

- [x] Large Selection transform-outline correction: fast overlay is now disabled while a Selection move, scale, or rotate gesture is active, so the exact per-object transform outline remains visible during direct object transforms. Cached-bounds fast overlay remains available for large selected sets during navigation/tile-deferred interaction. Document format, storage, raster tiles, settings, presets, and tile cache identity are unchanged.

- [x] Saved fallback screen-cull checkpoint manually confirmed: ordinary saved no-tile fallback operations are now rejected by rough screen bounds before expensive point projection when they are definitely outside the current viewport. CompactBlock source culling remains unchanged, and early rejections are included in `fallback_skipped_operations` diagnostics. This targets paused/manual tile-generation scenes where `fallback_project_ms` was dominated by thousands of projected operations after navigation; `.esketch`, saved operations, raster tiles, tile cache identity, settings, presets, selection, and input behavior are unchanged.
