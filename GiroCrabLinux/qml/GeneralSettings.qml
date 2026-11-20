import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

// SettingsView.swift › GeneralSettings.
ScrollView {
    ScrollBar.vertical: ScrollViewBar {}
    id: pane
    contentWidth: availableWidth
    /// LanguagePreference: { choice: "system" | "de" | "en", system, running }.
    property var language: AppModel.run(() => AppModel.core("language_state")) || ({ choice: "system", system: "de", running: "de" })
    /// AppLanguage.nativeName: language names in their own language, as in the system settings.
    function nativeName(code) { return ({ de: "Deutsch", en: "English" })[code] || code }
    readonly property string effectiveLanguage: language.choice === "system" ? language.system : language.choice
    ColumnLayout {
        width: pane.availableWidth - 40
        x: 20
        spacing: 18
        Item { implicitHeight: 2 }
        // LanguagePreference.swift › LanguageSettingsSection
        SettingsFormSection {
            footer: qsTr("„Systemsprache“ folgt der Sprache Ihrer Arbeitsumgebung (z. B. Systemeinstellungen › Region & Sprache).")
            SettingsFormRow {
                label: qsTr("Sprache")
                AppComboBox {
                    Layout.preferredWidth: 200 * Theme.textScale
                    model: [{ id: "system", title: qsTr("Systemsprache (%1)").arg(pane.nativeName(pane.language.system)) },
                            { id: "de", title: pane.nativeName("de") }, { id: "en", title: pane.nativeName("en") }]
                    textRole: "title"; valueRole: "id"
                    currentIndex: ["system", "de", "en"].indexOf(pane.language.choice)
                    onActivated: {
                        AppModel.run(() => AppModel.core("set_language_choice", { choice: currentValue }))
                        pane.language = AppModel.run(() => AppModel.core("language_state")) || pane.language
                    }
                }
            }
            SettingsFormRow {
                visible: pane.effectiveLanguage !== pane.language.running
                label: qsTr("Die neue Sprache wird nach einem Neustart verwendet.")
                Button {
                    text: qsTr("Neu starten")
                    onClicked: { AppModel.run(() => AppModel.core("request_relaunch")); Qt.quit() }
                }
            }
        }
        // SettingsView.swift › AnnouncementsSettingsSection: one switch, on by default (quiet banner only)
        SettingsFormSection {
            title: qsTr("Mitteilungen")
            CheckBox {
                text: qsTr("Mitteilungen des GiroCrab-Projekts anzeigen")
                checked: Announcements.isEnabled
                onToggled: Announcements.setEnabled(checked)
            }
            Text { Layout.fillWidth: true; wrapMode: Text.WordWrap; font.pixelSize: Theme.callout; color: Theme.secondary
                   text: qsTr("Gelegentliche Neuigkeiten, unabhängig von Updates. GiroCrab lädt dazu eine signierte Textdatei von GitHub, ohne Kennung und ohne Tracking. Mitteilungen erscheinen nie als Fenster und lassen sich jederzeit ausblenden. Sicherheitswarnungen sind davon unabhängig.") }
        }
        SettingsFormSection {
            title: qsTr("Ansicht")
            // Appearance.swift › AppAppearance (GitHub #388)
            SettingsFormRow {
                label: qsTr("Erscheinungsbild")
                AppComboBox {
                    Layout.preferredWidth: 200 * Theme.textScale
                    model: [{ id: "system", title: qsTr("System") }, { id: "light", title: qsTr("Hell") }, { id: "dark", title: qsTr("Dunkel") }]
                    textRole: "title"; valueRole: "id"
                    currentIndex: Math.max(0, ["system", "light", "dark"].indexOf(Theme.appearanceSettings.appearance))
                    onActivated: Theme.appearanceSettings.appearance = currentValue
                }
            }
            Text { Layout.fillWidth: true; wrapMode: Text.WordWrap; font.pixelSize: Theme.caption; color: Theme.secondary
                   text: qsTr("Hell oder Dunkel gilt nur für GiroCrab; „System“ folgt der Einstellung des Systems.") }
            // BookingDensity.swift (#542)
            SettingsFormRow {
                label: qsTr("Buchungsdichte")
                AppComboBox {
                    Layout.preferredWidth: 200 * Theme.textScale
                    model: BookingPrefs.densities
                    textRole: "title"; valueRole: "id"
                    currentIndex: BookingPrefs.densities.findIndex(d => d.id === BookingPrefs.density)
                    onActivated: BookingPrefs.setDensity(currentValue)
                }
            }
            Text { Layout.fillWidth: true; wrapMode: Text.WordWrap; font.pixelSize: Theme.caption; color: Theme.secondary
                   text: qsTr("Kompakt öffnet die aktive Buchung mit allen Details. Standard zeigt einen kurzen Verwendungszweck. Großzügig zeigt alle Buchungen ausführlich.") }
            // TextSize.swift (#343): Schriftgröße
            SettingsFormRow {
                label: qsTr("Schriftgröße")
                AppComboBox {
                    Layout.preferredWidth: 200 * Theme.textScale
                    model: Theme.textSizes
                    textRole: "title"; valueRole: "id"
                    currentIndex: Theme.textSizes.findIndex(s => s.id === Theme.textSize)
                    onActivated: Theme.setTextSize(currentValue)
                }
            }
            Text { Layout.fillWidth: true; wrapMode: Text.WordWrap; font.pixelSize: Theme.caption; color: Theme.secondary
                   text: qsTr("Vergrößert die Schrift in der ganzen App. Auch im Menü Darstellung mit %1 / %2 / %3.")
                             .arg(Fmt.keys("Strg+Plus")).arg(Fmt.keys("Strg+Minus")).arg(Fmt.keys("Strg+0")) }
            // SettingsView.swift › Toggle("Privatmodus") + Picker("Im Privatmodus ausblenden") (#719)
            CheckBox {
                text: qsTr("Privatmodus")
                checked: PrivacyMode.enabled
                onToggled: PrivacyMode.setEnabled(checked)
            }
            SettingsFormRow {
                label: qsTr("Im Privatmodus ausblenden")
                AppComboBox {
                    Layout.preferredWidth: 260 * Theme.textScale
                    model: PrivacyMode.scopes
                    textRole: "title"; valueRole: "id"
                    currentIndex: PrivacyMode.scopes.findIndex(s => s.id === PrivacyMode.scope)
                    onActivated: PrivacyMode.setScope(currentValue)
                }
            }
            Text { Layout.fillWidth: true; wrapMode: Text.WordWrap; font.pixelSize: Theme.caption; color: Theme.secondary
                   text: qsTr("Blendet Beträge, Namen, Kontonummern und Verwendungszwecke aus, wenn du die App jemandem zeigst. Auf Wunsch nur die Beträge.") }
            CheckBox {
                text: qsTr("Auswertungen in der Übersicht anzeigen")
                checked: AppModel.showStatistics
                onToggled: AppModel.showStatistics = checked
            }
            Text { Layout.fillWidth: true; wrapMode: Text.WordWrap; font.pixelSize: Theme.caption; color: Theme.secondary
                   text: qsTr("Blendet Saldoverlauf, Cashflow und Budgets über der Buchungsliste aus. Auch im Menü Ansicht umschaltbar.") }
            CheckBox {
                text: qsTr("Budgets anzeigen")
                checked: AppModel.showBudgets
                onToggled: AppModel.showBudgets = checked
            }
            Text { Layout.fillWidth: true; wrapMode: Text.WordWrap; font.pixelSize: Theme.caption; color: Theme.secondary
                   text: qsTr("Wer keine Monatsbudgets nutzt, blendet die Budget-Karte und „Monatsbudget festlegen …“ ganz aus. Auch im Menü Ansicht umschaltbar.") }
            // NegativeAmountStyle.swift (#218, #571)
            SettingsFormRow {
                label: qsTr("Negative Beträge")
                AppComboBox {
                    Layout.preferredWidth: 200 * Theme.textScale
                    model: BookingPrefs.negativeStyles
                    textRole: "title"; valueRole: "id"
                    currentIndex: BookingPrefs.negativeStyles.findIndex(s => s.id === BookingPrefs.negativeStyle)
                    onActivated: BookingPrefs.setNegativeStyle(currentValue)
                }
            }
            Text { Layout.fillWidth: true; wrapMode: Text.WordWrap; font.pixelSize: Theme.caption; color: Theme.secondary
                   text: qsTr("Farbe von Abbuchungen in der Buchungsliste: neutral wie der übrige Text, rot wie negative Salden in der Seitenleiste oder kontraststark (orange und blau, auch bei Rot-Grün-Schwäche gut unterscheidbar). Das Vorzeichen bleibt immer sichtbar.") }
        }
        SettingsFormSection {
            title: qsTr("Buchungen")
            SettingsFormRow {
                label: qsTr("Als geprüft markieren")
                AppComboBox {
                    Layout.preferredWidth: 200 * Theme.textScale
                    model: AppModel.autoReviewDelays
                    textRole: "title"; valueRole: "id"
                    currentIndex: AppModel.autoReviewDelays.findIndex(d => d.id === AppModel.autoReviewDelay)
                    onActivated: AppModel.autoReviewDelay = currentValue
                }
            }
            Text { Layout.fillWidth: true; wrapMode: Text.WordWrap; font.pixelSize: Theme.caption; color: Theme.secondary
                   text: AppModel.autoReviewDelay === "account"
                         ? qsTr("Angeklickte Umsätze werden sofort als gelesen markiert. Beim Wechsel auf ein anderes Konto werden alle übrigen ungelesenen Umsätze des vorherigen Kontos als gelesen markiert, unabhängig von Zeitraum und Filtern.")
                         : qsTr("Eine ausgewählte Buchung gilt nach dieser Zeit als geprüft – wie eine gelesene E-Mail. Bei Mehrfachauswahl nie.") }
        }
        // SettingsView.swift › TransferSettingsSection: opt-in clipboard check (#663), per device
        SettingsFormSection {
            title: qsTr("Überweisungen")
            CheckBox {
                text: qsTr("Zwischenablage beim Öffnen einer Überweisung prüfen")
                checked: AppModel.clipboardCheck
                onToggled: AppModel.clipboardCheck = checked
            }
            Text { Layout.fillWidth: true; wrapMode: Text.WordWrap; font.pixelSize: Theme.caption; color: Theme.secondary
                   text: qsTr("Nur bei leerem Formular. Der Inhalt wird nur auf diesem Gerät gelesen, nie gespeichert oder protokolliert. Felder werden erst nach Bestätigung im Hinweis ausgefüllt.") }
        }
        // SettingsView.swift › BalanceChartSettingsSection (GitHub #203)
        SettingsFormSection {
            title: qsTr("Saldoverlauf")
            SettingsFormRow {
                label: qsTr("Darstellung")
                AppComboBox {
                    Layout.preferredWidth: 200 * Theme.textScale
                    model: BalanceChartPrefs.styles
                    textRole: "title"; valueRole: "id"
                    currentIndex: BalanceChartPrefs.styles.findIndex(s => s.id === BalanceChartPrefs.style)
                    onActivated: BalanceChartPrefs.setStyle(currentValue)
                }
            }
            SettingsFormRow {
                visible: BalanceChartPrefs.style === "bars"
                label: qsTr("Balken je")
                AppComboBox {
                    Layout.preferredWidth: 200 * Theme.textScale
                    model: BalanceChartPrefs.units
                    textRole: "title"; valueRole: "id"
                    currentIndex: BalanceChartPrefs.units.findIndex(u => u.id === BalanceChartPrefs.barUnit)
                    onActivated: BalanceChartPrefs.setBarUnit(currentValue)
                }
            }
            Text { Layout.fillWidth: true; wrapMode: Text.WordWrap; font.pixelSize: Theme.caption; color: Theme.secondary
                   text: qsTr("Balken zeigen den durchschnittlichen Saldo je Monat (bei kurzen Zeiträumen je Tag, ab sechs Jahren je Jahr) und machen Trends und Jahre vergleichbar. Gestrichelte Linien markieren den Jahreswechsel. Auch per Rechtsklick auf den Saldoverlauf umschaltbar.") }
        }
        // SettingsView.swift › MerchantLogoSettingsSection (GitHub #33); needs an open vault
        SettingsFormSection {
            title: qsTr("Händler-Logos")
            enabled: AppModel.isUnlocked
            footer: qsTr("Ohne Anbieter zeigt GiroCrab mitgelieferte Markenzeichen, sonst Monogramme und Kategorie-Symbole. Mit Anbieter werden Favicons geladen und verschlüsselt im Tresor zwischengespeichert. Übertragen wird nur die Händler-Domain (z. B. lidl.de) – keine Beträge, keine Kontodaten.")
            SettingsFormRow {
                label: qsTr("Logos aus dem Internet laden")
                AppComboBox {
                    Layout.preferredWidth: 200 * Theme.textScale
                    model: [{ id: "off", title: qsTr("Aus") }, { id: "duckduckgo", title: qsTr("DuckDuckGo") },
                            { id: "google", title: qsTr("Google") }]
                    textRole: "title"; valueRole: "id"
                    currentIndex: Math.max(0, ["off", "duckduckgo", "google"].indexOf(MerchantLogos.provider))
                    onActivated: MerchantLogos.setProvider(currentValue)
                }
            }
            Button {
                text: qsTr("Logo-Cache leeren")
                onClicked: MerchantLogos.clearLogoCache()
            }
        }
        // AutomationSettings.swift › AutoSyncSettingsSection (#546): kept outside the vault (QSettings
        // `[autoSync] hours`), so it is active again after a relaunch. Logic: AutoSync.qml
        SettingsFormSection {
            title: qsTr("Abruf")
            SettingsFormRow {
                label: qsTr("Automatischer Abruf")
                AppComboBox {
                    readonly property int current: AutoSync.selection
                    // a value set elsewhere stays selectable
                    readonly property var options: AutoSync.hourOptions.indexOf(current) >= 0
                        ? AutoSync.hourOptions : AutoSync.hourOptions.concat([current])
                    Layout.preferredWidth: 200 * Theme.textScale
                    model: options.map(h => ({ id: h, title: AutoSync.label(h) }))
                    textRole: "title"; valueRole: "id"
                    currentIndex: options.indexOf(current)
                    onActivated: AutoSync.setHours(currentValue)
                }
            }
            Text { Layout.fillWidth: true; wrapMode: Text.WordWrap; font.pixelSize: Theme.caption; color: Theme.secondary
                   text: qsTr("Ruft die Kontostände im gewählten Abstand ab, solange GiroCrab läuft und der Tresor entsperrt ist. Bei gesperrtem Tresor wird der Abruf übersprungen und nach dem Entsperren nachgeholt. Es werden nur Zugänge mit gespeicherter PIN oder gültiger Freigabe abgerufen, nie mit einem Anmeldedialog.") }
        }
        Item { implicitHeight: 8 }
    }
}
