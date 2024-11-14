# marker.py

from PyQt5.QtWidgets import QGraphicsEllipseItem, QGraphicsTextItem
from PyQt5.QtGui import QBrush, QColor, QPen
from PyQt5.QtCore import Qt, QPointF

class Marker(QGraphicsEllipseItem):
    def __init__(self, position: QPointF, label: str, radius=10):
        super().__init__(-radius, -radius, 2*radius, 2*radius)
        self.setPos(position)
        self.label = label

        # Настройка внешнего вида маркера
        brush = QBrush(QColor(255, 0, 0))  # Красный цвет
        pen = QPen(Qt.black)
        pen.setWidth(1)
        self.setBrush(brush)
        self.setPen(pen)
        self.setZValue(1)  # Поверх всех объектов

        # Добавление текста
        self.text = QGraphicsTextItem(label, self)
        self.text.setDefaultTextColor(Qt.black)
        self.text.setPos(radius + 2, -radius)

        # Установка всплывающей подсказки
        self.setToolTip(label)
