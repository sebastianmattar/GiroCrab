# Symbolleiste anpassen (macOS)

Die Symbolleiste des Hauptfensters zeigt den Ansichtsfilter in der Mitte und darum eine linke und eine rechte Gruppe von Symbolen. Die rechte Gruppe steht neben der Suche. GiroCrab speichert die Anordnung selbst (`ToolbarLayout`, #2049). Die Systemanpassung von macOS wird nicht verwendet.

## Aufbau

Ansicht › „Symbolleiste anpassen …“ öffnet ein Sheet über dem Hauptfenster. Jede Änderung gilt sofort und wird gespeichert.

| Bereich | Inhalt und Aktionen |
|---|---|
| Ansichtsfilter | Schalter: Filter der gewählten Ansicht in der Mitte der Leiste ein- oder ausblenden. |
| Links | Symbole der linken Gruppe in ihrer Reihenfolge. Ziehen ändert die Reihenfolge, der Pfeil verschiebt nach rechts, ⊖ entfernt. |
| Rechts, neben der Suche | Wie „Links“, der Pfeil verschiebt nach links. |
| Weitere Symbole | Nicht gezeigte Symbole. ⊕ fügt links oder rechts hinzu. |
| Unten | „Standard wiederherstellen“ (nur wenn etwas geändert ist) und „Fertig“. |

Vor macOS 26 gibt es keinen flexiblen Abstand zwischen den Gruppen. Das Sheet zeigt dort eine Liste „Symbole“ ohne Links/Rechts. ⊕ hängt am Ende an.

Ansicht › „Symbolleiste ausblenden/einblenden“ (⌥⌘T) blendet die Leiste aus und ein.

## Darstellung und Prüfung

- Symbole und Namen sind dieselben wie in der Symbolleiste. Das Sheet zeigt keine persönlichen Daten.
- Prüfen: `--demo-seed --demo-sheet toolbar --screenshot <png>`, auch mit `--text-size larger`.

Gestaltungsregeln stehen in [DESIGN.md](../DESIGN.md).
