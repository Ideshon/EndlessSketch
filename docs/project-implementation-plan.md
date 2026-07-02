# План EndlessSketch

Обновлено: 2026-07-02

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
- [x] `Alt+клик` и `Alt+колесо` циклически переключают перекрывающиеся объекты под курсором в прямом/обратном направлении; каждое сырое физическое деление колеса даёт ровно один шаг даже при быстрой прокрутке.
- [x] `Shift+клик` добавляет объект к выделению, `Ctrl+клик` переключает его состояние.
- [x] Rectangle по умолчанию работает в `Inside`; toolbar позволяет явно включить `Crossing`.
- [ ] Первое `S` активирует Selection, каждое повторное `S` при уже активном Selection переключает `Inside`/`Crossing`.
- [ ] Toolbar, status и курсор сразу показывают текущий Selection mode; переключение не очищает уже выбранные объекты.
- [ ] Сохранить текущий `Object selection` для выбора операций целиком и добавить независимый `Area selection`, который задаёт постоянную clipping-область Rectangle/Lasso.
- [ ] Object selection и Area selection не активны одновременно; явное переключение режима очищает несовместимое временное состояние после подтверждения, если это необходимо.
- [ ] Учитывать Brush width, Fill polygon, Eraser и текущую видимость.
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
- [ ] Для Fill выполнять polygon clipping и сохранять все валидные внутренние/внешние контуры без разрывов и самопересечений.
- [ ] Записывать split/replacement как одну grouped transaction, чтобы undo полностью восстанавливал исходную операцию.
- [x] Drag-перемещение Object selection с live preview и сохранением как одной tombstone/replacement transaction; replacement наследует исходный `paint_order`, поэтому объект не поднимается над соседями после Move, reopen, undo или redo.
- [x] `Ctrl+колесо` при непустом Object selection меняет порядок относительно соседних объектов active layer: вверх — на один шаг вперёд, вниз — на один шаг назад.
- [x] Каждое физическое деление колеса даёт ровно один reorder step; обычное колесо продолжает zoom, `Alt+колесо` продолжает cycle объектов.
- [ ] Добавить stylus-доступные drag-эквиваленты wheel-команд: default `Alt+вертикальный drag` циклически перебирает overlap candidates, `Ctrl+Alt+вертикальный drag` меняет paint order.
- [ ] Дискретный drag накапливает signed distance и даёт ровно один command step на настраиваемый порог; отпускание сбрасывает остаток и завершает gesture.
- [ ] Более специфичный modifier chord имеет приоритет: `Ctrl+Alt+drag` не запускает обычный `Alt` cycle, Selection/Brush и navigation не получают случайный stroke.
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

- [ ] Rectangle и свободное Lasso после release преобразуются из screen-space в точную canvas/depth geometry и остаются на месте при pan/zoom/depth navigation.
- [ ] Показать постоянный пунктирный контур и bounds; `Esc`/Deselect снимает область, не изменяя документ.
- [ ] Первая версия использует один Replace-контур; затем добавить `Shift Add`, `Alt Subtract` и `Intersect` с валидной multi-contour geometry.
- [ ] Area selection является временной clipping mask, а не layer mask и не отдельным объектом документа.
- [ ] Brush, Eraser, Fill и Gradient не создают содержимое за пределами активной области.
- [ ] Clipping выполнять при commit: stroke может стать несколькими векторными fragments одной transaction, Fill/Erase/Gradient получают валидный polygon clip.
- [ ] Изменение одной только Area selection не меняет revision и не запускает tile rebuild; commit инвалидирует только затронутые tiles.
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
- [ ] Первое `X` активирует Area tool, повторное `X` циклически переключает `Fill`/`Erase`; прежний `L` можно временно оставить alias для прямого входа в Fill.
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
  - [ ] M5.1b: rotate handle, Flip Horizontal/Vertical и общие transform-команды.
  - [ ] M5.1c: `Recolor selected` через текущую палитру для Paint/Fill как одна grouped replacement transaction с сохранением geometry/layer/paint order и поддержкой Undo/Redo/Reopen.
- [ ] Checkpoint 5.2: постоянная Rectangle/Lasso Area selection и clipping Brush/Eraser/Fill.
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

## Стабилизация ввода и FPS под нагрузкой

**Сложность: высокая. Выполнить после ближайшего блока Layers и до экспорта.**

На насыщенном холсте с высоким качеством подтверждены подвисания UI. Во время подвисания Z + ЛКМ может кратко изменить направление, не сработать или начать Brush; колесо иногда не даёт видимого шага; pan иногда пропускает движение. Несколько быстрых отдельных штрихов могут объединиться в «галочку», потому что текущая обработка одного кадра сохраняет движение мыши, но сводит несколько `press/release`-циклов к одному draft.

### S1. Измеряемое воспроизведение

- [ ] Добавить раздельные p50/p95/p99/max для frame, input, fallback paint, tile submit/result upload и UI tessellation.
- [ ] Сделать debug stress mode с искусственной задержкой UI-потока `50/100/200/300 ms`, не меняя release defaults.
- [ ] Считать входные `press/move/release`, wheel events, созданные strokes и случаи смены gesture mode.
- [ ] Сохранить контрольный насыщенный документ и одинаковые High Quality настройки для повторных замеров.

### S2. Последовательная очередь событий

- [ ] Заменить один агрегированный `primary_pressed/primary_released` на обработку `PointerButton` и `PointerMoved` строго в порядке `egui::Event`.
- [ ] Поддержать несколько полных `press -> move -> release` циклов за один UI-кадр; каждый цикл создаёт отдельный stroke.
- [ ] Mouse history использовать только для восстановления промежуточных координат внутри текущего нажатия, не для границ нажатия.
- [ ] Не соединять release одного stroke с press следующего линейной интерполяцией.

### S3. Зафиксированный режим жеста

- [ ] На каждом primary press выбрать один режим: Draw, Z Zoom, Space Pan, brush resize или Selection, и удерживать его до соответствующего release.
- [ ] Учитывать Key Z/Space/Ctrl и PointerButton в порядке событий одного кадра, а не только итоговое `key_down`.
- [ ] Z Zoom и Space Pan всегда потребляют свой жест и не могут оставить случайный Brush draft.
- [ ] Zoom/pan считать из последовательных event positions; большая задержка кадра не должна терять delta или менять его знак.

### S4. Колесо без пропусков

- [ ] Обрабатывать сырые wheel events по порядку вместо одного `smooth_scroll_delta` кадра.
- [ ] Накапливать дробные trackpad delta отдельно; каждое физическое деление обычного колеса применять ровно один раз.
- [ ] Не терять wheel input во время отмены устаревшей tile generation и zoom settle.

### S5. Снижение нагрузки взаимодействия

- [ ] Профилировать High Quality отдельно для активных tile workers, vector fallback, texture upload и egui tessellation.
- [ ] Во время Draw/Zoom/Pan приостанавливать новые тяжёлые jobs и texture upload по frame budget; готовое качество восстанавливать после idle.
- [ ] Добавить cooperative cancellation длинной растеризации устаревшего поколения, если profiling подтвердит конкуренцию CPU.
- [ ] Отдельно профилировать Selection на слоях с большим числом объектов: hover hit test, projection, полную подсветку выбранной геометрии и egui tessellation.
- [ ] Пересчитывать hover-кандидатов только при движении курсора/камеры/revision, кешировать projected selection overlay и отсекать невидимые операции spatial index.
- [ ] Для очень больших selection рассмотреть упрощённый bounding-box preview вместо полной подсветки каждой линии во время взаимодействия.
- [ ] Не снижать итоговое качество тайлов: допустимо только временное упрощение интерактивного preview.
- [ ] Нативную timestamped очередь `WM_INPUT`/`WM_POINTER` добавлять только если упорядоченные egui events всё ещё теряются.

### S6. Критерии готовности

- [ ] Серия из 100 быстрых отдельных линий даёт 100 strokes без «галочек» при искусственных задержках до 300 ms.
- [ ] 100 жестов Z + ЛКМ не создают Brush operations, не меняют направление и не теряют release.
- [ ] Wheel и pan дают воспроизводимый суммарный сдвиг при той же задержке.
- [ ] Deferred drawing остаётся дополнительным режимом снижения нагрузки, а не условием корректного press/release.
- [ ] High Quality после завершения взаимодействия даёт тот же результат, что и до оптимизации.

### S7. Уплотнение старых объектов

**Сложность: очень высокая; выполнять только после profiling S1-S5.**

- [ ] Добавить ручную команду `Freeze/Compact selected` и `Freeze/Compact layer` для старого завершённого содержимого.
- [ ] Не заменять исходные векторы одним bitmap: хранить проверенный compressed source snapshot и rebuildable multi-depth LOD cache.
- [ ] Представлять frozen block как один spatial/index object; раскрывать внутренние операции только при редактировании или cache miss.
- [ ] Undo размораживает исходные операции без потери UUID/геометрии; reopen и удаление cache дают идентичный рисунок.
- [ ] После ручного режима добавить опциональный auto-compact в idle по порогу числа операций, например `Off/500/1000/5000`.
- [ ] Автоматически объединять только стабильные операции вне активной истории/выделения/draft и только после проверенного checkpoint.
- [ ] Сравнить размер SQLite, время открытия, spatial query, fallback FPS и tile rebuild до включения auto-compact.

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
- [ ] `S` циклически меняет Selection `Inside/Crossing`; `X` циклически меняет Area `Fill/Erase`.
- [ ] Тот же механизм использовать для будущих инструментов с конечным набором режимов, без отдельных hardcoded обработчиков клавиш.
- [ ] Не обрабатывать tool shortcuts и cycle mode, когда фокус находится в text/numeric input.
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

- [ ] Вынести палитру цвета в отдельное перемещаемое окно с тем же поведением позиционирования, что у `Settings` и `Layers`.
- [ ] Сохранить текущий быстрый доступ к активному цвету и не менять color/persistence модель в рамках этой UI-задачи.
- [x] Selection-версия контекстного окна по ПКМ выполнена в checkpoint 5.0e и переиспользует команды общего popover.
- [x] В checkpoint 5.0f добавлены для Brush и Fill по ПКМ inline RGB-палитра и общий `Size`, связанные с теми же значениями toolbar; открытие/редактирование menu не создаёт operation или history step.
- [ ] После 5.0f расширять тот же tool-sensitive command surface без дублирования обработчиков: Eraser — `Size`; Selection — общий popover; пустой canvas — Paste/New layer.
- [ ] Не показывать больше 4-6 основных действий одновременно; расширенные параметры остаются в Settings/Layers/File/Navigation.
- [x] `F1` открывает вкладку Help общего окна `File/Help`.
- [ ] Help строить из тех же command/setting descriptions, чтобы горячие клавиши и диапазоны не расходились с UI.
- [ ] В Controls явно описать правило повторного нажатия shortcut и показать текущие циклы `S: Inside/Crossing`, `X: Fill/Erase`.

## Ближайший checkpoint

**Checkpoint 5.0f — проверен вручную:** Brush и Fill открывают по ПКМ inline RGB picker и `Size`; палитра остаётся открытой при взаимодействии, значения связаны с toolbar, а вторичная кнопка не создаёт operation/history step. Следующий transform-checkpoint — M5.1b rotate/Flip, затем M5.1c Recolor selected.

После release рамка rectangle-selection исчезает; остаётся подсветка выбранных объектов, которая корректно следует за навигацией.
