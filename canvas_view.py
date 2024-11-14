# canvas_view.py

from PyQt5.QtWidgets import QMainWindow, QGraphicsView, QGraphicsScene, QToolBar, QAction, QColorDialog, QMenu, QInputDialog
from PyQt5.QtGui import QPainter, QPen, QColor, QMouseEvent
from PyQt5.QtCore import Qt, QEvent
from tools import BrushTool, EraserTool, LassoFillTool, LassoEraseTool
from settings import Settings

class CanvasWindow(QMainWindow):
    def __init__(self):
        super().__init__()
        self.setWindowTitle("EndlessSketch")
        self.setGeometry(100, 100, 800, 600)
        self.settings = Settings()
        self.initUI()

    def initUI(self):
        self.scene = QGraphicsScene()
        self.view = CanvasView(self.scene, self.settings)
        self.setCentralWidget(self.view)
        self.createToolBar()

    def createToolBar(self):
        toolbar = QToolBar("Инструменты")
        self.addToolBar(toolbar)

        # Инструмент кисти
        brush_action = QAction("Кисть", self)
        brush_action.triggered.connect(self.selectBrushTool)
        toolbar.addAction(brush_action)

        # Ластик
        eraser_action = QAction("Ластик", self)
        eraser_action.triggered.connect(self.selectEraserTool)
        toolbar.addAction(eraser_action)

        # Лассо Заливка
        lasso_fill_action = QAction("Лассо Заливка", self)
        lasso_fill_action.triggered.connect(self.selectLassoFillTool)
        toolbar.addAction(lasso_fill_action)

        # Лассо Стирание
        lasso_erase_action = QAction("Лассо Стирание", self)
        lasso_erase_action.triggered.connect(self.selectLassoEraseTool)
        toolbar.addAction(lasso_erase_action)

        # Выбор цвета
        color_action = QAction("Цвет", self)
        color_action.triggered.connect(self.chooseColor)
        toolbar.addAction(color_action)

    def selectBrushTool(self):
        self.view.current_tool = BrushTool(self.settings)

    def selectEraserTool(self):
        self.view.current_tool = EraserTool(self.settings)

    def selectLassoFillTool(self):
        self.view.current_tool = LassoFillTool(self.settings)

    def selectLassoEraseTool(self):
        self.view.current_tool = LassoEraseTool(self.settings)

    def chooseColor(self):
        color = QColorDialog.getColor()
        if color.isValid():
            self.settings.current_color = color

class CanvasView(QGraphicsView):
    def __init__(self, scene, settings):
        super().__init__(scene)
        self.settings = settings
        self.current_tool = BrushTool(self.settings)
        self.setRenderHint(QPainter.Antialiasing)
        self.last_point = None

    def wheelEvent(self, event):
        zoom_in_factor = 1.25
        zoom_out_factor = 0.8
        if event.angleDelta().y() > 0:
            scale_factor = zoom_in_factor
        else:
            scale_factor = zoom_out_factor
        self.scale(scale_factor, scale_factor)

    def mousePressEvent(self, event):
        if event.button() == Qt.MiddleButton:
            self.setDragMode(QGraphicsView.ScrollHandDrag)
            fake_event = QMouseEvent(QEvent.MouseButtonPress, event.localPos(), Qt.LeftButton,
                                     event.buttons() | Qt.LeftButton, event.modifiers())
            super(CanvasView, self).mousePressEvent(fake_event)
        elif event.button() == Qt.RightButton:
            self.contextMenuEvent(event)
        else:
            self.current_tool.on_press(event, self)
            super(CanvasView, self).mousePressEvent(event)

    def mouseMoveEvent(self, event):
        if event.buttons() & Qt.MiddleButton:
            fake_event = QMouseEvent(QEvent.MouseMove, event.localPos(), Qt.LeftButton,
                                     event.buttons() | Qt.LeftButton, event.modifiers())
            super(CanvasView, self).mouseMoveEvent(fake_event)
        else:
            self.current_tool.on_move(event, self)
            super(CanvasView, self).mouseMoveEvent(event)

    def mouseReleaseEvent(self, event):
        if event.button() == Qt.MiddleButton:
            self.setDragMode(QGraphicsView.NoDrag)
            fake_event = QMouseEvent(QEvent.MouseButtonRelease, event.localPos(), Qt.LeftButton,
                                     event.buttons() & ~Qt.LeftButton, event.modifiers())
            super(CanvasView, self).mouseReleaseEvent(fake_event)
        else:
            self.current_tool.on_release(event, self)
            super(CanvasView, self).mouseReleaseEvent(event)

    def contextMenuEvent(self, event):
        context_menu = QMenu(self)
        change_brush_size_action = QAction("Изменить размер кисти", self)
        context_menu.addAction(change_brush_size_action)
        change_brush_size_action.triggered.connect(self.change_brush_size)
        context_menu.exec_(event.globalPos())

    def change_brush_size(self):
        size, ok = QInputDialog.getInt(self, "Размер кисти", "Введите размер кисти:", min=1, max=100,
                                       value=self.settings.brush_size)
        if ok:
            self.settings.brush_size = size
