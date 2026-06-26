<img width="818" height="440" alt="EndlessSketch_101902_1342" src="https://github.com/user-attachments/assets/a060edf8-ba10-4036-8d44-b97f9bc50a44" />

# EndlessSketch

EndlessSketch is a Windows application written in Rust for drawing on a canvas that is virtually unlimited in distance and depth.

## How the canvas works

The camera uses hierarchical depth levels, BigInt tile addresses, and local normalized coordinates. There is no global rectangular scene or coordinate limit.

The source data is a compressed vector operation in SQLite/WAL. PNG tiles use 1×/2×/4× resolutions without stretching; above 4×, the app draws the original vectors. Tiles are only a delete-and-auto-restore LOD cache.

When changing the depth, the app continues to show vectors until the entire visible area of a single revision is ready. An incomplete cache cannot replace a frame.

An opaque brush and eraser cover all older levels. Newer strokes remain visible; undo/redo is saved between launches.

The stroke draft is saved every 100 ms. SQLite is checked at checkpoint; two checked backups are stored.

The new document is a name.esketch directory with manifest.json, canvas.sqlite3, backups/, assets/, and a recoverable cache/. Old .ess/.esp files are not imported or modified.

## Management

- B — brush, E — eraser, L — lasso fill, I — eyedropper.

- Mouse wheel / Z+LMB — zoom relative to the cursor.

- Middle mouse button / spacebar + LMB — move.

- Ctrl+Z / Ctrl+Y — undo/redo.

- The top panel contains the color, brush size, opening/creating documents, and bookmarking the current position.

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
Камера использует иерархические уровни глубины, BigInt-адреса тайлов и локальные нормализованные координаты. Глобальной прямоугольной сцены и предела координат нет.

Исходные данные — сжатые векторные операции в SQLite/WAL. PNG-тайлы используют разрешения 1×/2×/4× без растягивания; выше 4× приложение рисует исходные векторы. Тайлы являются только удаляемым и автоматически восстанавливаемым LOD-кэшем.

При смене глубины приложение продолжает показывать векторы, пока не готова вся видимая область одной ревизии. Неполный кэш не может заменить кадр.

Непрозрачная кисть и ластик перекрывают все более старые уровни. Более новые штрихи остаются видимыми; undo/redo сохраняется между запусками.

Черновик штриха сохраняется каждые 100 мс. SQLite проверяется при checkpoint; хранятся две проверенные резервные копии.

Новый документ — каталог name.esketch с manifest.json, canvas.sqlite3, backups/, assets/ и восстанавливаемым cache/. Старые .ess/.esp не импортируются и не изменяются.

## Управление

- B — кисть, E — ластик, L — заливка лассо, I — пипетка.
- Колесо мыши / Z+ЛКМ — масштабирование относительно курсора.
- Средняя кнопка мыши / пробел+ЛКМ — перемещение.
- Ctrl+Z / Ctrl+Y — undo/redo.
- Панель сверху содержит цвет, размер кисти, открытие/создание документов и закладки текущей позиции.
## Сборка и запуск

Требуются Rust stable MSVC и Visual Studio Build Tools с C++ workload.

```powershell
cargo run --release
cargo run --release -- "D:\Drawings\canvas.esketch"
```

Проверки:

```powershell
cargo fmt -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all-targets
cargo run --release --bin stress -- 1000000
```

Стресс-профиль строит и запрашивает иерархический индекс миллиона синтетических штрихов, распределённых по 12 уровням глубины.
