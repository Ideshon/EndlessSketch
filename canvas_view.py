# canvas_view.py

from PyQt5.QtWidgets import (
    QMainWindow, QGraphicsView, QGraphicsScene, QToolBar, QAction,
    QColorDialog, QSlider, QLabel, QFileDialog, QGraphicsPathItem,
    QMenu, QWidgetAction, QWidget
)
from PyQt5.QtGui import QPainter, QMouseEvent, QWheelEvent, QPainterPath, QPen, QColor
from PyQt5.QtCore import Qt, QEvent, QPoint
from tools import BrushTool, LassoFillTool, LassoEraseTool, EyedropperTool
from settings import Settings
import json

class CanvasWindow(QMainWindow):
    def __init__(self):
        super().__init__()
        self.setWindowTitle("EndlessSketch")
        self.setGeometry(100, 100, 800, 600)
        self.settings = Settings()
        self.initUI()

    def initUI(self):
        # Создаем сцену и представление
        self.scene = QGraphicsScene()
        self.view = CanvasView(self.scene, self.settings)
        self.setCentralWidget(self.view)

        # Создаем панель инструментов
        self.createToolBar()

        # Создаем меню для сохранения и загрузки
        self.createMenuBar()

    def createToolBar(self):
        toolbar = QToolBar("Инструменты")
        self.addToolBar(toolbar)

        # Инструмент кисти
        brush_action = QAction("Кисть", self)
        brush_action.triggered.connect(self.selectBrushTool)
        brush_action.setShortcut("B")  # Горячая клавиша B
        toolbar.addAction(brush_action)

        # Лассо Заливка
        lasso_fill_action = QAction("Лассо Заливка", self)
        lasso_fill_action.triggered.connect(self.selectLassoFillTool)
        lasso_fill_action.setShortcut("L")  # Горячая клавиша L
        toolbar.addAction(lasso_fill_action)

        # Лассо Стирание
        lasso_erase_action = QAction("Лассо Стирание", self)
        lasso_erase_action.triggered.connect(self.selectLassoEraseTool)
        lasso_erase_action.setShortcut("E")  # Горячая клавиша E
        toolbar.addAction(lasso_erase_action)

        # Пипетка
        eyedropper_action = QAction("Пипетка", self)
        eyedropper_action.triggered.connect(self.selectEyedropperTool)
        eyedropper_action.setShortcut("I")  # Горячая клавиша I
        toolbar.addAction(eyedropper_action)

        # Выбор цвета
        color_action = QAction("Цвет", self)
        color_action.triggered.connect(self.chooseColor)
        color_action.setShortcut("C")  # Горячая клавиша C
        toolbar.addAction(color_action)

        # Ползунок размера кисти
        brush_size_label = QLabel("Размер кисти:")
        toolbar.addWidget(brush_size_label)

        self.brush_slider = QSlider(Qt.Horizontal)
        self.brush_slider.setMinimum(1)
        self.brush_slider.setMaximum(100)
        self.brush_slider.setValue(self.settings.brush_size_percentage)
        self.brush_slider.setTickPosition(QSlider.TicksBelow)
        self.brush_slider.setTickInterval(10)
        self.brush_slider.valueChanged.connect(self.changeBrushSize)
        toolbar.addWidget(self.brush_slider)

    def createMenuBar(self):
        menubar = self.menuBar()
        file_menu = menubar.addMenu('Файл')

        # Сохранить холст
        save_canvas_action = QAction('Сохранить холст', self)
        save_canvas_action.triggered.connect(self.saveCanvas)
        save_canvas_action.setShortcut("Ctrl+S")  # Горячая клавиша Ctrl+S
        file_menu.addAction(save_canvas_action)

        # Загрузить холст
        load_canvas_action = QAction('Загрузить холст', self)
        load_canvas_action.triggered.connect(self.loadCanvas)
        load_canvas_action.setShortcut("Ctrl+O")  # Горячая клавиша Ctrl+O
        file_menu.addAction(load_canvas_action)

        # Сохранить место
        save_place_action = QAction('Сохранить место', self)
        save_place_action.triggered.connect(self.savePlace)
        file_menu.addAction(save_place_action)

        # Загрузить место
        load_place_action = QAction('Загрузить место', self)
        load_place_action.triggered.connect(self.loadPlace)
        file_menu.addAction(load_place_action)

    def selectBrushTool(self):
        print("CanvasWindow: Selected BrushTool")
        self.view.current_tool = BrushTool(self.settings)

    def selectLassoFillTool(self):
        print("CanvasWindow: Selected LassoFillTool")
        self.view.current_tool = LassoFillTool(self.settings)

    def selectLassoEraseTool(self):
        print("CanvasWindow: Selected LassoEraseTool")
        self.view.current_tool = LassoEraseTool(self.settings)

    def selectEyedropperTool(self):
        print("CanvasWindow: Selected EyedropperTool")
        self.view.current_tool = EyedropperTool(self.settings)

    def chooseColor(self):
        color = QColorDialog.getColor()
        if color.isValid():
            self.settings.current_color = color
            print(f"CanvasWindow: Color changed to {color.name()}")

    def changeBrushSize(self, value):
        print(f"CanvasWindow: Brush size percentage changed to {value}%")
        self.settings.brush_size_percentage = value
        self.view.updateBrushSize()

    def saveCanvas(self):
        print("CanvasWindow: Saving canvas")
        try:
            options = QFileDialog.Options()
            filename, _ = QFileDialog.getSaveFileName(self, "Сохранить холст", "",
                                                      "EndlessSketch Files (*.ess)", options=options)
            if filename:
                data = []
                for item in self.scene.items():
                    if isinstance(item, QGraphicsPathItem):
                        path = item.path()
                        # Extract path as list of points
                        points = []
                        for i in range(path.elementCount()):
                            element = path.elementAt(i)
                            points.append((element.x, element.y))
                        # Get pen properties
                        pen = item.pen()
                        item_data = {
                            'type': 'path',
                            'color': pen.color().name(),
                            'width': pen.widthF(),
                            'path': points
                        }
                        data.append(item_data)
                with open(filename, 'w') as f:
                    json.dump(data, f, indent=4)
                print(f"CanvasWindow: Canvas saved to {filename}")
        except Exception as e:
            print(f"CanvasWindow: Exception in saveCanvas: {e}")

    def loadCanvas(self):
        print("CanvasWindow: Loading canvas")
        try:
            options = QFileDialog.Options()
            filename, _ = QFileDialog.getOpenFileName(self, "Загрузить холст", "",
                                                      "EndlessSketch Files (*.ess)", options=options)
            if filename:
                with open(filename, 'r') as f:
                    data = json.load(f)
                self.scene.clear()
                for item_data in data:
                    if item_data['type'] == 'path':
                        path = QPainterPath()
                        points = item_data['path']
                        if points:
                            path.moveTo(*points[0])
                            for point in points[1:]:
                                path.lineTo(*point)
                        path_item = QGraphicsPathItem(path)
                        pen = QPen(QColor(item_data['color']), item_data['width'])
                        pen.setCapStyle(Qt.RoundCap)
                        pen.setJoinStyle(Qt.RoundJoin)
                        path_item.setPen(pen)
                        self.scene.addItem(path_item)
                print(f"CanvasWindow: Canvas loaded from {filename}")
        except Exception as e:
            print(f"CanvasWindow: Exception in loadCanvas: {e}")

    def savePlace(self):
        print("CanvasWindow: Saving place")
        try:
            options = QFileDialog.Options()
            filename, _ = QFileDialog.getSaveFileName(self, "Сохранить место", "",
                                                      "EndlessSketch Place Files (*.esp)", options=options)
            if filename:
                center = self.view.mapToScene(self.view.viewport().rect().center())
                place = {
                    'x': center.x(),
                    'y': center.y(),
                    'zoom_factor': self.view.zoom_factor  # Используем zoom_factor из view
                }
                with open(filename, 'w') as f:
                    json.dump(place, f, indent=4)
                print(f"CanvasWindow: Place saved to {filename}")
        except Exception as e:
            print(f"CanvasWindow: Exception in savePlace: {e}")

    def loadPlace(self):
        print("CanvasWindow: Loading place")
        try:
            options = QFileDialog.Options()
            filename, _ = QFileDialog.getOpenFileName(self, "Загрузить место", "",
                                                      "EndlessSketch Place Files (*.esp)", options=options)
            if filename:
                with open(filename, 'r') as f:
                    place = json.load(f)
                # Reset zoom to 1.0 first
                self.resetZoom()

                # Apply saved zoom factor
                target_zoom = place.get('zoom_factor', 1.0)
                if target_zoom <= 0:
                    print("CanvasWindow: Invalid zoom_factor in place file")
                    target_zoom = 1.0
                while abs(self.view.zoom_factor - target_zoom) > 0.01:
                    if self.view.zoom_factor < target_zoom:
                        self.view.scale(1.25, 1.25)
                        self.view.zoom_factor *= 1.25
                    else:
                        self.view.scale(0.8, 0.8)
                        self.view.zoom_factor *= 0.8
                print(f"CanvasWindow: Zoom factor set to {self.view.zoom_factor}")

                # Update settings zoom_factor
                self.settings.zoom_factor = self.view.zoom_factor

                # Center view on saved coordinates
                self.view.centerOn(place['x'], place['y'])
                print(f"CanvasWindow: Place loaded from {filename}")

                # Обновляем размер кисти после изменения масштаба
                self.view.updateBrushSize()
        except Exception as e:
            print(f"CanvasWindow: Exception in loadPlace: {e}")

    def resetZoom(self):
        print("CanvasWindow: Resetting zoom to 1.0")
        # Reset the view's scale to original
        while self.view.zoom_factor > 1.01:
            self.view.scale(0.8, 0.8)
            self.view.zoom_factor *= 0.8
        while self.view.zoom_factor < 0.99:
            self.view.scale(1.25, 1.25)
            self.view.zoom_factor *= 1.25
        self.view.zoom_factor = 1.0
        self.settings.zoom_factor = 1.0
        print("CanvasWindow: Zoom reset to 1.0")

class CanvasView(QGraphicsView):
    def __init__(self, scene, settings):
        super().__init__(scene)
        self.settings = settings
        self.current_tool = BrushTool(self.settings)
        self.setRenderHint(QPainter.Antialiasing)
        self.last_point = None
        self.setDragMode(QGraphicsView.NoDrag)
        self.zoom_factor = 1.0  # Изначальный масштаб
        self.settings.zoom_factor = self.zoom_factor
        print(f"CanvasView: Initialized with zoom_factor = {self.zoom_factor}")

        # Отключаем прокрутку при приближении к краям
        self.setHorizontalScrollBarPolicy(Qt.ScrollBarAlwaysOff)
        self.setVerticalScrollBarPolicy(Qt.ScrollBarAlwaysOff)
        self.setTransformationAnchor(QGraphicsView.NoAnchor)
        self.setResizeAnchor(QGraphicsView.NoAnchor)

    def wheelEvent(self, event: QWheelEvent):
        zoom_in_factor = 1.25
        zoom_out_factor = 0.8

        if event.angleDelta().y() > 0:
            scale_factor = zoom_in_factor
            self.zoom_factor *= zoom_in_factor
            print(f"CanvasView: Zooming in. New zoom factor: {self.zoom_factor}")
        else:
            scale_factor = zoom_out_factor
            self.zoom_factor *= zoom_out_factor
            print(f"CanvasView: Zooming out. New zoom factor: {self.zoom_factor}")

        self.scale(scale_factor, scale_factor)
        self.settings.zoom_factor = self.zoom_factor
        self.updateBrushSize()

    def mousePressEvent(self, event: QMouseEvent):
        if event.button() == Qt.MiddleButton:
            print("CanvasView: Middle mouse button pressed - activating drag mode")
            self.setDragMode(QGraphicsView.ScrollHandDrag)
            fake_event = QMouseEvent(
                QEvent.MouseButtonPress,
                event.localPos(),
                Qt.LeftButton,
                event.buttons() | Qt.LeftButton,
                event.modifiers()
            )
            super(CanvasView, self).mousePressEvent(fake_event)
        elif event.button() == Qt.RightButton:
            self.showContextMenu(event)
        else:
            self.current_tool.on_press(event, self)
            super(CanvasView, self).mousePressEvent(event)

    def mouseMoveEvent(self, event: QMouseEvent):
        if event.buttons() & Qt.MiddleButton:
            fake_event = QMouseEvent(
                QEvent.MouseMove,
                event.localPos(),
                Qt.LeftButton,
                event.buttons() | Qt.LeftButton,
                event.modifiers()
            )
            super(CanvasView, self).mouseMoveEvent(fake_event)
        else:
            self.current_tool.on_move(event, self)
            super(CanvasView, self).mouseMoveEvent(event)

    def mouseReleaseEvent(self, event: QMouseEvent):
        if event.button() == Qt.MiddleButton:
            print("CanvasView: Middle mouse button released - deactivating drag mode")
            self.setDragMode(QGraphicsView.NoDrag)
            fake_event = QMouseEvent(
                QEvent.MouseButtonRelease,
                event.localPos(),
                Qt.LeftButton,
                event.buttons() & ~Qt.LeftButton,
                event.modifiers()
            )
            super(CanvasView, self).mouseReleaseEvent(fake_event)
        else:
            self.current_tool.on_release(event, self)
            super(CanvasView, self).mouseReleaseEvent(event)

    def updateBrushSize(self):
        print("CanvasView: updateBrushSize called")
        if hasattr(self.current_tool, 'updatePen'):
            try:
                self.current_tool.updatePen(self)
            except Exception as e:
                print(f"CanvasView: Exception in updateBrushSize: {e}")

    def showContextMenu(self, event):
        context_menu = QMenu(self)
        brush_size_widget = QWidget()
        brush_size_layout = QVBoxLayout()
        brush_size_label = QLabel("Размер кисти:")
        brush_size_slider = QSlider(Qt.Horizontal)
        brush_size_slider.setMinimum(1)
        brush_size_slider.setMaximum(100)
        brush_size_slider.setValue(self.settings.brush_size_percentage)
        brush_size_slider.setTickPosition(QSlider.TicksBelow)
        brush_size_slider.setTickInterval(10)
        brush_size_slider.valueChanged.connect(self.changeBrushSize)

        brush_size_layout.addWidget(brush_size_label)
        brush_size_layout.addWidget(brush_size_slider)
        brush_size_widget.setLayout(brush_size_layout)

        brush_size_action = QWidgetAction(self)
        brush_size_action.setDefaultWidget(brush_size_widget)
        context_menu.addAction(brush_size_action)

        # Добавляем действия инструментов
        brush_action = QAction("Кисть (B)", self)
        brush_action.triggered.connect(self.window().selectBrushTool)
        context_menu.addAction(brush_action)

        lasso_fill_action = QAction("Лассо Заливка (L)", self)
        lasso_fill_action.triggered.connect(self.window().selectLassoFillTool)
        context_menu.addAction(lasso_fill_action)

        lasso_erase_action = QAction("Лассо Стирание (E)", self)
        lasso_erase_action.triggered.connect(self.window().selectLassoEraseTool)
        context_menu.addAction(lasso_erase_action)

        eyedropper_action = QAction("Пипетка (I)", self)
        eyedropper_action.triggered.connect(self.window().selectEyedropperTool)
        context_menu.addAction(eyedropper_action)

        # Показываем контекстное меню
        context_menu.exec_(self.mapToGlobal(event.pos()))

    def changeBrushSize(self, value):
        print(f"CanvasView: Brush size percentage changed to {value}%")
        self.settings.brush_size_percentage = value
        self.updateBrushSize()
