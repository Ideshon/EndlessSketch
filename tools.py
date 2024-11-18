# tools.py

from PyQt5.QtWidgets import QGraphicsPathItem, QGraphicsPolygonItem, QApplication
from PyQt5.QtGui import QPen, QPainterPath, QColor, QPolygonF, QBrush, QScreen
from PyQt5.QtCore import Qt, QPointF

class BrushTool:
    def __init__(self, settings):
        self.settings = settings
        self.path_item = None

    def on_press(self, event, view):
        print("BrushTool: on_press")
        try:
            scene_pos = view.mapToScene(event.pos())
            self.path = QPainterPath()
            self.path.moveTo(scene_pos)

            self.path_item = QGraphicsPathItem()
            brush_size = self.settings.get_brush_size(
                view.viewport().width(), view.viewport().height(), view.zoom_factor)
            pen = QPen(self.settings.current_color, brush_size)
            pen.setCapStyle(Qt.RoundCap)
            pen.setJoinStyle(Qt.RoundJoin)
            self.path_item.setPen(pen)
            view.scene().addItem(self.path_item)
            print(f"BrushTool: Created path_item with brush size {pen.width()}")
        except Exception as e:
            print(f"BrushTool: Exception in on_press: {e}")

    def on_move(self, event, view):
        if self.path_item:
            print("BrushTool: on_move")
            try:
                scene_pos = view.mapToScene(event.pos())
                self.path.lineTo(scene_pos)
                self.path_item.setPath(self.path)
            except Exception as e:
                print(f"BrushTool: Exception in on_move: {e}")

    def on_release(self, event, view):
        print("BrushTool: on_release")
        self.path_item = None

    def updatePen(self, view):
        print("BrushTool: updatePen")
        if self.path_item:
            try:
                brush_size = self.settings.get_brush_size(
                    view.viewport().width(), view.viewport().height(), view.zoom_factor)
                pen = QPen(self.settings.current_color, brush_size)
                pen.setCapStyle(Qt.RoundCap)
                pen.setJoinStyle(Qt.RoundJoin)
                self.path_item.setPen(pen)
                print(f"BrushTool: Updated pen with new brush size {brush_size}")
            except Exception as e:
                print(f"BrushTool: Exception in updatePen: {e}")

class LassoFillTool:
    def __init__(self, settings):
        self.settings = settings
        self.path_item = None
        self.selection_polygon = []
        self.path = None

    def on_press(self, event, view):
        print("LassoFillTool: on_press")
        try:
            scene_pos = view.mapToScene(event.pos())
            self.path = QPainterPath()
            self.path.moveTo(scene_pos)
            self.selection_polygon = [scene_pos]

            self.path_item = QGraphicsPathItem()
            pen = QPen(Qt.DotLine)
            pen.setWidthF(2 / view.zoom_factor)
            self.path_item.setPen(pen)
            view.scene().addItem(self.path_item)
        except Exception as e:
            print(f"LassoFillTool: Exception in on_press: {e}")

    def on_move(self, event, view):
        if self.path_item:
            print("LassoFillTool: on_move")
            try:
                scene_pos = view.mapToScene(event.pos())
                self.path.lineTo(scene_pos)
                self.selection_polygon.append(scene_pos)
                self.path_item.setPath(self.path)
            except Exception as e:
                print(f"LassoFillTool: Exception in on_move: {e}")

    def on_release(self, event, view):
        print("LassoFillTool: on_release")
        try:
            if self.path_item:
                view.scene().removeItem(self.path_item)
                self.path_item = None

            if not self.selection_polygon:
                print("LassoFillTool: No selection polygon")
                return

            # Замыкаем полигон, если необходимо
            if self.selection_polygon[0] != self.selection_polygon[-1]:
                self.selection_polygon.append(self.selection_polygon[0])

            polygon = QPolygonF(self.selection_polygon)
            pen = QPen(Qt.NoPen)
            brush = QBrush(self.settings.current_color)
            fill_item = QGraphicsPolygonItem()
            fill_item.setPolygon(polygon)
            fill_item.setPen(pen)
            fill_item.setBrush(brush)
            view.scene().addItem(fill_item)
            print(f"LassoFillTool: Filled polygon with color {self.settings.current_color.name()}")
            self.selection_polygon = []
            self.path = None
        except Exception as e:
            print(f"LassoFillTool: Exception in on_release: {e}")

class LassoEraseTool:
    def __init__(self, settings):
        self.settings = settings
        self.path_item = None
        self.selection_polygon = []
        self.path = None

    def on_press(self, event, view):
        print("LassoEraseTool: on_press")
        try:
            scene_pos = view.mapToScene(event.pos())
            self.path = QPainterPath()
            self.path.moveTo(scene_pos)
            self.selection_polygon = [scene_pos]

            self.path_item = QGraphicsPathItem()
            pen = QPen(Qt.DotLine)
            pen.setWidthF(2 / view.zoom_factor)
            self.path_item.setPen(pen)
            view.scene().addItem(self.path_item)
        except Exception as e:
            print(f"LassoEraseTool: Exception in on_press: {e}")

    def on_move(self, event, view):
        if self.path_item:
            print("LassoEraseTool: on_move")
            try:
                scene_pos = view.mapToScene(event.pos())
                self.path.lineTo(scene_pos)
                self.selection_polygon.append(scene_pos)
                self.path_item.setPath(self.path)
            except Exception as e:
                print(f"LassoEraseTool: Exception in on_move: {e}")

    def on_release(self, event, view):
        print("LassoEraseTool: on_release")
        try:
            if self.path_item:
                view.scene().removeItem(self.path_item)
                self.path_item = None

            if not self.selection_polygon:
                print("LassoEraseTool: No selection polygon")
                return

            # Замыкаем полигон, если необходимо
            if self.selection_polygon[0] != self.selection_polygon[-1]:
                self.selection_polygon.append(self.selection_polygon[0])

            polygon = QPolygonF(self.selection_polygon)
            pen = QPen(Qt.NoPen)
            brush = QBrush(QColor(255, 255, 255))  # Белый цвет для стирания
            fill_item = QGraphicsPolygonItem()
            fill_item.setPolygon(polygon)
            fill_item.setPen(pen)
            fill_item.setBrush(brush)
            view.scene().addItem(fill_item)
            print("LassoEraseTool: Erased polygon area")
            self.selection_polygon = []
            self.path = None
        except Exception as e:
            print(f"LassoEraseTool: Exception in on_release: {e}")

class EyedropperTool:
    def __init__(self, settings):
        self.settings = settings

    def on_press(self, event, view):
        print("EyedropperTool: on_press")
        try:
            # Получаем глобальные координаты курсора
            global_pos = view.viewport().mapToGlobal(event.pos())

            # Получаем экран, на котором находится курсор
            screen = QApplication.screenAt(global_pos)
            if not screen:
                print("EyedropperTool: No screen found at cursor position")
                return

            # Учитываем коэффициент масштабирования экрана
            scale_factor = screen.devicePixelRatio()

            # Захватываем цвет пикселя под курсором
            x = int(global_pos.x() * scale_factor)
            y = int(global_pos.y() * scale_factor)
            screenshot = screen.grabWindow(0, x, y, 1, 1)

            if not screenshot.isNull():
                image = screenshot.toImage()
                if not image.isNull():
                    color = image.pixelColor(0, 0)
                    self.settings.current_color = color
                    print(f"EyedropperTool: Color picked {color.name()}")
                else:
                    print("EyedropperTool: Failed to get image from screenshot")
            else:
                print("EyedropperTool: Failed to grab screenshot")
        except Exception as e:
            print(f"EyedropperTool: Exception in on_press: {e}")

    def on_move(self, event, view):
        pass

    def on_release(self, event, view):
        pass
