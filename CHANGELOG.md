# Журнал изменений / Changelog

## Unreleased — 2026-08-19

### Русский

- Векторные Eraser и Eraser Lasso атомарно стирают Paint, Fill-группы и CompactBlock с Undo/Redo/Reopen.
- Area Selection сохраняет rectangle/lasso-маску при навигации и ограничивает новые Brush, Eraser и Fill.
- Улучшены Object Selection, трансформации, слои, Merge Down и упаковка старых объектов в CompactBlock.
- Исправлен ввод Windows-стилуса: независимая выборка курсора устраняет ложные боковые скачки и снижает зависимость от FPS.
- Ускорены spatial/fallback/raster пути; добавлены профили качества, диагностика фаз кадра и подробная внешняя справка.

### English

- Vector Eraser and Eraser Lasso atomically edit Paint, Fill groups, and CompactBlock with Undo/Redo/Reopen support.
- Area Selection keeps a rectangle/lasso mask across navigation and clips new Brush, Eraser, and Fill operations.
- Expanded Object Selection, transforms, layers, Merge Down, and CompactBlock packing for older objects.
- Fixed Windows stylus input with independent cursor sampling that removes false lateral jumps and reduces FPS dependence.
- Improved spatial/fallback/raster performance, quality presets, frame-phase diagnostics, and external help.
