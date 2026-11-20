# Empfänger (UI-Aufbau)

Diese Seite beschreibt die Empfängervorschläge der „Neuen Überweisung“ (`TransferSheetView.suggestionList`) und das
Blatt „Empfänger“ (`KnownRecipientsSheet`), was sie zeigen, was sie nicht tun und welche Regeln für Änderungen
gelten (#881, #978). Sie gilt für macOS und iPhone/iPad; Linux folgt denselben Regeln (`TransferSheet.qml`,
`KnownRecipientsDialog.qml`).

## Grundprinzip

Es gibt keine gespeicherte Empfängerliste. Jeder Eintrag kommt aus den ausgehenden Buchungen mit IBAN
(`recipients.rs`): **eine Zeile je IBAN** unter dem Namen, unter dem am häufigsten an sie gezahlt wurde, mit der Zahl
der Buchungen und dem Datum der letzten. Sortiert wird nach der letzten Nutzung, dann nach der Anzahl: Wer das Konto
gewechselt hat, steht mit der neuen IBAN vor der alten, auch wenn an die alte jahrelang öfter gezahlt wurde.

„Nicht mehr vorschlagen“ blendet eine IBAN aus (`hidden_recipients`), löscht aber nichts: Die Buchungen bleiben, und
das Blatt „Empfänger“ blendet sie wieder ein. Ausblenden gilt für die IBAN unter allen Namen. Eine ausgeblendete IBAN
bleibt ausgeblendet, auch wenn später wieder an sie gezahlt wird. Die Änderung erreicht über iCloud die anderen
Geräte (neueste Änderung gewinnt).

## Vorschläge im Formular

- Erst sobald etwas im Feld „Empfänger“ steht; höchstens fünf, ausgeblendete nie. Die Suche trifft jede Schreibweise
  des Namens und die IBAN.
- Zeile: Name und „zuletzt <Datum>“, darunter die IBAN. Klick, Tippen oder ↑/↓ und Return übernehmen Name und IBAN.
- Ausblenden: das ✕ der Zeile unter dem Zeiger (Mac), das Kontextmenü (Rechtsklick, langes Drücken) oder die
  Bedienungshilfen-Aktion. Die Liste rechnet sofort neu, die Markierung bleibt an ihrem Platz (die nächste Zeile
  rückt nach). Schließt das Blatt „Empfänger“, rechnet eine offene Liste ebenfalls neu.
- Linux zeigt die Vorschläge als Chips mit einem ✕ und ohne Kontextmenü: Ein Menü nimmt dem Namensfeld den Fokus,
  und ohne Fokus verschwinden die Chips. Das ✕ nimmt keinen Fokus.
- Der Aufruf läuft synchron bei jedem Tastendruck: Der Core hält die Liste, bis sich die Daten ändern
  (`vault/cache.rs`, auch nach dem Wiederherstellen einer Sicherung), und liest sie über den Index `tx_recipients`
  neu, nur die SQL-Abfrage unter der Tresor-Sperre (Messwerte in `examples/category_perf.rs`). Wer die Abfrage
  ändert, misst dort erneut; über ~16 ms gehört sie hinter `vaultOffMain`.

## Blatt „Empfänger“

Geöffnet über „Empfänger“ im Kopf des Formulars. Oben ein Satz, was die Liste ist, und die Suche (Name oder IBAN,
gefiltert im Core; nur das Ergebnis der jüngsten Suche wird gezeigt). Darunter „Ausgeblendet“ zuerst (meist wenige
Zeilen, so ist Rückgängigmachen nah), dann „Vorgeschlagen“. Jede Zeile hat „Ausblenden“ bzw. „Einblenden“ und
dasselbe im Kontextmenü. Scheitert das Lesen, bleiben die zuletzt gelesenen Zeilen stehen; unter der Suche steht
„Empfänger konnten nicht geladen werden“ mit „Erneut versuchen“ (nie „Noch keine Empfänger“). Ein Tresorwechsel
(Demo ↔ eigene Daten) bei offenem Blatt liest die Liste neu.

## Regeln

- Namen, IBANs und das Suchfeld tragen `.sensitive()`; Anzahl und Datum bleiben im Bild „Problem melden“ sichtbar. Das Blatt wird
  mit `.capturableSheet` gezeigt.
- Keine Löschfunktion für Buchungen und kein „Bearbeiten“ von Namen oder IBANs hier: Das wäre eine Änderung an
  Buchungen bzw. eine Vorlage (dafür gibt es „Vorlagen“).
- Neue Filter oder Sortierungen gehören in den Core (`matching_recipients`), damit Mac, iPhone und Linux dieselbe
  Liste zeigen.
