# canvas_view.py

import sys
import logging
from PyQt5.QtWidgets import (
    QMainWindow, QGraphicsView, QGraphicsScene, QToolBar, QAction,
    QColorDialog, QSlider, QLabel, QFileDialog, QGraphicsPathItem,
    QMenu, QWidgetAction, QWidget, QVBoxLayout, QHBoxLayout, QStatusBar, QGraphicsPolygonItem, QMessageBox
)
from PyQt5.QtGui import (
    QPainter, QMouseEvent, QWheelEvent, QPainterPath, QPen, QColor, QBrush, QPolygonF
)
from PyQt5.QtCore import Qt, QEvent, QPointF
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
        try:
            # Создаем сцену и представление
            self.scene = QGraphicsScene()
            self.view = CanvasView(self.scene, self.settings)
            self.setCentralWidget(self.view)

            # Создаем панель инструментов
            self.createToolBar()

            # Создаем меню для сохранения и загрузки
            self.createMenuBar()

            # Добавляем ползунок размера кисти в статус-бар
            self.createStatusBar()
        except Exception as e:
            logging.exception("Exception in initUI:")
            QMessageBox.critical(self, "Ошибка", f"Произошла ошибка при инициализации интерфейса:\n{e}")
            sys.exit(1)

    def createToolBar(self):
        try:
            toolbar = QToolBar("Инструменты")
            self.addToolBar(toolbar)

            # Инструмент кисти
            brush_action = QAction("Кисть", self)
            brush_action.triggered.connect(self.selectBrushTool)
            brush_action.setShortcut("B")  # Горячая клавиша B
            toolbar.addAction(brush_action)

            # Лассо Заливка
            lasso_fill_action = QAction("ЛЗаливка", self)
            lasso_fill_action.triggered.connect(self.selectLassoFillTool)
            lasso_fill_action.setShortcut("L")  # Горячая клавиша L
            toolbar.addAction(lasso_fill_action)

            # Лассо Стирание
            lasso_erase_action = QAction("ЛСтирание", self)
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
            color_action.setShortcut("C")  # Горячая клавиша Ctrl+C
            toolbar.addAction(color_action)

            # Кнопка Справка
            help_action = QAction("Справка", self)
            help_action.triggered.connect(self.showHelp)
            help_action.setShortcut("F1")  # Горячая клавиша F1
            toolbar.addAction(help_action)
        except Exception as e:
            logging.exception("Exception in createToolBar:")
            QMessageBox.critical(self, "Ошибка", f"Произошла ошибка при создании панели инструментов:\n{e}")

    def createStatusBar(self):
        try:
            status_bar = QStatusBar()
            self.setStatusBar(status_bar)

            brush_size_label = QLabel("Размер кисти:")
            status_bar.addPermanentWidget(brush_size_label)

            self.brush_slider = QSlider(Qt.Horizontal)
            self.brush_slider.setMinimum(1)
            self.brush_slider.setMaximum(100)
            self.brush_slider.setValue(self.settings.brush_size_percentage)
            self.brush_slider.setTickPosition(QSlider.TicksBelow)
            self.brush_slider.setTickInterval(10)
            self.brush_slider.valueChanged.connect(self.changeBrushSize)
            status_bar.addPermanentWidget(self.brush_slider)
        except Exception as e:
            logging.exception("Exception in createStatusBar:")
            QMessageBox.critical(self, "Ошибка", f"Произошла ошибка при создании статус-бара:\n{e}")

    def createMenuBar(self):
        try:
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
            save_place_action.setShortcut("Ctrl+Shift+S")  # Горячая клавиша Ctrl+Shift+S
            file_menu.addAction(save_place_action)

            # Загрузить место
            load_place_action = QAction('Загрузить место', self)
            load_place_action.triggered.connect(self.loadPlace)
            load_place_action.setShortcut("Ctrl+Shift+O")  # Горячая клавиша Ctrl+Shift+O
            file_menu.addAction(load_place_action)
        except Exception as e:
            logging.exception("Exception in createMenuBar:")
            QMessageBox.critical(self, "Ошибка", f"Произошла ошибка при создании меню:\n{e}")

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
        try:
            color = QColorDialog.getColor()
            if color.isValid():
                self.settings.current_color = color
                print(f"CanvasWindow: Color changed to {color.name()}")
        except Exception as e:
            logging.exception("Exception in chooseColor:")
            QMessageBox.critical(self, "Ошибка", f"Произошла ошибка при выборе цвета:\n{e}")

    def changeBrushSize(self, value):
        try:
            print(f"CanvasWindow: Brush size percentage changed to {value}%")
            self.settings.brush_size_percentage = value
            self.view.updateBrushSize()
        except Exception as e:
            logging.exception("Exception in changeBrushSize:")
            QMessageBox.critical(self, "Ошибка", f"Произошла ошибка при изменении размера кисти:\n{e}")

    def saveCanvas(self):
        print("CanvasWindow: Saving canvas")
        try:
            options = QFileDialog.Options()
            filename, _ = QFileDialog.getSaveFileName(self, "Сохранить холст", "",
                                                      "EndlessSketch Files (*.ess)", options=options)
            if filename:
                data = []
                for item in reversed(self.scene.items()):
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
                    elif isinstance(item, QGraphicsPolygonItem):
                        polygon = item.polygon()
                        points = [(point.x(), point.y()) for point in polygon]
                        brush = item.brush()
                        item_data = {
                            'type': 'polygon',
                            'color': brush.color().name(),
                            'points': points
                        }
                        data.append(item_data)
                with open(filename, 'w') as f:
                    json.dump(data, f, indent=4)
                print(f"CanvasWindow: Canvas saved to {filename}")
        except Exception as e:
            logging.exception("Exception in saveCanvas:")
            QMessageBox.critical(self, "Ошибка", f"Произошла ошибка при сохранении холста:\n{e}")

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
                for item_data in data:  # Загружаем в том же порядке
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
                    elif item_data['type'] == 'polygon':
                        points = [QPointF(x, y) for x, y in item_data['points']]
                        polygon = QPolygonF(points)
                        brush_color = QColor(item_data['color'])
                        brush = QBrush(brush_color)
                        pen = QPen(Qt.NoPen)
                        polygon_item = QGraphicsPolygonItem()
                        polygon_item.setPolygon(polygon)
                        polygon_item.setBrush(brush)
                        polygon_item.setPen(pen)
                        self.scene.addItem(polygon_item)
                print(f"CanvasWindow: Canvas loaded from {filename}")
        except Exception as e:
            logging.exception("Exception in loadCanvas:")
            QMessageBox.critical(self, "Ошибка", f"Произошла ошибка при загрузке холста:\n{e}")

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
            logging.exception("Exception in savePlace:")
            QMessageBox.critical(self, "Ошибка", f"Произошла ошибка при сохранении места:\n{e}")

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

                scale_factor = target_zoom / self.view.zoom_factor
                self.view.scale(scale_factor, scale_factor)
                self.view.zoom_factor = target_zoom
                print(f"CanvasWindow: Zoom factor set to {self.view.zoom_factor}")

                # Center view on saved coordinates
                self.view.centerOn(place['x'], place['y'])
                print(f"CanvasWindow: Place loaded from {filename}")

                # Обновляем размер кисти после изменения масштаба
                self.view.updateBrushSize()
        except Exception as e:
            logging.exception("Exception in loadPlace:")
            QMessageBox.critical(self, "Ошибка", f"Произошла ошибка при загрузке места:\n{e}")

    def resetZoom(self):
        print("CanvasWindow: Resetting zoom to 1.0")
        # Reset the view's scale to original
        self.view.resetTransform()
        self.view.zoom_factor = 1.0
        print("CanvasWindow: Zoom reset to 1.0")

    def keyPressEvent(self, event):
        try:
            if event.key() == Qt.Key_C and event.modifiers() & Qt.ControlModifier:
                self.chooseColor()
            elif event.key() == Qt.Key_S and event.modifiers() & Qt.ControlModifier and event.modifiers() & Qt.ShiftModifier:
                self.savePlace()
            elif event.key() == Qt.Key_F1:
                self.showHelp()
            else:
                super().keyPressEvent(event)
        except Exception as e:
            logging.exception("Exception in keyPressEvent:")
            QMessageBox.critical(self, "Ошибка", f"Произошла ошибка при обработке нажатия клавиши:\n{e}")

    def showHelp(self):
        try:
            print("CanvasWindow: Showing help")
            help_text = (
                "<h2>EndlessSketch - Справка</h2>"
                "<p>EndlessSketch - это приложение для рисования на бесконечном холсте (еще нет).</p>"
                "<h3>Инструменты:</h3>"
                "<ul>"
                "<li><b>Кисть (B):</b> Рисование свободной линией.</li>"
                "<li><b>Лассо Заливка (L):</b> Создание произвольной залитой области.</li>"
                "<li><b>Лассо Стирание (E):</b> Стирание произвольной области.</li>"
                "<li><b>Пипетка (I):</b> Выбор цвета из области холста.</li>"
                "</ul>"
                "<h3>Горячие клавиши:</h3>"
                "<ul>"
                "<li><b>B:</b> Выбрать инструмент Кисть.</li>"
                "<li><b>L:</b> Выбрать инструмент Лассо Заливка.</li>"
                "<li><b>E:</b> Выбрать инструмент Лассо Стирание.</li>"
                "<li><b>I:</b> Выбрать инструмент Пипетка.</li>"
                "<li><b>C:</b> Выбрать цвет.</li>"
                "<li><b>Ctrl+S:</b> Сохранить холст.</li>"
                "<li><b>Ctrl+O:</b> Загрузить холст.</li>"
                "<li><b>Ctrl+Shift+S:</b> Сохранить место.</li>"
                "<li><b>Ctrl+Shift+O:</b> Загрузить место.</li>"
                "<li><b>F1:</b> Открыть справку.</li>"
                "</ul>"
                "<h3>Изменение размера кисти:</h3>"
                "<p>Удерживайте клавишу <b>Shift</b> и прокручивайте колесико мыши для изменения размера кисти.</p>"
                "<h3>Перемещение по холсту:</h3>"
                "<p>Удерживайте <b>среднюю кнопку мыши</b> и перетаскивайте холст.</p>"
                "<h3>Масштабирование:</h3>"
                "<p>Используйте колесико мыши для увеличения или уменьшения масштаба.</p>"
            )
            QMessageBox.information(self, "Справка - EndlessSketch", help_text)
        except Exception as e:
            logging.exception("Exception in showHelp:")
            QMessageBox.critical(self, "Ошибка", f"Произошла ошибка при отображении справки:\n{e}")

class CanvasView(QGraphicsView):
    def __init__(self, scene, settings):
        try:
            super().__init__(scene)
            self.settings = settings
            self.current_tool = BrushTool(self.settings)
            self.setRenderHint(QPainter.Antialiasing)
            self.last_point = None
            self.setDragMode(QGraphicsView.NoDrag)
            self.zoom_factor = 1.0  # Изначальный масштаб
            print(f"CanvasView: Initialized with zoom_factor = {self.zoom_factor}")

            # Отключаем прокрутку
            self.setHorizontalScrollBarPolicy(Qt.ScrollBarAlwaysOff)
            self.setVerticalScrollBarPolicy(Qt.ScrollBarAlwaysOff)
            self.setTransformationAnchor(QGraphicsView.AnchorUnderMouse)
            self.setResizeAnchor(QGraphicsView.NoAnchor)

            # Устанавливаем большой размер сцены
            self.setSceneRect(-1e10, -1e10, 2e10, 2e10)

            # Разрешаем перетаскивание холста
            self.viewport().setCursor(Qt.CrossCursor)
        except Exception as e:
            logging.exception("Exception in CanvasView __init__:")
            QMessageBox.critical(self, "Ошибка", f"Произошла ошибка при инициализации CanvasView:\n{e}")
            sys.exit(1)

    def wheelEvent(self, event: QWheelEvent):
        try:
            if event.modifiers() & Qt.ShiftModifier:
                # Изменение размера кисти
                delta = event.angleDelta().y() / 8  # Получаем число "щелчков" колесика
                self.changeBrushSizeByDelta(delta)
            else:
                # Масштабирование
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
                self.updateBrushSize()
        except Exception as e:
            logging.exception("Exception in wheelEvent:")
            QMessageBox.critical(self, "Ошибка", f"Произошла ошибка при обработке события колесика мыши:\n{e}")

    def changeBrushSizeByDelta(self, delta):
        try:
            # Изменяем процент размера кисти в зависимости от прокрутки
            step = 1  # Шаг изменения размера кисти в процентах
            if delta > 0:
                self.settings.brush_size_percentage += step
            else:
                self.settings.brush_size_percentage -= step

            # Ограничиваем значение между 1 и 100
            self.settings.brush_size_percentage = max(1, min(100, self.settings.brush_size_percentage))

            # Обновляем ползунок в статус-баре
            self.parent().brush_slider.setValue(self.settings.brush_size_percentage)
            print(f"CanvasView: Brush size changed to {self.settings.brush_size_percentage}% via wheel")
            self.updateBrushSize()
        except Exception as e:
            logging.exception("Exception in changeBrushSizeByDelta:")
            QMessageBox.critical(self, "Ошибка", f"Произошла ошибка при изменении размера кисти:\n{e}")

    def mousePressEvent(self, event: QMouseEvent):
        try:
            if event.button() == Qt.MiddleButton:
                print("CanvasView: Middle mouse button pressed - activating drag mode")
                self.setDragMode(QGraphicsView.ScrollHandDrag)
                self.viewport().setCursor(Qt.ClosedHandCursor)
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
        except Exception as e:
            logging.exception("Exception in mousePressEvent:")
            QMessageBox.critical(self, "Ошибка", f"Произошла ошибка при обработке нажатия мыши:\n{e}")

    def mouseMoveEvent(self, event: QMouseEvent):
        try:
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
        except Exception as e:
            logging.exception("Exception in mouseMoveEvent:")
            QMessageBox.critical(self, "Ошибка", f"Произошла ошибка при обработке движения мыши:\n{e}")

    def mouseReleaseEvent(self, event: QMouseEvent):
        try:
            if event.button() == Qt.MiddleButton:
                print("CanvasView: Middle mouse button released - deactivating drag mode")
                self.setDragMode(QGraphicsView.NoDrag)
                self.viewport().setCursor(Qt.CrossCursor)
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
        except Exception as e:
            logging.exception("Exception in mouseReleaseEvent:")
            QMessageBox.critical(self, "Ошибка", f"Произошла ошибка при обработке отпускания мыши:\n{e}")

    def updateBrushSize(self):
        print("CanvasView: updateBrushSize called")
        if hasattr(self.current_tool, 'updatePen'):
            try:
                self.current_tool.updatePen(self)
            except Exception as e:
                logging.exception("Exception in updateBrushSize:")
                QMessageBox.critical(self, "Ошибка", f"Произошла ошибка при обновлении размера кисти:\n{e}")

    def showContextMenu(self, event):
        try:
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
        except Exception as e:
            logging.exception("Exception in showContextMenu:")
            QMessageBox.critical(self, "Ошибка", f"Произошла ошибка при отображении контекстного меню:\n{e}")

    def changeBrushSize(self, value):
        try:
            print(f"CanvasView: Brush size percentage changed to {value}%")
            self.settings.brush_size_percentage = value
            self.parent().brush_slider.setValue(value)
            self.updateBrushSize()
        except Exception as e:
            logging.exception("Exception in changeBrushSize:")
            QMessageBox.critical(self, "Ошибка", f"Произошла ошибка при изменении размера кисти:\n{e}")
