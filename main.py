# main.py

import sys
import logging
from PyQt5.QtWidgets import QApplication, QMessageBox
from canvas_view import CanvasWindow

def main():
    # Настройка логирования
    logging.basicConfig(
        filename='endless_sketch.log',
        level=logging.ERROR,
        format='%(asctime)s [%(levelname)s] %(message)s',
        datefmt='%Y-%m-%d %H:%M:%S'
    )

    app = QApplication(sys.argv)
    window = CanvasWindow()
    window.show()

    try:
        sys.exit(app.exec_())
    except Exception as e:
        # Логируем необработанное исключение
        logging.exception("Unhandled exception occurred:")
        # Отображаем сообщение об ошибке пользователю
        QMessageBox.critical(None, "Критическая ошибка", f"Произошла критическая ошибка:\n{e}")
        sys.exit(1)

if __name__ == '__main__':
    main()
