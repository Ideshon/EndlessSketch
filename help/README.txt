EndlessSketch Help translations

The application loads every valid *.json file in this directory at startup.
Files with id "ru" or "en" replace the built-in Russian or English fallback.
A file with another unique id adds another language tab without recompiling.

Required JSON structure:

{
  "id": "de",
  "tab_label": "Deutsch",
  "title": "EndlessSketch Hilfe",
  "intro": "Short introduction.",
  "sections": [
    {
      "title": "Section",
      "items": [
        {
          "name": "Control or setting",
          "description": "What it does and its tradeoffs."
        }
      ]
    }
  ]
}

Rules:
- Save the file as UTF-8 JSON.
- id must contain only ASCII letters, digits, hyphen, or underscore.
- id, tab_label, title, intro, section titles, item names, and descriptions cannot be empty.
- Restart EndlessSketch after editing or adding a translation.
- Invalid external files are ignored; the built-in Russian and English Help remains available.
