# Changelog – GiroCrab für Linux

Die Linux-Oberfläche ist ein Community-Port der Mac-App
([#389](https://github.com/GiroCrab/GiroCrab/issues/389)). Sie folgt der Mac-App in eigenem
Tempo: Der Kern – Bankanbindungen, verschlüsselter Tresor, Kategorisierung, Auswertungen – ist
immer der aktuelle, die Oberfläche holt die Funktionen der Mac-App schrittweise nach. Aufbau,
Verhalten und Texte kommen dabei 1:1 aus der Mac-App, auf Deutsch und Englisch. In
Einstellungen › Sicherheit steht deshalb „Kern-Version“: Die Nummer gehört zum Kern, nicht zum
Funktionsumfang der Oberfläche.

## Funktionsstand 0.8.7

### EquatePlus (#720)

- **Mitarbeiter-Aktienprogramme über EquatePlus:** Anmeldung im Browserfenster (bestätigt mit
  EquateAccess oder der angebotenen Alternative), danach **„Programme und Bestände abrufen“**:
  Jedes Programm wird ein eigenes Depot mit verfügbaren und gesperrten Beständen. Bisher führte
  ein EquatePlus-Zugang unter Linux in den falschen Anmeldeablauf.
- **„Dokumente laden …“** lädt die PDFs der Bibliothek (Bestätigungen, Auszüge,
  Steuerbescheinigungen), nur noch fehlende. Sie liegen in **„Belege & PostBox“** (Suche:
  equateplus), beim Programm-Depot nur bei eindeutigem Programmnamen. Einzelne defekte Dokumente
  halten den Lauf nicht an; die Meldung nennt die häufigsten Gründe.
- **Abgewählte Konten werden ausgeblendet:** In der Kontenauswahl eines Zugangs archiviert das
  Abwählen das Konto (nicht mehr abgerufen, nicht in der Summe, Daten bleiben), und die
  Seitenleiste blendet archivierte Konten aus; das Archiv-Symbol zeigt sie wieder an. Im
  Bearbeiten-Fenster eines Zugangs speichern „Speichern“ und „Speichern & abrufen“ die
  Kontenauswahl mit; ein Abruf startet erst danach.
- Die Skripte im Browser sind die der Mac-App (`scripts/gen-equateplus-qml.py`, abgeglichen von
  `scripts/test-equateplus-linux.cjs`). Mit einem echten EquatePlus-Zugang noch nicht getestet.
### Automatischer Abruf (#546, #567)

- **Einstellungen › Allgemein › Abruf › „Automatischer Abruf“:** Aus (Standard), jede Stunde oder
  alle 2, 4, 6, 12 oder 24 Stunden. GiroCrab ruft die Kontostände im gewählten Abstand ab, solange
  die App läuft und der Tresor entsperrt ist; bei gesperrtem Tresor wird der Abruf nach dem
  Entsperren nachgeholt. Die Wahl bleibt über Neustarts erhalten.
- Abgerufen werden nur Zugänge mit gespeicherter PIN oder gültiger Freigabe, nie mit einem
  Anmeldedialog: Verlangt die Bank eine TAN oder neue Freigabe, wird der Abruf übersprungen und
  im Abruf-Protokoll als „Automatisch“ vermerkt. Fehler erscheinen nur als Hinweis, ein Hinweis
  zu neuen Buchungen nur, wenn sich etwas geändert hat. Die comdirect-PostBox bleibt außen vor.
- Wird ein automatischer Abruf übersprungen, geht „Alle abrufen“ erst weiter, wenn der Bankdialog
  beendet ist; bei derselben Bank lehnt diese sonst den nächsten Dialog ab.
- Nicht in der Demo. Die Menüleiste mit Kontoständen der Mac-App gibt es unter Linux nicht.

### FinTS-Produkt-ID nur noch optional (#842, #865, #789)

- Die eigene FinTS-Produktregistrierung wird beim Hinzufügen einer Bank, im Dialog „… einrichten“
  und in der Seitenleiste nie mehr als fehlend gemeldet und blockiert „Speichern & abrufen“ nicht.
- Fehlt beim Abruf trotzdem eine Registrierung, wechselt GiroCrab auf die eingebaute und ruft
  einmal neu ab. Lässt sich die Umstellung nicht speichern, erscheint deren Fehler, ohne neuen Abruf. Ohne eingebaute Registrierung erscheint „Diese Version enthält keine
  FinTS-Registrierung von GiroCrab. Tragen Sie Ihre eigene Registrierung ein.“ mit
  **Eigene Registrierung eintragen …**.
- **Eigene Registrierung eintragen …** stellt erst auf die eigene Registrierung um, wenn eine
  gespeichert ist, und setzt den Cursor in das Feld unter Einstellungen › Banken.

### Tags

- **Buchungen nach Tags filtern und auswerten:** In der Filterzeile über der Buchungsliste
  öffnet der Knopf **„Tags“** das Fenster „Nach Tags filtern“. Mehrere angekreuzte Tags zeigen
  nur Buchungen, die alle tragen; „Ohne Tag“ zeigt Buchungen ohne Tag. Je Tag stehen Anzahl und
  Summe für den Zeitraum und die übrigen Filter der Liste. Der Knopf nennt nur die Zahl der Tags,
  der Druck nennt den Filter in der Kopfzeile.
- **Tags verwalten:** Die Buchungszahl eines Tags zeigt beim Darüberfahren oder per Klick die
  **Buchungen nach Konten**; ein Klick auf ein Konto zeigt dessen Buchungen mit diesem Tag in der
  Liste.

### Privatmodus (#719)

- **Darstellung › Privatmodus** (Strg+Umschalt+H) und das **Auge in der Symbolleiste** blenden
  Beträge aus, wenn Sie die App jemandem zeigen: Salden, Buchungsbeträge, Summen, Anteile,
  Budgets, Kredittilgung, Depot und Vermögen, Achsenwerte und Ablesewerte der Diagramme.
- **Einstellungen › Allgemein:** Schalter „Privatmodus“ und „Im Privatmodus ausblenden“ mit
  **Beträge** oder **Beträge und persönliche Angaben** (dann auch Namen, Kontonummern und
  Verwendungszwecke). Die Wahl bleibt über Neustarts erhalten.
- Gilt im Hauptfenster, in den Sheets und in den Einstellungen; Tooltips mit Beträgen oder Namen
  zeigen dann „•••“. „Problem melden“ schwärzt wie bisher und zusätzlich alles, was der
  Privatmodus verdeckt.
- Startoption `--privacy[=all]` für Screenshots.

## Korrekturen (Kern 0.8.7)

- **„PostBox jetzt abrufen“** (comdirect, #838) ruft die PostBox jetzt wirklich ab; die Anforderung
  ging bisher vor jedem Abruf verloren.
- **Eigene FinTS-Produktregistrierung:** Das Feld in Einstellungen › Banken steht wie in der Mac-App
  ohne Rahmen rechtsbündig in der Zeile; nach dem Eintragen gilt „Eigene Registrierung“.

## Korrekturen (Kern 0.8.0)

- **Belegvorschau:** In „Belege & PostBox“ füllt das PDF die Breite der Vorschau und bleibt beim
  Wechsel zwischen Belegen sichtbar (vorher blieb sie ab dem zweiten Beleg leer).
- **Ausgeführt, aber nicht gespeichert (#808):** Führt die Bank einen Auftrag aus, den GiroCrab nicht
  speichern kann, warnt jetzt ein Hinweis wie in der Mac-App, statt Erfolg zu melden. Bei
  Lastschriften nennt er, dass die Mandate vor dem nächsten Einzug zu prüfen sind.
- **Bankzugang bearbeiten:** Uberspace, FFB, Ford Money und Opel Direktbank haben wieder das Feld für
  Passwort bzw. PIN samt „im Tresor speichern“; Easybank, Ford Money und Opel Direktbank bieten
  „Kontoauszüge abrufen“ an. Bei der Commerzbank mit Schlüsseldatei (RAH-10) fragt der Dialog nach
  dem „Passwort der Schlüsseldatei“, die Benutzerkennung ist fest.
- **Regeln, die nur Tags setzen** (in der Mac-App angelegt), zeigen „→ Nur Tags“ und ihre Tags statt
  einer leeren Kategorie; im Formular steht „Keine (nur Tags)“.

## Funktionsstand 0.7.5 (Kern 0.8.0)

Mit diesem Schritt kann die Linux-Oberfläche alles, was die Mac-App 0.7.5 kann – bis auf die
kleinen Lücken, die damals am Ende standen. Einige Funktionen sind schon aus neueren Mac-Versionen übernommen, weil
sie mit dem Übrigen eng zusammenhängen; sie sind mit *(Mac-App: erst nach 0.7.5)* markiert.

### Seitenleiste, Kategorien, Regeln

- **Seitenleiste mit Abschnitten:** „Neue Umsätze“ und „Vermögen“ stehen ganz oben. Darunter
  folgen einklappbare Abschnitte **Konten**, **Zahlungen** (Geplant, Lastschriften – mit Zählern),
  **Dokumente** und **Kategorien**; der Zustand bleibt über Neustarts erhalten. Alle Pfeile und
  Symbole liegen auf einer gemeinsamen Achse, Unterkategorien und Konten rücken einheitlich ein.
  *(Zähler in der Mac-App erst nach 0.7.5)*
- **Konto-Zeilen** kompakter: Zahl neuer Buchungen am Kontosymbol, Kontoinhaber als Tooltip,
  Saldo in der zweiten Zeile; archivierte Konten zeigen keinen Saldo mehr („Archiviert – wird
  nicht mehr abgerufen“).
- **„+“ unten** öffnet ein Menü: Neues Konto, Neue Kontengruppe, Neue Kategorie (unter der
  gewählten), Neue Kategoriengruppe.
- **Konto bearbeiten …** im Kontextmenü: Name, Kontoinhaber, Kontoart und eine **Konto-Notiz**,
  die unter dem Kontonamen erscheint. „… abrufen“ nennt das Konto statt des Bankzugangs.
- Einheitliche Dialoge zum Archivieren und Löschen von Konten und Kategorien (Name im Titel).
  Konten-Auswertungen erscheinen nicht mehr bei Depots.
- **„Ohne Kategorie“** lässt sich umbenennen (Rechtsklick › Umbenennen …, „Standardbezeichnung“
  stellt den Namen zurück); die Bezeichnung gilt auch in Titel, Buchungsliste und Aufteilen.
- **Kategorie duplizieren** samt Unterkategorien („… Kopie“).
- **Aufgeklappte Kategorien** werden gemerkt, getrennt für Demo und eigenen Tresor.
- **Regeln:** auf mehrere Konten einschränken (Konten als Chips, „Konto hinzufügen“),
  Reihenfolge per Drag & Drop oder Kontextmenü („An den Anfang“, „Nach oben“, „Nach unten“,
  „Ans Ende“), Filter über der Liste mit `*` und `?` (z. B. „deu*b“), Rang vor jeder Regel.
  Das Feld „Priorität“ entfällt wie in der Mac-App: Es zählt die Reihenfolge der Liste.
- Bankzugang bearbeiten: Easybank und Alte Leipziger speichern das Passwort, die Volkswagen Bank
  die PIN im Tresor.

### Buchungsliste und rechte Seitenleiste

- **Tags vergeben (#228):** Die rechte Seitenleiste hat eine Karte „Tags“ unter der Notiz: Chips
  mit ✕, ein Eingabefeld mit Vorschlägen (die ersten 8, „+n weitere“ klappt alle auf, beim Tippen
  alle passenden und ähnlichen) und einem Hinweis, was ↩ tut (neu anlegen, hinzufügen, schon
  gesetzt, ähnlicher Tag vorhanden, zu lang). Bei der Mehrfachauswahl sind Tags, die nur einige
  Buchungen tragen, gestrichelt; ein Klick gibt sie allen. Dazu das Untermenü „Tags“ im
  Kontextmenü und Buchung › „Tags bearbeiten“. In der Liste stehen bis zu zwei Tags an der
  Buchung, „+n“ zeigt beim Darüberfahren alle. Bearbeiten › Rückgängig (Strg+Z) nimmt eine
  Änderung genau für die geänderten Buchungen zurück. *(Mac-App: erst nach 0.7.5)*
- **Tags verwalten (#228):** Konten › „Tags verwalten …“ (Strg+Umschalt+T, auch „Verwalten …“
  in der Tag-Karte der rechten Seitenleiste) zeigt alle Tags mit der Zahl ihrer Buchungen.
  Umbenennen per Doppelklick, ↩ oder Rechtsklick (mit denselben Hinweisen wie im Eingabefeld),
  Farbe aus 8 Farben, „Keine Farbe“ oder „Eigene Farbe“, Reihenfolge per Ziehen oder „Sortieren“
  (nach Name, meiste oder wenigste Buchungen). Namen, Farben und Reihenfolge gelten erst beim
  Schließen. Ab zwei markierten Tags (Strg-Klick) „Zusammenführen …“ mit Wahl des Tags, der
  bleibt; Löschen (−, Entf, Rechtsklick) mit Rückfrage, die die Zahl der Buchungen nennt;
  „Ungenutzte entfernen …“; Suche über alle Tags, auch nach ähnlichen Schreibweisen; Strg+N legt
  einen Tag an, Esc hebt erst die Auswahl auf und schließt dann. *(Mac-App: erst nach 0.7.5)*
- **Buchungsdichte (#542):** Einstellungen › Allgemein › „Buchungsdichte“: Kompakt (eine Zeile
  je Buchung mit Datum, ohne Tageskarten; die aktive Buchung klappt mit allen Details auf, ein
  zweiter Klick klappt sie zu), Standard (mit kurzem Verwendungszweck unter dem Namen) oder
  Großzügig (alle Buchungen mit allen Details). *(Mac-App: erst nach 0.7.5)*
- **Verwendungszweck in der Zeile (#497):** Der bereinigte Zweck steht einzeilig in der Zeile
  (Standard: unter dem Namen, Kompakt: hinter dem Namen); wiederholt er nur Händler, Gegenpartei
  oder Zahlungsart, entfällt er.
- **Rechte Seitenleiste anpassbar (#448):** „Anpassen“ über den Karten schaltet in einen
  Bearbeiten-Modus: Karten per Ziehen umsortieren (blaue Einfügelinie) oder mit ↑ / ↓, mit ⊖
  ausblenden, unter „Ausgeblendet“ mit ⊕ wieder einblenden, „Zurücksetzen“ stellt die
  Standard-Reihenfolge wieder her.
- **Farbe negativer Beträge (#218, #571):** Einstellungen › Allgemein › „Negative Beträge“ wählt
  Neutral (wie bisher), Rot (wie negative Salden in der Seitenleiste) oder Kontraststark (orange
  und blau, auch bei Rot-Grün-Schwäche gut unterscheidbar). Das Vorzeichen bleibt immer sichtbar;
  Bestellkonto-Zeilen zeigen jetzt ebenfalls Minus bzw. Plus.
  *(„Kontraststark“ in der Mac-App erst nach 0.7.5)*
- **Händler-Liste (#495):** Die Überschrift „Händler-Historie“ öffnet alle Buchungen des Händlers
  im gewählten Zeitraum über alle Konten. Eine Leiste „Alle Buchungen von …“ führt mit „Zurück“
  zur vorherigen Ansicht samt Filtern zurück.
- **Kategorisieren per Kontextmenü und Drag & Drop (#366):** Das Kontextmenü einer Buchung hat
  das Untermenü „Kategorie“ mit dem Kategorienbaum, „Kategorisieren …“ und „Kategorie entfernen“
  (bei Mehrfachauswahl für alle markierten Buchungen). Buchungen lassen sich auf eine Kategorie
  ohne Unterkategorien in der Seitenleiste ziehen. *(Mac-App: erst nach 0.7.5)*
- **Wiederkehrende Buchungen:** Das Kontextmenü einer Serie in der Übersicht hat das Untermenü
  „Wiederkehrend“ mit dem Intervall der ganzen Serie (#581). *(Mac-App: erst nach 0.7.5)*
- **Kategorie je Konto ausblenden und Gegenkonto (#336, #523):** Im Kontomenü schaltet
  „Kategorie in der Buchungszeile anzeigen“ den Kategorie-Chip für dieses Konto ab. Überweisungen
  auf ein eigenes Konto oder einen Unterkonto-„Pocket“ nennen es in der Zeile („⇄ Name“) und in
  der Seitenleiste mit IBAN.

### Kopfbereich und Kategorie-Ansichten

- **Kopfbereich minimieren:** Ein Pfeil neben dem Titel (oder Darstellung › Kopfbereich
  minimieren) reduziert die Kopfkarte auf Titel, Saldo und Filterzeile; die Einstellung bleibt
  gespeichert.
- **Budgets unabhängig von „Auswertungen anzeigen“:** Die Budget-Karte folgt nur noch ihrem
  eigenen Schalter.
- **Kategorien zeigen ihre Monatssummen** als Balkendiagramm (mit Unterkategorien) statt des
  Kontoverlaufs.
- **Kategorie-Ansichten auf Konten eingrenzen:** Bei einer Kategorie oder „Ohne Kategorie“ hat
  die Filterzeile das Menü „Konten: Alle“.
- **Zeitraum je Kategorie:** Der zuletzt für eine Kategorie gewählte Zeitraum gilt beim nächsten
  Öffnen wieder.

### Saldoverlauf als Balken

- **Balken statt Linie:** Einstellungen › Allgemein › Saldoverlauf (oder Rechtsklick auf den
  Verlauf) zeigt den durchschnittlichen Saldo je Tag, Monat oder Jahr als Balken („Balken je“:
  automatisch, Tage, Monate, Jahre). Gestrichelte Linien markieren den Jahreswechsel; ein Klick
  springt zu den Buchungen des letzten Tags des Balkens. Der Verlauf hat jetzt wie am Mac eine
  schmale Achse mit Null, Höchst- und Tiefststand.

### Händler-Logos (#33)

- **Logos aus dem Internet:** Einstellungen › Allgemein › „Händler-Logos“ wählt den Anbieter (Aus,
  DuckDuckGo, Google) und leert den Logo-Cache. Standard bleibt „Aus“: Vorher wird nichts
  angefragt, auch keine früher zwischengespeicherten Logos angezeigt. Übertragen wird nur die
  Händler-Domain; die Favicons liegen verschlüsselt im Tresor.
- **Hinweis in der Buchungsliste:** Sind echte Buchungen zu sehen und Logos aus, bietet ein Hinweis
  einmalig „Mit DuckDuckGo laden“ oder „Einstellungen …“ an (nicht im Demo-Tresor).
- **Eigene Domain:** Im Beleg setzt der Stift neben der Domain eine eigene Domain für das Logo,
  „Automatische Erkennung wiederherstellen“ entfernt sie wieder.

### Überweisungen, Sammelüberweisung und Daueraufträge

- **Überweisungsformular neu geordnet** wie in der Mac-App: „Von Konto“ steht oben, darunter
  Empfänger, Zahlung und „Ausführung“. Die Felder stehen wie dort ohne Rahmen rechtsbündig in Zeilen mit
  Trennlinien; der Verwendungszweck ist eine Zeile, die bis drei Zeilen wächst. Normale Überweisung, Terminüberweisung und
  Echtzeitüberweisung sind eine Auswahl statt zweier Häkchen; die Prüfansicht zeigt Betrag,
  Empfänger und Zahlung im selben Stil.
- **Umbuchung zwischen eigenen Konten:** „Überweisung | Umbuchung“ im Formular; das eigene
  Zielkonto füllt Name und IBAN aus (#547). *(Mac-App: erst nach 0.7.5)*
- **Sammelüberweisung** (#240): „Einzeln | Sammelüberweisung“ unter dem Titel. Viele Zahlungen
  von einem Konto mit einer TAN, als Tabelle zum Bearbeiten, mit CSV-Import (Knopf oder Datei ins
  Fenster ziehen; UTF-8 oder Windows-1252, höchstens 2 MB), Hinweis auf mögliche Doppelte, Zeilen
  entfernen mit Entf und Strg+Z, eine Sammelbuchung oder Einzelbuchungen, Terminüberweisung.
- **Daueraufträge anlegen, ändern und löschen** per FinTS (#496), mit den Grenzen der Bank
  (Rhythmus, Ausführungstag, frühester Termin). Kontextmenü einer Buchung › „Als Dauerauftrag
  anlegen …“ übernimmt Empfänger, Betrag, Zweck (ohne Datum) und Rhythmus.
- **TAN abgebrochen:** Eine abgebrochene Sammelüberweisung oder ein abgebrochener Dauerauftrag
  öffnet wieder das eigene Formular mit Hinweis; die nächste Einzelüberweisung läuft wieder als
  Einzelüberweisung (#533).
- **GiroCode in Rechnungen:** Ein Bild (PNG, JPEG) oder PDF mit GiroCode (EPC-QR-Code), ins
  Überweisungsformular gezogen, füllt Empfänger, IBAN, BIC, Betrag und Zweck aus – wie eine
  ZUGFeRD-Rechnung, danach „Felder ausgefüllt – bitte prüfen“. In PDFs werden die ersten zehn
  Seiten durchsucht (Mac-App: nur die erste), kleine Codes auf den ersten beiden Seiten in
  höherer Auflösung; dafür braucht es Qts PDF-Bildmodul (`qt6-image-formats-plugin-pdf`, wie
  beim Drucken). Solange die Rechnung gelesen wird, zeigt das Formular einen Fortschrittskreis
  mit „Lese Rechnung …“.

### Geplant

- **„Geplant“** (#240) zeigt alle bei der Bank beauftragten, noch nicht gebuchten Zahlungen
  (Terminüberweisungen, Daueraufträge, eingereichte Lastschriften) in Monatskarten wie die
  Buchungsliste, mit Konto- und Zeitraumfilter (30 Tage, 3 Monate, 12 Monate, Alles) und den
  Summen abgehend und eingehend.
- Eigene rechte Seitenleiste für die gewählte Zahlung; ↑/↓ wählen, Esc hebt die Auswahl auf.
  Kontextmenü und Seitenleiste: Dauerauftrag ändern oder löschen, Konto anzeigen, IBAN kopieren.
  „Dauerauftrag anlegen …“ oben rechts.

### Lastschriften

- **„Lastschriften“** (#143): Mandate je Gläubigerkonto mit Gläubiger-ID (Vorschlag aus
  vorhandenen Daten), Mandat anlegen, bearbeiten (auch per Doppelklick), aktivieren,
  deaktivieren und löschen; ↑/↓ und Esc in der Liste.
- **Import und Export:** CSV-Mitgliederliste (UTF-8 oder Windows-1252) oder SEPA-Lastschriftdatei
  (pain.008) per Dateidialog oder Datei ins Fenster ziehen; Export der Mandate als CSV.
- **Einzug vorbereiten:** Betrag und Zweck je Mandat, „Letzter Einzug“ (FNAL), Fälligkeit; die
  pain.008-Dateien (je Art und Folgetyp eine) lassen sich sichern, als eingezogen markieren oder
  per FinTS bei der Bank einreichen – eine TAN je Datei, danach stehen die Einzüge unter „Geplant“.
- Rücklastschriften erscheinen in der Liste und in der rechten Seitenleiste („Als geprüft
  markieren“), dazu die Buchungen zum Mandat.

### Anlageklassen im Vermögen

- **Anlageklassen und Instrumente:** Wertpapiere und Sachwerte zeigen ihre Anlageklasse (Aktien,
  Anleihen, Immobilien …) und ihr Instrument (Einzeltitel, ETF, Fonds …) in der Positionszeile.
  „Klassifizierung bearbeiten“ (Kontextmenü, Stift in der Vermögensübersicht) setzt eine eigene
  Zuordnung; „Automatisch“ setzt das jeweilige Merkmal zurück. Offene Positionen lassen sich mit
  „Jetzt zuordnen“ nacheinander zuordnen.
- **Vermögen gruppieren:** Neben „Nach Konto“ gibt es „Nach Anlageklasse“ und im Tab Wertpapiere
  „Nach Instrument“. Ring und Legende filtern die Karten darunter (in jeder Ansicht);
  Nicht klassifizierte Werte lassen sich im Diagramm ausblenden, direkt gehaltene Immobilien aus der
  ganzen Ansicht („Nettovermögen ohne Immobilien“). Bei mehreren Kontoinhabern filtert eine Auswahl
  die gesamte Ansicht nach Inhaber.

### Tresor sichern und wiederherstellen

- **Einstellungen › Sicherheit › „Sicherung“:** „Tresor sichern …“ speichert den eigenen Tresor
  als Datei „GiroCrab-Sicherung <Datum>.girocrab-backup“, verschlüsselt mit einem Passwort Ihrer
  Wahl (mindestens 8 Zeichen, zweimal eingegeben). Die Sicherung lässt sich auf jedem Rechner
  öffnen, auch ohne den Schlüsselbund dieses Rechners – aber nicht ohne das Passwort.
- **„Passwort vorschlagen“** erzeugt ein starkes Passwort (drei Sechsergruppen, rund 90 Bit aus
  dem Zufallsgenerator des Kernels) und trägt es in beide Felder ein. „Kopieren“ legt es für
  60 Sekunden in die Zwischenablage, ohne Eintrag im Verlauf der Zwischenablage.
- **„Tresor wiederherstellen …“** ersetzt alle Daten des eigenen Tresors durch eine Sicherung und
  zeigt danach den eigenen Tresor, auch wenn vorher die Demo zu sehen war.
- **„Vorhandene Daten importieren“ › Datenbank** bietet jetzt auch die GiroCrab-Sicherung an
  (neben der Datenbank anderer Programme). Bei leerem Tresor ist sie vorgewählt; enthält der Tresor schon
  Konten, fragt „Tresor ersetzen?“ nach.

### Belege & PostBox

- **Filter oben in der Symbolleiste:** „Alle Dokumente“, „Belege“ (selbst hinzugefügte
  Rechnungen, Kassenbons, Fotos) oder „PostBox“ (Dokumente der Bank) – unabhängig davon, ob ein
  Dokument einem Umsatz zugeordnet ist. Überschrift und Erklärung folgen der Auswahl, darunter
  steht die Zahl der Dokumente.
- **„Beleg hinzufügen …“** wählt PDFs oder Fotos per Dateidialog und sucht den passenden Umsatz,
  wie beim Hineinziehen.
- Jede Zeile nennt Herkunft und Konto („Beleg · Girokonto“) und den zugeordneten Umsatz oder
  „Ohne zugeordneten Umsatz“. „Nur ohne zugeordneten Umsatz“ ersetzt „Nur ohne Buchung“.
- „Belege & PostBox“ beginnt wieder mit „Alle Konten“, wenn man den Bereich verlässt und später
  zurückkehrt. Findet ein Filter nichts, führt „Alle Dokumente anzeigen“ zurück.
- **„Zuordnung aufheben“** (vorher „Verknüpfung lösen“) ist bei Dokumenten ohne Umsatz
  ausgegraut und bestätigt sich mit einem Hinweis.
- **„Im Dateimanager anzeigen“** im Kontextmenü und im Beleg-Menü der Detailspalte öffnet den
  Dateimanager (Dolphin, Nautilus, …) mit der entschlüsselten Kopie ausgewählt; ohne
  passenden Dateimanager öffnet sich der Ordner.

### Commerzbank mit HBCI-Schlüsseldatei (RAH-10)

- **Bank hinzufügen › „Commerzbank · Schlüsseldatei“** (unter „Nicht gefunden?“) übernimmt eine
  bestehende HBCI-Schlüsseldatei mit Profil 10 als Kopie in den verschlüsselten Tresor; die
  Originaldatei bleibt unverändert. Zuerst die FinTS-Produktregistrierung, dann Datei,
  Anzeigename und Passwort; „Datei prüfen“ prüft Passwort und Datei ohne Netz.
- Fehlen die öffentlichen Bankschlüssel, ruft „Öffentliche Bankschlüssel abrufen“ sie ab. Sie
  werden erst übernommen, wenn für beide Schlüssel der SHA-256-Prüfwert aus der offiziellen
  Commerzbank-Quelle eingetragen ist und übereinstimmt.
- Danach startet der erste Abruf. Beim Abruf fragt GiroCrab nach dem „Passwort der
  Schlüsseldatei“ statt nach einer PIN; vorerst nur Konten, Salden und Umsätze.

### American Express im Portal-Fenster

- **Vorgemerkte Umsätze nie doppelt** (#629): Der Kern speichert sie als Vormerkungen und
  gleicht sie mit der späteren Buchung ab. Ist eine Vormerkung nicht mehr offen, ihre Buchung
  aber noch nicht gefunden, bleibt das Fenster offen und weist auf den vorherigen
  Abrechnungszeitraum hin. Beim Schließen fragt „Vorgemerkte Umsätze nicht gefunden“, ob sie
  verworfen werden („Weiter suchen“ / „Verwerfen und schließen“). *(Mac-App: erst nach 0.7.5)*
- **„Ältere Abrechnungen laden“** hält das Fenster offen, damit im Portal frühere
  Abrechnungszeiträume geöffnet werden können; „Umsätze übernehmen“ übernimmt dann alles
  Erkannte und schließt. Beim ersten Abruf eines Zugangs ist das schon eingeschaltet.
  *(Mac-App: erst nach 0.7.5)*

### Mitteilungen

- **Mitteilungen des GiroCrab-Projekts:** Neuigkeiten erscheinen als ruhiges Banner über der
  Buchungsliste, höchstens eines zugleich, nie während einer Sicherheitssperre. Das Banner lässt
  sich ausblenden (es kommt nicht wieder); „Mehr erfahren“ zeigt den ganzen Text. Alles, was gilt,
  bleibt unter Hilfe › Mitteilungen … lesbar. GiroCrab lädt dazu höchstens alle sechs Stunden eine
  signierte Textdatei von GitHub, ohne Kennung und ohne Tracking; Einstellungen › Allgemein ›
  Mitteilungen schaltet das samt Netzabruf ab.

### Widerrufen bei Darlehen und Startsaldo

- **„Alle Buchungen als gelesen markieren“** lässt sich mit Strg+Z zurücknehmen, genau für die
  Buchungen, die dabei markiert wurden.
- **Bearbeiten › Rückgängig / Wiederholen** (Strg+Z / Strg+Umschalt+Z) gilt jetzt auch für
  Darlehen und Startsaldo: „Darlehenszuordnung“ (Buchung › Darlehen zuordnen oder Zuordnung
  aufheben), „Darlehen ändern“ (auch „Stand neu setzen“), „Darlehen anlegen“ (Rückgängig
  entfernt das neue Darlehen wieder) sowie „Startsaldo ändern“ und „Startsaldo entfernen“.
- Wie in der Mac-App gibt es dafür einen gemeinsamen Verlauf mit den Tag-Änderungen: Rückgängig
  nimmt immer die letzte Änderung zurück, gleich welcher Art, und der Menüpunkt nennt sie. Beim
  Sperren, beim Wechsel zwischen Demo und eigenem Tresor und nach einer Wiederherstellung wird
  der Verlauf geleert.

### Einstellungen

- Unten im Einstellungsfenster steht dezent die Version (z. B. „GiroCrab 0.8.0“), auf jedem Reiter.
  Wie „Kern-Version“ in Sicherheit ist es die Nummer des Kerns, auf dem die Linux-Oberfläche
  gebaut ist.

## Funktionsstand 0.7.0 (Kern 0.7.5)

Mit diesem Schritt kann die Linux-Oberfläche alles, was die Mac-App 0.7.0 kann. Was aus
0.7.1 bis 0.7.5 damals noch fehlte, kam mit dem Funktionsstand 0.7.5.

### Seitenleiste: alle Konten im Blick

- **„Alle Konten“** steht jetzt ganz oben als Wurzel der Kontenliste. Die Zeile zeigt den
  Gesamtsaldo aller Konten, die zur Gesamtsumme zählen – bei mehreren Währungen eine Summe je
  Währung, untereinander, damit nichts abgeschnitten wird (Fremdwährungen mit Summe 0 entfallen).
  Ein Klick auf den Pfeil klappt den ganzen Kontenbaum weg, ein Klick auf die Zeile öffnet die
  Übersicht über alle Buchungen (#397).
- **Drei Ansichten** für die Konten: nach eigenen **Gruppen**, nach **Bank** oder nach
  **Kontoart**. Umschaltbar oben in der Seitenleiste, im Menü Darstellung › Konten anzeigen und
  in der Befehlspalette (Strg+K), die auch jede Bank direkt anspringt (#377).
- **Kontosuche:** Das Suchfeld über der Liste filtert nach Name, Kontoinhaber, Bank, IBAN oder
  Kontonummer (Leerzeichen in IBANs egal). Passt der Inhaber oder die IBAN, zeigt die Zeile genau
  das statt des Banknamens, damit der Treffer sofort erkennbar ist.
- **Konto-Zeilen** zeigen das Kürzel des Kontoinhabers und statt eines Punktes eine **Blase mit
  der Zahl neuer Buchungen** (#426).
- Neu im Kontextmenü eines Kontos: **„Kontoart ändern …“** (z. B. ein Girokonto, das eigentlich
  eine Kreditkarte ist) und **„Kontoinhaber …“** (#462, #280).
- **„Neue Umsätze“**: eine eigene Seite mit allen noch ungeprüften Buchungen, als eine Karte je
  Konto mit Kontostand und vorgemerkter Summe. Wer neue Buchungen abhaken will, muss nicht mehr
  Konto für Konto durchgehen (#352).
- **Kategorien** lassen sich als Ganzes einklappen, wenn man sie selten braucht (#408).
- Bedienung mit der **Tastatur**: ↑/↓ wechseln die Auswahl in der Seitenleiste, ←/→ klappen
  Kategorien mit Unterkategorien auf und zu.

### Gruppen, Farben, Kontoauswahl

- **„Gruppen bearbeiten …“** hat eine Suche über Gruppen, Konten, Kontoinhaber und IBANs mit
  Trefferzahl je Gruppe. Konten lassen sich per Ziehen auf eine Gruppe links legen (#377).
- **Farbwahl** für Konten, Gruppen und Kategorien: zusätzlich Gelb, Braun und Schiefer, dazu
  „Eigene Farben“ – alle Farben, die im Tresor schon benutzt werden, mit einem Klick wieder
  verwendbar und per Rechtsklick entfernbar (#466).
- Die Kontoauswahl in **Belege & PostBox** und im **Regel-Editor** ist gegliedert wie die
  Seitenleiste (nach Gruppe, Bank oder Kontoart) statt einer langen, flachen Liste (#412).

### Vermögen und Sachwerte

- Neue Seite **„Vermögen“**: Nettovermögen mit Vermögenswerten und Verbindlichkeiten (Kredite
  und Karten wahlweise abgezogen), die Zusammensetzung als Ring, der Cash-Verlauf über 3 Monate
  bis 5 Jahre, Wertpapiere mit Tagesveränderung und Ausschüttungen, Gebühren und Steuern.
  Umschaltbar zwischen Alles, Cash, Wertpapiere und Sachwerte (#348).
- **Sachwerte** wie Gold, Immobilien oder Fahrzeuge gibt es jetzt als Konto ohne Bankanbindung
  (Ablage › „Neues Sachwerte-Konto …“). Je Sachwert lassen sich Bewertungen und Mengen mit Datum
  erfassen und Verkäufe buchen; Goldbestände werden mit aktuellen Goldpreisen bewertet (#87, #345).

### Buchungen und Auswertungen

- **Export** der Umsätze als **PDF** (Kontoauszug-Layout, A4), **CSV** (für Tabellen,
  Semikolon, UTF-8) oder **CAMT.053** (ISO 20022, für andere Programme und Steuerberater) – für
  einen Zeitraum, ein oder mehrere Konten oder nur die markierten Buchungen. Ablage › „Umsätze
  exportieren …“, im Kontextmenü der Buchungen und in der Befehlspalette. CAMT.053 erstellt der
  Kern mit den echten Salden der Bank; Darlehen und Depots sind dafür gesperrt (#401, #372, #483).
- Das **Zeitraum-Menü** bietet rollierende Zeiträume (30 Tage, 90 Tage, 12 Monate), „Seit
  letztem Gehalt“, die letzten Monate, Quartale und Jahre beim Namen und einen eigenen Zeitraum
  (#382).
- **Budgets** lassen sich bearbeiten und entfernen; wer keine Budgets nutzt, blendet sie in
  Darstellung oder in den Einstellungen ganz aus (#430).
- **Splitbuchung:** Der Restbetrag wird angezeigt, „Rest zuweisen“ verteilt ihn, und ein Hinweis
  sagt, warum „Speichern“ noch ausgegraut ist (#383). Darlehen lassen sich nur ausgehenden
  Buchungen zuordnen.
- Eine eben geprüfte Buchung bleibt in der Liste stehen, bis man eine andere auswählt, statt
  unter dem Mauszeiger zu verschwinden (#470).
- Die **Händler-Historie** im Inspektor zeigt neben allen Konten auch die Zahlen für das eigene
  Konto (#299).
- Eine **Regel aus einer Einnahme** gilt nur für Einnahmen und kategorisiert nicht versehentlich
  auch die Überweisungen an denselben Empfänger (#295).
- **Amazon-Bestellungen** (Zugang „Amazon Bestellungen“): Buchungen zeigen die bestellten Artikel,
  „+N weitere“, Abo-Bestellungen und die Bestellnummer; ein Pfeil klappt alle Artikel auf. Im
  Inspektor sind die Artikel Links zur Produktseite, Erstattungen springen zur Bestellung
  (#333, #349, #354).

### Einstellungen, Hilfe und Darstellung

- **Schriftgröße** Standard, Groß oder Sehr groß für die ganze App – in Einstellungen ›
  Allgemein und mit Strg+Plus, Strg+Minus, Strg+0 (#343).
- **Erscheinungsbild** System, Hell oder Dunkel, unabhängig vom System (#388).
- **App zurücksetzen** (Einstellungen › Sicherheit): löscht nach einer Bestätigung Tresor,
  Schlüssel im Schlüsselbund und Einstellungen und startet GiroCrab neu wie beim ersten Start
  (#241).
- **Kontoauswahl beim Bankzugang:** Liefert ein neuer Bankzugang Konten, die ein anderer Zugang
  schon abruft, fragt GiroCrab, welche übernommen werden – doppelte Konten bleiben aus (#346).
- **Hilfe › Problem melden** (Strg+Alt+B): ein eigenes Fenster mit einem Screenshot des
  Hauptfensters, in dem Beträge, Namen, IBANs und andere persönliche Daten automatisch geschwärzt
  sind; weitere Stellen lassen sich von Hand schwärzen. Optional mit dem Abruf-Protokoll, ebenfalls
  bereinigt. Gesendet wird nichts automatisch: Ein Klick öffnet ein vorausgefülltes GitHub-Issue im
  Browser, das Bild liegt in der Zwischenablage (#301).
- Die **animierte Krabbe** der Mac-App begrüßt auf Startbild, Sperrbildschirm und
  Willkommenskarte. Unter Linux wirft sie beim Entsperren und Laden des Tresors die Münze; die
  Animation läuft im Render-Thread und steht deshalb auch beim Laden großer Tresore nicht still.

### PSD2-Banken (Enable Banking)

- **Revolut** und andere Banken, deren Freigabe ein zweites Fenster öffnet, funktionieren jetzt
  im GiroCrab-Anmeldefenster. Bisher wurde das zweite Fenster in das erste geladen und die Seite,
  die auf die Freigabe am Handy wartete, ging verloren („Autorisierung konnte nicht abgeschlossen
  werden“). Jetzt öffnet es sich als echtes Pop-up mit derselben Sitzung (#289).
- Das Anmeldefenster blieb weiß, stürzte nach einer erfolgreichen Freigabe ab und gab sich als
  „QtWebEngine“ zu erkennen, was manche Banken ablehnen – alles behoben.
- `girocrab --debug-consent` protokolliert den Anmeldeablauf im Terminal (nur Adressen und die
  Namen der Parameter, keine Codes oder Tokens) – für die Fehlersuche bei weiteren Banken.
- **Bank hinzufügen** bietet für PSD2 die heruntergeladene .pem-Schlüsseldatei an statt eines
  Textfelds; die Anleitung nennt auch, was in „Privacy URL“ und „Terms URL“ gehört.

### Weitere Korrekturen

- „Bearbeiten › Suchen“ setzt den Fokus ins Suchfeld; die Kontosuche verliert beim Tippen nicht
  mehr den Fokus.
- Bankdialoge, die aus den Einstellungen starten (z. B. „Neu verbinden“), erscheinen vorn statt
  hinter dem Einstellungsfenster.
- Scrollleisten sind im Dunkelmodus gut sichtbar; „Einstellungen › Allgemein“ ist groß genug.
- Namen in der Buchungsliste werden nicht mehr zu früh gekürzt („Tim Hoffma…“).
- Neue Regeln lassen sich wieder speichern (das Feld heißt im Kern jetzt `account_ids`); Regeln,
  die in der Mac-App auf mehrere Konten eingeschränkt wurden, zeigt Linux als solche an.
