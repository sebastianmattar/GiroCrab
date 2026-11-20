import QtQuick
import QtQuick.Controls
import QtQuick.Layouts
import QtQuick.Dialogs
import QtCore

// RootView.swift: GiroCrabScenes (window + menu commands), GiroCrabRootView
// (vault state) and MainSplitView (sidebar | booking column | inspector).
ApplicationWindow {
    id: window
    // `--dark` / `--light`: the controls follow the forced appearance (Theme.forcedPalette)
    Binding on palette { when: Theme.forced !== ""; value: Theme.forcedPalette }
    // appTextSize: controls without their own size follow the text size (#343)
    font.pointSize: Theme.controlPointSize
    visible: true
    // Fits a 13" MacBook Air with room for the Dock – same default here.
    width: 1280; height: 820
    // The booking list gives up width first (560 → 360) so the inspector never leaves the
    // window on small screens (e.g. 1920 px at 200 % scaling).
    minimumWidth: 240 * Theme.textScale + 360 + 280; minimumHeight: 480
    // BookingListView.swift › WindowTitle: an account or category name stays out of the title
    // while privacy mode (or the report) hides personal data or labels
    title: AppModel.isUnlocked && (AppModel.effectiveRedaction & (AppModel.redactPersonal | AppModel.redactLabels)) === 0
           ? AppModel.title + " – GiroCrab" : "GiroCrab"
    color: Theme.canvas

    // LanguagePreference.startupQuestion (RootView on the Mac): decided once at start-up.
    readonly property string languageQuestion: (AppModel.run(() => AppModel.core("language_state")) || { question: "none" }).question
    property bool languagePromptDone: false
    readonly property bool showLanguagePrompt: languageQuestion === "choose" && !languagePromptDone
    Component.onCompleted: {
        if (showLanguagePrompt) return   // the vault opens after the language screen
        Qt.callLater(AppModel.bootstrap)
        if (languageQuestion.startsWith("changed:")) Qt.callLater(systemLanguageChanged.open)
    }
    /// Bearbeiten › Suchen …: after the menu has closed (it hands the focus back to the
    /// item that had it before), so the search field keeps it.
    function focusSearch() {
        window.requestActivate()
        mainToolbar.focusSearch()
    }

    menuBar: MenuBar {
        font: Qt.application.font   // menus keep the system size (#343)
        // Same look as the menus (AppMenu): calm bar, rounded highlight for the open menu.
        background: Rectangle { color: Theme.canvas; Rectangle { anchors.bottom: parent.bottom; width: parent.width; height: 1; color: Theme.border } }
        delegate: MenuBarItem {
            id: mbi
            topPadding: 4; bottomPadding: 4; leftPadding: 9; rightPadding: 9
            background: Rectangle {
                anchors.fill: parent; anchors.topMargin: 2; anchors.bottomMargin: 2
                radius: 5
                color: mbi.highlighted || mbi.menu && mbi.menu.visible ? Theme.selection : "transparent"
            }
        }
        AppMenu {
            title: qsTr("&Ablage")
            Action { text: qsTr("Neue Überweisung …"); shortcut: "Ctrl+N"; enabled: AppModel.canTransfer && !TagManager.shown; onTriggered: AppModel.openTransfer() }
            Action { text: qsTr("Neue Kontogruppe …"); shortcut: "Ctrl+Shift+N"; enabled: AppModel.isUnlocked
                     onTriggered: AppModel.groupDialog = { kind: "create", movingAccount: null, parent: null } }
            Action { text: qsTr("Bank hinzufügen …"); enabled: AppModel.isUnlocked; onTriggered: AppModel.showAddBank = true }
            Action { text: qsTr("Neues Darlehen …"); enabled: AppModel.isUnlocked; onTriggered: AppModel.openLoanEditor("create") }
            Action { text: qsTr("Neues Sachwerte-Konto …"); enabled: AppModel.isUnlocked; onTriggered: AppModel.createTangibleAccount() }
            Action { text: qsTr("Neue Hauptkategorie …"); enabled: AppModel.isUnlocked; onTriggered: AppModel.newCategoryRequest = { parent: null } }
            AppMenuSeparator {}
            Action { text: qsTr("Vorhandene Daten importieren …"); enabled: AppModel.isUnlocked; onTriggered: AppModel.openMoneyMoneyImport() }
            Action { text: qsTr("Umsätze exportieren …"); enabled: AppModel.isUnlocked; onTriggered: AppModel.openBookingExport(false) }
            Action { text: qsTr("Umsätze als CSV exportieren …"); enabled: AppModel.isUnlocked; onTriggered: AppModel.exportFilteredBookingsCSV() }
            AppMenuSeparator {}
            Action { text: qsTr("Drucken …"); shortcut: "Ctrl+P"; enabled: AppModel.isUnlocked; onTriggered: AppModel.showPrintSheet = true }
            AppMenuSeparator {}
            Action { text: qsTr("Einstellungen …"); shortcut: "Ctrl+,"; onTriggered: AppModel.openSettings() }
            AppMenuSeparator {}
            Action { text: qsTr("Tresor sperren"); shortcut: "Ctrl+Meta+L"; enabled: AppModel.hasOwnVault; onTriggered: AppModel.lock() }
            Action { text: qsTr("Beenden"); shortcut: StandardKey.Quit; onTriggered: Qt.quit() }
        }
        AppMenu {
            title: qsTr("&Bearbeiten")
            // The window's undo manager (ManualLoansUI.swift › windowUndoManager): tags, loans
            // and the Startsaldo register on it; text fields keep their own Strg+Z
            Action { text: UndoManager.undoName ? qsTr("Rückgängig: %1").arg(UndoManager.undoName) : qsTr("Rückgängig")
                     shortcut: StandardKey.Undo; enabled: UndoManager.canUndo; onTriggered: UndoManager.undo() }
            Action { text: UndoManager.redoName ? qsTr("Wiederholen: %1").arg(UndoManager.redoName) : qsTr("Wiederholen")
                     shortcut: StandardKey.Redo; enabled: UndoManager.canRedo; onTriggered: UndoManager.redo() }
            AppMenuSeparator {}
            Action { id: findAction; text: qsTr("Suchen …"); shortcut: StandardKey.Find; enabled: AppModel.isUnlocked && !TagManager.shown; onTriggered: Qt.callLater(window.focusSearch) }
            Action { text: qsTr("Alles auswählen"); shortcut: StandardKey.SelectAll
                     enabled: AppModel.isUnlocked && AppModel.sidebarSelection !== "documents"; onTriggered: AppModel.selectAllBookings() }
        }
        AppMenu {
            title: qsTr("&Darstellung")
            Action { text: AppModel.showInspector ? qsTr("Informationen ausblenden") : qsTr("Informationen einblenden")
                     shortcut: "Ctrl+Alt+I"; enabled: AppModel.isUnlocked; onTriggered: AppModel.showInspector = !AppModel.showInspector }
            Action { text: qsTr("Auswertungen anzeigen"); checkable: true; checked: AppModel.showStatistics; onToggled: AppModel.showStatistics = checked }
            Action { text: qsTr("Budgets anzeigen"); checkable: true; checked: AppModel.showBudgets; enabled: AppModel.isUnlocked; onToggled: AppModel.showBudgets = checked }
            // RootView.swift › Toggle("Kopfbereich minimieren") (#358)
            Action { text: qsTr("Kopfbereich minimieren"); checkable: true; checked: AppModel.compactHeader; enabled: AppModel.isUnlocked; onToggled: AppModel.compactHeader = checked }
            // RootView.swift › Toggle("Privatmodus") ⇧⌘H (#719): hides amounts (or all personal data, Einstellungen › Allgemein)
            Action { text: qsTr("Privatmodus"); shortcut: "Ctrl+Shift+H"; checkable: true; checked: PrivacyMode.enabled; onToggled: PrivacyMode.setEnabled(checked) }
            AppMenuSeparator {}
            // Like Mail/Notes: text size of the whole app (#343).
            Action { text: qsTr("Größer"); shortcut: "Ctrl++"; enabled: Theme.steppedTextSize(1) !== ""
                     onTriggered: Theme.setTextSize(Theme.steppedTextSize(1)) }
            Action { text: qsTr("Kleiner"); shortcut: "Ctrl+-"; enabled: Theme.steppedTextSize(-1) !== ""
                     onTriggered: Theme.setTextSize(Theme.steppedTextSize(-1)) }
            Action { text: qsTr("Originalgröße"); shortcut: "Ctrl+0"; enabled: Theme.textSize !== "standard"
                     onTriggered: Theme.setTextSize("standard") }
            AppMenuSeparator {}
            Action { text: qsTr("Befehlspalette …"); shortcut: "Ctrl+K"; enabled: AppModel.isUnlocked; onTriggered: AppModel.showCommandPalette = true }
            AppMenuSeparator {}
            // Picker("Konten anzeigen"): a submenu with the three AccountSidebarMode titles
            AppMenu {
                title: qsTr("Konten anzeigen")
                enabled: AppModel.isUnlocked
                Action { text: qsTr("Nach Gruppe"); checkable: true; checked: AppModel.accountView === "byGroup"; onTriggered: AppModel.setAccountView("byGroup") }
                Action { text: qsTr("Nach Bank"); checkable: true; checked: AppModel.accountView === "byBank"; onTriggered: AppModel.setAccountView("byBank") }
                Action { text: qsTr("Nach Kontoart"); checkable: true; checked: AppModel.accountView === "byKind"; onTriggered: AppModel.setAccountView("byKind") }
            }
            Action { text: qsTr("Kontogruppen bearbeiten …"); enabled: AppModel.isUnlocked; onTriggered: AppModel.openGroupManager() }
            AppMenuSeparator {}
            Action { text: AppModel.demoActive ? qsTr("Eigenen Tresor anzeigen") : qsTr("Demo-Daten anzeigen"); shortcut: "Ctrl+Shift+D"
                     enabled: AppModel.isUnlocked
                     onTriggered: AppModel.demoActive ? AppModel.showOwnVault() : AppModel.showDemo() }
        }
        AppMenu {
            title: qsTr("&Konten")
            Action { text: qsTr("Alle Konten abrufen"); shortcut: "Ctrl+R"; enabled: AppModel.isUnlocked && AppModel.syncingConnectionID === null && !AppModel.bankingBlocksAll
                     onTriggered: AppModel.requestSyncAll() }
            AppMenuSeparator {}
            Action { text: qsTr("Regeln …"); shortcut: "Ctrl+Alt+K"; enabled: AppModel.isUnlocked; onTriggered: AppModel.openRuleEditor(null) }
            // TagManagerView.swift (#228): while the sheet is up it takes Strg+N („Neuer Tag …“)
            Action { text: qsTr("Tags verwalten …"); shortcut: "Ctrl+Shift+T"; enabled: AppModel.isUnlocked; onTriggered: TagManager.open() }
            Action { text: qsTr("Wiederkehrende Buchungen …"); enabled: AppModel.isUnlocked; onTriggered: AppModel.showRecurringSheet = true }
        }
        AppMenu {
            title: qsTr("B&uchung")
            // In the list, the single keys C and R do the same (kept for speed).
            Action { text: qsTr("Kategorisieren …"); shortcut: "Ctrl+Meta+C"; enabled: AppModel.selectedBooking !== null
                     onTriggered: AppModel.showCategorizer = true }
            Action { text: qsTr("Regel für diesen Händler erstellen …"); shortcut: "Ctrl+Meta+R"; enabled: AppModel.selectedBooking !== null
                     onTriggered: AppModel.openRuleEditor(AppModel.selectedBooking) }
            Action { text: qsTr("Geprüft"); shortcut: "Ctrl+Shift+U"; checkable: true
                     enabled: AppModel.selectedBooking !== null
                     checked: AppModel.selectedBooking ? AppModel.selectedBooking.checked : false
                     onTriggered: AppModel.setChecked(!AppModel.selectedBooking.checked, AppModel.selectedBooking) }
            Action { text: qsTr("Alle Buchungen als gelesen markieren"); shortcut: "Ctrl+Shift+Alt+U"; enabled: AppModel.canMarkAllChecked
                     onTriggered: AppModel.markAllChecked() }
            Action { text: qsTr("Notiz bearbeiten"); enabled: AppModel.selectedBooking !== null; onTriggered: AppModel.editNote(AppModel.selectedBooking) }
            Action { text: qsTr("Tags bearbeiten"); enabled: AppModel.selectedBooking !== null; onTriggered: TagsModel.editTags(AppModel.selectedBooking) }
            // like the Finder (Strg+Y); Space in the list does the same
            Action { text: qsTr("Beleg in Quick Look anzeigen"); shortcut: "Ctrl+Y"
                     enabled: AppModel.selectedBooking !== null && AppModel.documents(AppModel.selectedBooking).length > 0
                     onTriggered: AppModel.quickLookFirstDocument(AppModel.selectedBooking) }
            AppMenuSeparator {}
            // RecurringMenu
            AppMenu {
                id: recurringMenu
                title: qsTr("Wiederkehrend")
                enabled: AppModel.selectedBooking !== null
                readonly property var series: AppModel.selectedBooking ? AppModel.recurringSeries(AppModel.selectedBooking) : null
                Action { text: AppModel.intervalLabels.Monthly; checkable: true; checked: recurringMenu.series !== null && recurringMenu.series.interval === "Monthly"
                         onTriggered: AppModel.markRecurring(AppModel.selectedBooking, "Monthly") }
                Action { text: AppModel.intervalLabels.Quarterly; checkable: true; checked: recurringMenu.series !== null && recurringMenu.series.interval === "Quarterly"
                         onTriggered: AppModel.markRecurring(AppModel.selectedBooking, "Quarterly") }
                Action { text: AppModel.intervalLabels.HalfYearly; checkable: true; checked: recurringMenu.series !== null && recurringMenu.series.interval === "HalfYearly"
                         onTriggered: AppModel.markRecurring(AppModel.selectedBooking, "HalfYearly") }
                Action { text: AppModel.intervalLabels.Yearly; checkable: true; checked: recurringMenu.series !== null && recurringMenu.series.interval === "Yearly"
                         onTriggered: AppModel.markRecurring(AppModel.selectedBooking, "Yearly") }
                Action { text: qsTr("Individuell …"); checkable: true; checked: recurringMenu.series !== null && recurringMenu.series.interval === "Custom"
                         onTriggered: { checked = Qt.binding(() => recurringMenu.series !== null && recurringMenu.series.interval === "Custom"); AppModel.customRecurringBooking = AppModel.selectedBooking } }
                AppMenuSeparator {}
                Action { text: qsTr("Automatisch erkennen"); enabled: recurringMenu.series !== null && recurringMenu.series.source === "Manual"
                         onTriggered: AppModel.resetRecurring(recurringMenu.series.series_key) }
                Action { text: qsTr("Nicht wiederkehrend"); enabled: recurringMenu.series !== null; onTriggered: AppModel.ignoreRecurring(recurringMenu.series) }
            }
            Action { text: qsTr("Als Gehalt / Rente zählen"); checkable: true
                     enabled: AppModel.selectedBooking !== null && AppModel.selectedBooking.amount_cents > 0
                     checked: AppModel.selectedBooking ? AppModel.selectedBooking.is_salary : false
                     onTriggered: { const on = !AppModel.selectedBooking.is_salary; checked = Qt.binding(() => AppModel.selectedBooking ? AppModel.selectedBooking.is_salary : false)
                                    AppModel.setSalaryFlag(AppModel.selectedBooking, on) } }
            AppMenuSeparator {}
            Action { text: qsTr("Nächste Buchung"); shortcut: "Ctrl+Meta+Down"; onTriggered: AppModel.moveSelection(1) }
            Action { text: qsTr("Vorherige Buchung"); shortcut: "Ctrl+Meta+Up"; onTriggered: AppModel.moveSelection(-1) }
            AppMenuSeparator {}
            Action { text: qsTr("Alle Buchungen"); shortcut: "Ctrl+1"; onTriggered: AppModel.scope = "All" }
            Action { text: qsTr("Alltag && Konsum"); shortcut: "Ctrl+2"; onTriggered: AppModel.scope = "Everyday" }
            Action { text: qsTr("Sparpläne && Perks"); shortcut: "Ctrl+3"; onTriggered: AppModel.scope = "Investments" }
            // MARK: manual loans – LoanAssignMenu(model:booking: model.selectedBooking)
            AppMenuSeparator {}
            LoanAssignMenu { booking: AppModel.selectedBooking }
        }
        AppMenu {
            title: qsTr("&Fenster")
            Action { text: qsTr("Abruf-Protokoll"); enabled: AppModel.isUnlocked; onTriggered: AppModel.showSyncLog = true }
        }
        AppMenu {
            title: qsTr("&Hilfe")
            Action { text: qsTr("GiroCrab-Hilfe"); shortcut: "Ctrl+?"; onTriggered: AppModel.showHelp = true }
            AppMenuSeparator {}
            // Hilfe › Mitteilungen …: ellipsis because a window opens (AnnouncementsWindow.qml)
            Action { text: qsTr("Mitteilungen …"); onTriggered: Announcements.showWindow = true }
            AppMenuSeparator {}
            // #301: redacted screenshot + scrubbed diagnostics, then a prefilled issue.
            Action { text: qsTr("Problem melden …"); shortcut: "Ctrl+Alt+B"; onTriggered: AppModel.showBugReport = true }
            AppMenuSeparator {}
            Action { text: qsTr("Webseite && Dokumentation (girocrab.de)"); onTriggered: Qt.openUrlExternally("https://girocrab.de") }
            Action { text: qsTr("Fehler melden / Issue erstellen (GitHub)"); onTriggered: Qt.openUrlExternally("https://github.com/GiroCrab/GiroCrab/issues") }
            Action { text: qsTr("Community && Diskussionen (GitHub)"); onTriggered: Qt.openUrlExternally("https://github.com/GiroCrab/GiroCrab/discussions") }
        }
    }

    // `--demo-menu <title>` (e.g. "Ablage", "Buchung"): pops up that menu of the menu bar
    // over the window, for screenshots of the menus (Screenshot.swift › captureMenu).
    Timer {
        running: AppModel.launchValue("--demo-menu") !== "" && AppModel.vaultState === "open"
        interval: 700
        onTriggered: {
            const wanted = AppModel.launchValue("--demo-menu")
            for (let i = 0; i < window.menuBar.count; i++) {
                const item = window.menuBar.itemAt(i)
                if (item && item.menu && item.menu.title.replace(/&/g, "").toLowerCase() === wanted) {
                    item.menu.popup(item, 0, item.height)
                    return
                }
            }
            console.warn("--demo-menu: no menu", wanted)
        }
    }

    property real sidebarWidth: 260

    ColumnLayout {
    id: rootLayout
    anchors.fill: parent
    spacing: 0
    MainToolbar { id: mainToolbar; Layout.fillWidth: true }
    // GiroCrabRootView: vault state
    StackLayout {
        Layout.fillWidth: true
        Layout.fillHeight: true
        currentIndex: AppModel.vaultState === "open" ? 2 : AppModel.vaultState === "starting" ? 0 : 1

        // splash (starting)
        Rectangle {
            color: Theme.canvas
            // RootView.swift › StartLogo: square #263040 backdrop, crab on top, spinner 96 below the centre
            Rectangle {
                anchors.centerIn: parent
                width: 120; height: 120
                color: "#263040"
                AnimatedCrabLogo { size: 120; showBackgroundSquircle: false; busy: true }
            }
            BusyIndicator { anchors.centerIn: parent; anchors.verticalCenterOffset: 96; running: true }
        }

        // LockView / LockView(error:)
        LockView { error: AppModel.vaultState === "failed" ? AppModel.failure : "" }

        // MainSplitView
        SplitView {
            orientation: Qt.Horizontal
            handle: Rectangle {
                implicitWidth: 1
                color: Theme.border
            }
            SidebarView {
                id: sidebarPane
                SplitView.minimumWidth: 240 * Theme.textScale; SplitView.preferredWidth: 260 * Theme.textScale; SplitView.maximumWidth: 480 * Theme.textScale
                onWidthChanged: window.sidebarWidth = width
            }
            // bookingColumn: Belege, Vermögen, Geplant or Lastschriften replace the booking list
            StackLayout {
                SplitView.minimumWidth: Math.max(360, Math.min(560, window.width - sidebarPane.width - (AppModel.showInspector ? 280 : 0) - 2))
                SplitView.fillWidth: true
                currentIndex: ({ documents: 1, wealth: 2, planned: 3, directDebits: 4 })[AppModel.sidebarSelection] ?? 0
                BookingStreamView {}
                DocumentsView {}
                // built only while shown (it loads the depot positions and the cash history)
                Loader { active: AppModel.sidebarSelection === "wealth"; sourceComponent: Component { WealthView {} } }
                Loader { active: AppModel.sidebarSelection === "planned"; sourceComponent: Component { PlannedPaymentsView {} } }
                Loader { active: AppModel.sidebarSelection === "directDebits"; sourceComponent: Component { DirectDebitsView {} } }
            }
            InspectorView {
                visible: AppModel.showInspector
                SplitView.minimumWidth: 280; SplitView.preferredWidth: 320; SplitView.maximumWidth: 380
            }
        }
    }

    }

    // MainToolbar (macOS): scope picker in the centre, then transfer, sync, inspector, search.
    component MainToolbar: ToolBar {
        visible: AppModel.isUnlocked
        // the search field is an id inside this component: Main.focusSearch() reaches it here
        function focusSearch() { search.forceActiveFocus(Qt.ShortcutFocusReason); search.selectAll() }
        background: Rectangle { color: Theme.canvas; Rectangle { anchors.bottom: parent.bottom; width: parent.width; height: 1; color: Theme.border } }
        RowLayout {
            anchors.fill: parent
            anchors.leftMargin: window.sidebarWidth + 8; anchors.rightMargin: 10
            spacing: 6
            Item { Layout.fillWidth: true }
            // Same toolbar item, other content: the booking filter makes no sense in the wealth view.
            Segmented {
                visible: AppModel.sidebarSelection === "wealth"
                options: AppModel.wealthScopes
                current: AppModel.wealthScope
                onActivated: id => AppModel.wealthScope = id
            }
            // DocumentsScopePicker (DocumentsUI.swift, #526) in „Belege & PostBox“
            Segmented {
                visible: AppModel.sidebarSelection === "documents"
                options: DocumentsScope.options
                current: DocumentsScope.current
                onActivated: id => DocumentsScope.current = id
                HoverHandler { id: docScopeHover }
                ToolTip.text: qsTr("Alle Dokumente, eigene Belege oder Bankdokumente anzeigen"); ToolTip.visible: docScopeHover.hovered; ToolTip.delay: 600
            }
            Segmented {
                visible: AppModel.sidebarSelection !== "wealth" && AppModel.sidebarSelection !== "documents"
                options: AppModel.scopes
                current: AppModel.scope
                enabled: AppModel.sidebarSelection !== "newBookings"
                onActivated: id => AppModel.scope = id
                ToolTip.text: qsTr("Ansicht wählen (Strg+1, Strg+2, Strg+3)")
            }
            IconButton { symbol: "paperplane"; enabled: AppModel.canTransfer; onClicked: AppModel.openTransfer(); ToolTip.text: qsTr("Neue Überweisung (Strg+N)"); ToolTip.visible: hovered }
            // .disabled(bankingBlocksAll).help(gate message): the wrapper sees the hover of the disabled button
            Item {
                implicitWidth: syncAllButton.implicitWidth; implicitHeight: syncAllButton.implicitHeight
                HoverHandler { id: syncAllHover }
                IconButton {
                    id: syncAllButton
                    anchors.fill: parent
                    symbol: "arrow.triangle.2.circlepath"
                    enabled: AppModel.syncingConnectionID === null && !AppModel.bankingBlocksAll
                    onClicked: AppModel.requestSyncAll()
                    ToolTip.text: AppModel.bankingBlocksAll ? (AppModel.bankingPolicy.message || "") : qsTr("Alle Konten abrufen (Strg+R)")
                    ToolTip.visible: syncAllHover.hovered
                }
            }
            IconButton {
                symbol: "sidebar.trailing"
                checkable: true; checked: AppModel.showInspector
                onToggled: AppModel.showInspector = checked
                ToolTip.text: AppModel.showInspector ? qsTr("Informationen ausblenden (Strg+Alt+I)") : qsTr("Informationen einblenden (Strg+Alt+I)")
                ToolTip.visible: hovered
            }
            // MainToolbar › ToolbarItem(id: "privacy") (BookingListView.swift, #719)
            IconButton {
                symbol: PrivacyMode.enabled ? "eye.slash" : "eye"
                checkable: true; checked: PrivacyMode.enabled
                onToggled: PrivacyMode.setEnabled(checked)
                ToolTip.text: PrivacyMode.enabled ? qsTr("Privatmodus beenden (Strg+Umschalt+H)") : qsTr("Privatmodus: Beträge ausblenden (Strg+Umschalt+H)")
                ToolTip.visible: hovered
            }
            TextField {
                id: search
                Layout.preferredWidth: Math.max(220, Math.min(510, window.width * 0.22))
                placeholderText: qsTr("Händler, Zweck, IBAN, Betrag")
                text: AppModel.searchText
                onTextEdited: AppModel.searchText = text
                leftPadding: 28
                SFIcon { symbol: "magnifyingglass"; size: 14; anchors.verticalCenter: parent.verticalCenter; x: 8 }
                Keys.onEscapePressed: AppModel.searchText = ""
                Sensitive { when: search.text !== "" }   // typed names, IBANs, amounts (#301)
            }
        }
    }


    // .overlay(alignment: .bottom) { toast }
    Rectangle {
        id: toastView
        visible: opacity > 0
        opacity: AppModel.toast !== "" ? 1 : 0
        Behavior on opacity { NumberAnimation { duration: 250 } }
        anchors.horizontalCenter: parent.horizontalCenter
        anchors.bottom: parent.bottom; anchors.bottomMargin: 22
        width: toastRow.implicitWidth + 32; height: toastRow.implicitHeight + 20
        radius: height / 2
        color: Theme.card; border.color: Theme.border
        z: 100
        RowLayout { id: toastRow; anchors.centerIn: parent; spacing: 6
            SFIcon { symbol: "checkmark.circle.fill"; size: 16; color: Theme.inflow }
            Text { text: AppModel.toast; color: Theme.text; font.pixelSize: Theme.body } }
        // a changed notice gets its full time again
        Timer { id: toastTimer; interval: 3500; onTriggered: AppModel.toast = "" }
        Connections { target: AppModel; function onToastChanged() { if (AppModel.toast !== "") toastTimer.restart() } }
    }

    // Sheets of MainSplitView
    PinSheet {}
    // Settings & recurring: sheets of the main window, separate windows.
    RecurringSheet {}
    CustomDatesSheet {}
    CustomPeriodForm {}
    // Separate windows, created when opened (built eagerly they cost start-up time
    // and memory, the help window carries the whole help text).
    Loader { active: AppModel.showSettings; sourceComponent: Component { SettingsWindow {} }
             onLoaded: { item.requestActivate(); item.raise() } }
    Loader { active: AppModel.showSyncLog; sourceComponent: Component { SyncLogWindow {} }
             onLoaded: { item.reload(); item.requestActivate(); item.raise() } }
    Loader { active: AppModel.showHelp; sourceComponent: Component { HelpWindow {} }
             onLoaded: { item.requestActivate(); item.raise(); item.focusSearch() } }
    Loader { active: Announcements.showWindow; sourceComponent: Component { AnnouncementsWindow {} } }
    // BugReportView: its own window, so the main window stays free to be captured.
    Loader { active: AppModel.showBugReport
             sourceComponent: Component { BugReportWindow { mainWindow: window; captureItem: rootLayout } }
             onLoaded: { item.requestActivate(); item.raise() } }
    TwoFactorSheet {}
    BankSetupSheet {}
    AccountDocumentsSheet {}
    Loader {
        active: AppModel.showAddBank
        sourceComponent: Component {
            AddBankSheet {
                visible: AppModel.showAddBank
                onClosed: AppModel.showAddBank = false
            }
        }
    }
    // LanguagePromptView: before the vault opens, only when the system speaks neither language.
    Loader {
        anchors.fill: parent
        z: 1000
        active: window.showLanguagePrompt
        sourceComponent: Component {
            LanguagePromptView {
                onDone: { window.languagePromptDone = true; Qt.callLater(AppModel.bootstrap) }
            }
        }
    }
    // SystemLanguageChangedAlert: the system language moved away from the chosen app language.
    Sheet {
        id: systemLanguageChanged
        readonly property string newCode: window.languageQuestion.startsWith("changed:") ? window.languageQuestion.slice(8) : ""
        readonly property string newName: ({ de: "Deutsch", en: "English" })[newCode] || newCode
        readonly property string currentName: ({ de: "Deutsch", en: "English" })[(AppModel.run(() => AppModel.core("language_state")) || { running: "de" }).running] || ""
        sheetWidth: 420
        ColumnLayout {
            width: systemLanguageChanged.availableWidth
            spacing: 12
            Text { text: qsTr("Die Systemsprache wurde geändert"); font.pixelSize: Theme.body + 2; font.weight: Font.Bold; color: Theme.text; wrapMode: Text.WordWrap; Layout.fillWidth: true }
            Text {
                text: qsTr("Ihr Computer verwendet jetzt %1, GiroCrab ist auf %2 eingestellt. Zum Wechseln startet GiroCrab neu.")
                      .arg(systemLanguageChanged.newName).arg(systemLanguageChanged.currentName)
                font.pixelSize: Theme.body; color: Theme.secondary; wrapMode: Text.WordWrap; Layout.fillWidth: true
            }
            RowLayout {
                Layout.fillWidth: true
                Item { Layout.fillWidth: true }
                Button {
                    text: qsTr("%1 beibehalten").arg(systemLanguageChanged.currentName)
                    onClicked: { AppModel.run(() => AppModel.core("acknowledge_system_language")); systemLanguageChanged.close() }
                }
                Button {
                    text: qsTr("Zu %1 wechseln").arg(systemLanguageChanged.newName)
                    highlighted: true; focus: true
                    onClicked: {
                        AppModel.run(() => AppModel.core("set_language_choice", { choice: "system" }))
                        AppModel.run(() => AppModel.core("request_relaunch"))
                        Qt.quit()
                    }
                }
            }
        }
    }

    // Printing: QtWebEngine lays the report out (optional at runtime, loaded per job).
    Loader {
        active: AppModel.printJob !== null
        source: "WebPrinter.qml"
        onStatusChanged: if (status === Loader.Error)
            Qt.callLater(AppModel.finishPrintJob, false, qsTr("Zum Drucken fehlt QtWebEngine (Paket „qml6-module-qtwebengine“)."))
        onLoaded: {
            item.finished.connect(ok => Qt.callLater(AppModel.finishPrintJob, ok, ""))
            item.render(AppModel.printJob.html, AppModel.printJob.path)
        }
    }
    // Sheets with long lists (categories, accounts, bookings, symbols) are built only
    // while open – like SwiftUI's .sheet – and dropped again when closed. Built eagerly,
    // every category reload rebuilt all of them (minutes with a large MoneyMoney import).
    // categorizing, rules, categories
    Loader { active: AppModel.showRuleEditor; sourceComponent: Component { RuleEditorView {} } }
    Loader { active: AppModel.showCategorizer; sourceComponent: Component { CategorizerSheet {} } }
    Loader { active: AppModel.splitBooking !== null; sourceComponent: Component { SplitSheet {} } }
    Loader { active: AppModel.newCategoryRequest !== null; sourceComponent: Component { NewCategoryDialog {} } }
    Loader { active: AppModel.budgetCategoryID !== null; sourceComponent: Component { BudgetEditorSheet {} } }
    CategoryDialogs {}
    Loader {
        active: AppModel.showMoneyMoneyImport
        sourceComponent: Component {
            MoneyMoneyImportSheet {
                visible: AppModel.showMoneyMoneyImport
                onClosed: AppModel.showMoneyMoneyImport = false
            }
        }
    }
    Loader {
        active: AppModel.showPrintSheet
        sourceComponent: Component {
            PrintSheet {
                visible: AppModel.showPrintSheet
                onClosed: AppModel.showPrintSheet = false
            }
        }
    }
    // .fileExporter(… defaultFilename: "GiroCrab-Buchungen")
    FileDialog {
        id: csvExporter
        title: qsTr("Buchungen als CSV exportieren")
        fileMode: FileDialog.SaveFile
        defaultSuffix: "csv"
        nameFilters: [qsTr("CSV-Dateien (*.csv)")]
        currentFolder: StandardPaths.writableLocation(StandardPaths.DocumentsLocation)
        selectedFile: currentFolder + "/GiroCrab-Buchungen.csv"
        visible: AppModel.showCSVExporter
        onAccepted: { AppModel.showCSVExporter = false; AppModel.writeFilteredBookingsCSV(AppModel.localPath(selectedFile)) }
        onRejected: AppModel.showCSVExporter = false
    }

    // Umsätze exportieren (ExportSheet.swift); „Unter …“ asks here, after the sheet has closed.
    Loader { active: AppModel.showExportSheet; sourceComponent: Component { ExportSheet { visible: AppModel.showExportSheet } } }
    FileDialog {
        readonly property var request: AppModel.exportSaveRequest
        readonly property string folder: "file://" + BookingExport.preferredDirectory()
        title: qsTr("Umsätze sichern")
        acceptLabel: qsTr("Sichern")
        fileMode: FileDialog.SaveFile
        nameFilters: !request ? [] : request.format === "pdf" ? [qsTr("PDF-Dokumente (*.pdf)")]
                   : request.format === "csv" ? [qsTr("CSV-Dateien (*.csv)")] : [qsTr("XML-Dateien (*.xml)")]
        currentFolder: folder
        selectedFile: request ? folder + "/" + request.filename : ""
        visible: request !== null
        onAccepted: BookingExport.saveExportRequest(AppModel.localPath(selectedFile))
        onRejected: BookingExport.saveExportRequest(null)
    }

    TransferSheet {}
    PayeeCheckSheet {}
    // Sammelüberweisung, Dauerauftrag (RootView: showBatchSheet, showStandingSheet, standingUnknown)
    BatchTransferSheet {}
    StandingOrderSheet {}
    // MARK: manual loans / Startsaldo: .manualLoanSheets(model), .openingBalanceSheet(model)
    LoanEditorSheet {}
    // Sachwerte (TangibleAssetsUI.swift › tangibleAssetSheets)
    AssetEditorSheet {}
    AssetActionSheet {}
    AssetClassificationSheet {}
    OpeningBalanceSheet {}

    // MARK: account groups & command palette (AccountGroupsUI.swift, CommandPalette.swift)
    AccountGroupDialogs {}
    Loader { active: AppModel.showGroupManager; sourceComponent: Component { GroupManagerSheet {} } }
    Loader { active: TagManager.shown; sourceComponent: Component { TagManagerSheet {} } }
    Loader { active: AppModel.accountSelectionConnection !== null; sourceComponent: Component { AccountSelectionSheet {} } }
    Loader { active: AppModel.showCommandPalette; sourceComponent: Component { CommandPaletteSheet {} } }

    // MARK: - Web logins
    // .sheet(item: $model.webLoginConnection): WebLoginSheet.qml needs QtWebEngine, so it is
    // loaded on first use and then kept (its browser profile holds the portal sessions).
    Loader {
        id: webLoginLoader
        active: false
        source: "WebLoginSheet.qml"
        Connections {
            target: AppModel
            function onWebLoginConnectionChanged() { if (AppModel.webLoginConnection !== null) webLoginLoader.active = true }
        }
        onStatusChanged: if (status === Loader.Error) {
            const c = AppModel.webLoginConnection
            active = false
            AppModel.cancelWebLogin()
            AppModel.errorTitle = qsTr("Browser-Fenster nicht verfügbar")
            AppModel.errorMessage = qsTr("Für die Anmeldung im Browser-Fenster (%1) wird QtWebEngine benötigt (Paket qml6-module-qtwebengine).").arg(c ? c.display_name : "")
        }
    }

    // .alert("Stornierung nicht bestätigt") (RootView.swift)
    Sheet {
        id: transferCancelDialog
        sheetWidth: 460
        visible: AppModel.transferCancelNotice !== null
        onClosed: AppModel.transferCancelNotice = null
        ColumnLayout {
            width: transferCancelDialog.availableWidth
            spacing: 12
            Text { text: qsTr("Stornierung nicht bestätigt"); font.pixelSize: Theme.body + 2; font.weight: Font.Bold; color: Theme.text
                   wrapMode: Text.WordWrap; Layout.fillWidth: true }
            TextEdit {
                Layout.fillWidth: true; readOnly: true; selectByMouse: true; wrapMode: Text.WordWrap; color: Theme.text; font.pixelSize: Theme.body
                text: AppModel.transferCancelNotice ? AppModel.transferCancelMessage(AppModel.transferCancelNotice.outcome) : ""
            }
            RowLayout {
                Item { Layout.fillWidth: true }
                Button { text: qsTr("Schließen"); onClicked: transferCancelDialog.close() }
                Button { text: qsTr("Status erneut prüfen"); highlighted: true
                         enabled: AppModel.transferCancelNotice !== null && !AppModel.transferCancelNotice.busy
                         onClicked: AppModel.recheckTransferCancel() }
            }
        }
    }

    // .alert("Status der Überweisung unklar")
    Sheet {
        id: transferUnknownDialog
        sheetWidth: 440
        visible: AppModel.transferUnknown !== null
        onClosed: AppModel.transferUnknown = null
        ColumnLayout {
            width: transferUnknownDialog.availableWidth
            spacing: 12
            Text { text: qsTr("Status der Überweisung unklar"); font.pixelSize: Theme.body + 2; font.weight: Font.Bold; color: Theme.text; wrapMode: Text.WordWrap; Layout.fillWidth: true }
            TextEdit {
                Layout.fillWidth: true; readOnly: true; selectByMouse: true; wrapMode: Text.WordWrap; color: Theme.text; font.pixelSize: Theme.body
                text: AppModel.transferUnknown ? qsTr("Der Auftrag hat die Bank erreicht, aber die Antwort ist nicht eindeutig. Die Überweisung wurde möglicherweise bereits ausgeführt.\n\nBitte prüfen Sie zuerst Ihr Konto (Umsätze oder Online-Banking), bevor Sie sie erneut senden. In GiroCrab ist sie als „Status unklar“ vorgemerkt.\n\nReferenz (EndToEnd-ID): %1\nBank: %2")
                                                 .arg(AppModel.transferUnknown.end_to_end_id).arg(AppModel.transferUnknown.reason) : ""
            }
            RowLayout {
                Item { Layout.fillWidth: true }
                Button { text: qsTr("Verstanden"); highlighted: true; onClicked: transferUnknownDialog.close() }
            }
        }
    }

    // .alert(errorTitle, errorMessage) with "Erneut versuchen"
    Sheet {
        id: errorDialog
        sheetWidth: 420
        visible: AppModel.errorMessage !== ""
        onClosed: { AppModel.errorMessage = ""; AppModel.errorTitle = ""; AppModel.errorRetry = null; AppModel.errorAlternatives = [] }
        ColumnLayout {
            width: errorDialog.availableWidth
            spacing: 12
            Text { text: AppModel.errorTitle || qsTr("Etwas ist schiefgelaufen"); font.pixelSize: Theme.body + 2; font.weight: Font.Bold; color: Theme.text; wrapMode: Text.WordWrap; Layout.fillWidth: true }
            TextEdit { text: AppModel.errorMessage; readOnly: true; selectByMouse: true; wrapMode: Text.WordWrap; color: Theme.text; font.pixelSize: Theme.body; Layout.fillWidth: true }
            // errorAlternatives (rejected FinTS registration): long titles, one per line
            Repeater {
                model: AppModel.errorAlternatives
                Button { required property var modelData; text: modelData.title; Layout.alignment: Qt.AlignRight
                         onClicked: { const a = modelData.action; errorDialog.close(); a() } }
            }
            RowLayout {
                Item { Layout.fillWidth: true }
                Button { visible: AppModel.errorRetry !== null; text: qsTr("Erneut versuchen")
                         onClicked: { const r = AppModel.errorRetry; errorDialog.close(); if (r) r() } }
                Button { text: qsTr("OK"); highlighted: true; onClicked: errorDialog.close() }
            }
        }
    }

    // --screenshot <path>: render the demo once and save the window (Screenshot.swift).
    Timer {
        running: AppModel.screenshotMode && AppModel.vaultState !== "starting" && AppModel.vaultState !== "unlocking"
        // `--demo-sync`: long enough for the simulated PIN / TAN sheet to appear
        interval: AppModel.optionDemoSync ? 4000 : 1500
        onTriggered: { AppModel.core("grab_window", { path: AppModel.screenshotPath, title: AppModel.screenshotWindow }); Qt.quit() }
    }
}
