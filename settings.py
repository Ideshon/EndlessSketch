# settings.py

from PyQt5.QtGui import QColor

class Settings:
    def __init__(self):
        self.current_color = QColor(0, 0, 0)  # Черный по умолчанию
        self.brush_size = 5  # Размер кисти по умолчанию
        self.eraser_size = 10  # Размер ластика по умолчанию
