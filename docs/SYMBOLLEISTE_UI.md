# Symbolleiste anpassen (macOS)

Die Symbolleiste des Hauptfensters zeigt den Ansichtsfilter in der Mitte und darum eine linke und eine rechte Gruppe von Symbolen. Die rechte Gruppe steht neben der Suche. GiroCrab speichert die Anordnung selbst (`ToolbarLayout`, #2049). Die Systemanpassung von macOS wird nicht verwendet.

## Aufbau

Ansicht › „Symbolleiste anpassen …“ öffnet ein Sheet über dem Hauptfenster. Es zeigt eine Liste, die der Symbolleiste entspricht. Jede Änderung gilt sofort und wird gespeichert.

Über der Liste steht das Häkchen „Ansichtsfilter in der Mitte anzeigen“. Der Filter steht in der Leiste immer in der Mitte und ist deshalb keine Zeile der Liste.

| Zeile | Inhalt und Aktionen |
|---|---|
| Symbole | Häkchen, Symbol und Name wie in der Leiste. Ein Symbol ohne Häkchen bleibt an seiner Stelle in der Liste, erscheint aber nicht in der Leiste. Symbol und Name sind dann in der sekundären Textfarbe gesetzt (mindestens 4,5:1). |
| Flexibler Zwischenraum | Trennzeile ohne Häkchen. Symbole darüber stehen nach dem Ansichtsfilter, Symbole darunter neben der Suche. |

Jede Zeile lässt sich ziehen. Ohne Ziehen geht es über das Kontextmenü der Zeile: „Nach oben“, „Nach unten“ und bei Symbolen „Neben die Suche“ oder „Nach links“, je nach Seite. Was nichts bewirkt, ist deaktiviert. VoiceOver bietet dieselben Befehle als Aktionen der Zeile an.

Unter der Liste stehen ein Hinweis zum Sortieren, „Standard wiederherstellen“ (nur wenn etwas geändert ist) und „Fertig“ (Return). Die Höhe reicht für alle Zeilen ohne Scrollen.

Ausgeblendete Symbole stehen beim nächsten Öffnen am Ende der Liste.

Vor macOS 26 gibt es keinen flexiblen Zwischenraum in der Leiste. Die Liste zeigt dort keine Trennzeile und keinen Seitenwechsel, alle Symbole mit Häkchen bilden eine Gruppe.

Ansicht › „Symbolleiste ausblenden/einblenden“ (⌥⌘T) blendet die Leiste aus und ein.

## Darstellung und Prüfung

- Symbole und Namen sind dieselben wie in der Symbolleiste. Das Sheet zeigt keine persönlichen Daten.
- Prüfen: `--demo-seed --demo-sheet toolbar --screenshot <png>`, auch mit `--text-size larger`.

Gestaltungsregeln stehen in [DESIGN.md](../DESIGN.md).
