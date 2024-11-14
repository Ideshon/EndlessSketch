# main.py

import sys
from PyQt5.QtWidgets import QApplication
from canvas_view import CanvasWindow

def main():
    app = QApplication(sys.argv)
    window = CanvasWindow()
    window.show()
    sys.exit(app.exec_())

if __name__ == '__main__':
    main()
