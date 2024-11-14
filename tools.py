# tools.py

from PyQt5.QtWidgets import QGraphicsPathItem, QGraphicsItemGroup
from PyQt5.QtGui import QPen, QPainterPath, QColor, QPolygonF
from PyQt5.QtCore import Qt

class BrushTool:
    def __init__(self, settings):
        self.settings = settings
        self.path_item = None

    def on_press(self, event, view):
        scene_pos = view.mapToScene(event.pos())
        self.path = QPainterPath()
        self.path.moveTo(scene_pos)

        self.path_item = QGraphicsPathItem()
        pen = QPen(self.settings.current_color, self.settings.brush_size)
        pen.setCapStyle(Qt.RoundCap)
        pen.setJoinStyle(Qt.RoundJoin)
        self.path_item.setPen(pen)
        view.scene().addItem(self.path_item)

    def on_move(self, event, view):
        if self.path_item:
            scene_pos = view.mapToScene(event.pos())
            self.path.lineTo(scene_pos)
            self.path_item.setPath(self.path)

    def on_release(self, event, view):
        self.path_item = None

class EraserTool:
    def __init__(self, settings):
        self.settings = settings
        self.path_item = None

    def on_press(self, event, view):
        scene_pos = view.mapToScene(event.pos())
        self.path = QPainterPath()
        self.path.moveTo(scene_pos)

        self.path_item = QGraphicsPathItem()
        pen = QPen(Qt.white, self.settings.eraser_size)
        pen.setCapStyle(Qt.RoundCap)
        pen.setJoinStyle(Qt.RoundJoin)
        self.path_item.setPen(pen)
        view.scene().addItem(self.path_item)

    def on_move(self, event, view):
        if self.path_item:
            scene_pos = view.mapToScene(event.pos())
            self.path.lineTo(scene_pos)
            self.path_item.setPath(self.path)

    def on_release(self, event, view):
        self.path_item = None

class LassoFillTool:
    def __init__(self, settings):
        self.settings = settings
        self.path_item = None
        self.selection_polygon = None

    def on_press(self, event, view):
        scene_pos = view.mapToScene(event.pos())
        self.path = QPainterPath()
        self.path.moveTo(scene_pos)
        self.selection_polygon = [scene_pos]

    def on_move(self, event, view):
        scene_pos = view.mapToScene(event.pos())
        self.path.lineTo(scene_pos)
        self.selection_polygon.append(scene_pos)

        if self.path_item:
            view.scene().removeItem(self.path_item)
        self.path_item = QGraphicsPathItem()
        pen = QPen(Qt.DotLine)
        self.path_item.setPen(pen)
        self.path_item.setPath(self.path)
        view.scene().addItem(self.path_item)

    def on_release(self, event, view):
        if self.path_item:
            view.scene().removeItem(self.path_item)
            self.path_item = None

        polygon = QPolygonF(self.selection_polygon)
        pen = QPen(Qt.NoPen)
        brush = self.settings.current_color
        fill_item = view.scene().addPolygon(polygon, pen, brush)

        self.selection_polygon = None

class LassoEraseTool:
    def __init__(self, settings):
        self.settings = settings
        self.path_item = None
        self.selection_polygon = None

    def on_press(self, event, view):
        scene_pos = view.mapToScene(event.pos())
        self.path = QPainterPath()
        self.path.moveTo(scene_pos)
        self.selection_polygon = [scene_pos]

    def on_move(self, event, view):
        scene_pos = view.mapToScene(event.pos())
        self.path.lineTo(scene_pos)
        self.selection_polygon.append(scene_pos)

        if self.path_item:
            view.scene().removeItem(self.path_item)
        self.path_item = QGraphicsPathItem()
        pen = QPen(Qt.DotLine)
        self.path_item.setPen(pen)
        self.path_item.setPath(self.path)
        view.scene().addItem(self.path_item)

    def on_release(self, event, view):
        if self.path_item:
            view.scene().removeItem(self.path_item)
            self.path_item = None

        polygon = QPolygonF(self.selection_polygon)
        pen = QPen(Qt.NoPen)
        brush = QColor(255, 255, 255)
        fill_item = view.scene().addPolygon(polygon, pen, brush)

        self.selection_polygon = None
