# settings.py

from PyQt5.QtGui import QColor


class Settings:
    def __init__(self):
        self.current_color = QColor(0, 0, 0)  # Черный по умолчанию
        self.brush_size_percentage = 5  # Размер кисти в процентах (1-100)

    def get_brush_size(self, view_width, view_height, zoom_factor):
        # Ограничиваем brush_size_percentage до диапазона 1-100
        self.brush_size_percentage = max(1, min(self.brush_size_percentage, 100))

        # Рассчитываем размер кисти на основе процента и масштаба
        min_dim = min(view_width, view_height)
        base_size = min_dim * (self.brush_size_percentage / 100)

        if zoom_factor <= 0:
            print("Settings: Invalid zoom_factor <= 0, resetting to 1.0")
            zoom_factor = 1.0

        actual_size = base_size / zoom_factor
        brush_size = max(1, actual_size)  # Минимальный размер кисти - 1 пиксель
        print(f"Settings: Calculated brush size: {brush_size}")
        return brush_size
