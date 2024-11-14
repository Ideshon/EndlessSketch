# tools.py

from PyQt5.QtWidgets import QGraphicsPathItem
from PyQt5.QtGui import QPen, QPainterPath, QColor, QPolygonF
from PyQt5.QtCore import Qt

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
            brush_size = self.settings.get_brush_size(view.viewport().width(), view.viewport().height())
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
                brush_size = self.settings.get_brush_size(view.viewport().width(), view.viewport().height())
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

    def on_press(self, event, view):
        print("LassoFillTool: on_press")
        try:
            scene_pos = view.mapToScene(event.pos())
            self.path = QPainterPath()
            self.path.moveTo(scene_pos)
            self.selection_polygon = [scene_pos]
        except Exception as e:
            print(f"LassoFillTool: Exception in on_press: {e}")

    def on_move(self, event, view):
        print("LassoFillTool: on_move")
        try:
            scene_pos = view.mapToScene(event.pos())
            self.path.lineTo(scene_pos)
            self.selection_polygon.append(scene_pos)

            if self.path_item:
                view.scene().removeItem(self.path_item)
            self.path_item = QGraphicsPathItem()
            pen = QPen(Qt.DotLine)
            pen.setWidthF(2 / self.settings.zoom_factor)  # Fixed width on screen
            self.path_item.setPen(pen)
            self.path_item.setPath(self.path)
            view.scene().addItem(self.path_item)
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

            polygon = QPolygonF(self.selection_polygon)
            pen = QPen(Qt.NoPen)
            brush = self.settings.current_color
            fill_item = view.scene().addPolygon(polygon, pen, brush)
            print(f"LassoFillTool: Filled polygon with color {brush.name()}")
            self.selection_polygon = []
        except Exception as e:
            print(f"LassoFillTool: Exception in on_release: {e}")

class LassoEraseTool:
    def __init__(self, settings):
        self.settings = settings
        self.path_item = None
        self.selection_polygon = []

    def on_press(self, event, view):
        print("LassoEraseTool: on_press")
        try:
            scene_pos = view.mapToScene(event.pos())
            self.path = QPainterPath()
            self.path.moveTo(scene_pos)
            self.selection_polygon = [scene_pos]
        except Exception as e:
            print(f"LassoEraseTool: Exception in on_press: {e}")

    def on_move(self, event, view):
        print("LassoEraseTool: on_move")
        try:
            scene_pos = view.mapToScene(event.pos())
            self.path.lineTo(scene_pos)
            self.selection_polygon.append(scene_pos)

            if self.path_item:
                view.scene().removeItem(self.path_item)
            self.path_item = QGraphicsPathItem()
            pen = QPen(Qt.DotLine)
            pen.setWidthF(2 / self.settings.zoom_factor)  # Fixed width on screen
            self.path_item.setPen(pen)
            self.path_item.setPath(self.path)
            view.scene().addItem(self.path_item)
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

            polygon = QPolygonF(self.selection_polygon)
            pen = QPen(Qt.NoPen)
            brush = QColor(255, 255, 255)  # White color for erase
            fill_item = view.scene().addPolygon(polygon, pen, brush)
            print("LassoEraseTool: Erased polygon area")
            self.selection_polygon = []
        except Exception as e:
            print(f"LassoEraseTool: Exception in on_release: {e}")
