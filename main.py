# main.py

import sys
import logging
from PyQt5.QtWidgets import QApplication
from canvas_view import CanvasWindow

# Настройка логирования
logging.basicConfig(
    level=logging.DEBUG,
    format='%(asctime)s - %(levelname)s - %(message)s',
    handlers=[
        logging.StreamHandler(sys.stdout),
        logging.FileHandler("endlesssketch.log", mode='w')
    ]
)

def main():
    logging.debug("Запуск приложения EndlessSketch")
    app = QApplication(sys.argv)
    window = CanvasWindow()
    window.show()
    logging.debug("Приложение запущено")
    sys.exit(app.exec_())

if __name__ == '__main__':
    main()
