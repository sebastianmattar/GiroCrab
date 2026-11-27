# Symbolleiste anpassen (macOS)

Die Symbolleiste des Hauptfensters zeigt den Ansichtsfilter in der Mitte und darum eine linke und eine rechte Gruppe von Symbolen. Die rechte Gruppe steht neben der Suche. GiroCrab speichert die Anordnung selbst (`ToolbarLayout`, #2049). Die Systemanpassung von macOS wird nicht verwendet.

## Aufbau

Ansicht › „Symbolleiste anpassen …“ öffnet ein Sheet über dem Hauptfenster. Es zeigt eine Liste, die der Symbolleiste entspricht. Jede Änderung gilt sofort und wird gespeichert.

| Zeile | Inhalt und Aktionen |
|---|---|
| Ansichtsfilter | Immer oben. Häkchen blendet den Filter der gewählten Ansicht in der Mitte der Leiste ein oder aus. |
| Symbole | Häkchen, Symbol und Name wie in der Leiste. Ziehen ändert die Reihenfolge. Ein Symbol ohne Häkchen bleibt an seiner Stelle in der Liste, erscheint aber nicht in der Leiste. |
| Flexibler Abstand | Trennzeile ohne Häkchen, wird wie die Symbole gezogen. Symbole darüber stehen nach dem Ansichtsfilter, Symbole darunter neben der Suche. |

Unter der Liste stehen ein Hinweis zum Ziehen, „Standard wiederherstellen“ (nur wenn etwas geändert ist) und „Fertig“ (Return). Die Höhe reicht für alle Zeilen ohne Scrollen.

Ausgeblendete Symbole stehen beim nächsten Öffnen am Ende der Liste.

Vor macOS 26 gibt es keinen flexiblen Abstand in der Leiste. Die Liste zeigt dort keine Trennzeile, alle Symbole mit Häkchen bilden eine Gruppe.

Ansicht › „Symbolleiste ausblenden/einblenden“ (⌥⌘T) blendet die Leiste aus und ein.

## Darstellung und Prüfung

- Symbole und Namen sind dieselben wie in der Symbolleiste. Das Sheet zeigt keine persönlichen Daten.
- Prüfen: `--demo-seed --demo-sheet toolbar --screenshot <png>`, auch mit `--text-size larger`.

Gestaltungsregeln stehen in [DESIGN.md](../DESIGN.md).
