pragma Singleton
import QtQuick
import QtCore

// BookingExport.swift + AppModel+Printing.swift (Umsatz-Export, GitHub #53 / #372 / #401):
// formats, period presets, remembered options, file names, the white-label Umsatzliste
// (PrintReport.qml with `statement_style`) and saving. CSV and CAMT.053 are written by the
// core (`export_csv`, `camt053_export`); the PDF is laid out by QtWebEngine
// (WebPrinter.qml via AppModel.printJob), with the Mac's stylesheet and paginator.
QtObject {
    id: ex

    // MARK: - BookingExportFormat
    readonly property var formats: [
        { id: "pdf", title: qsTr("PDF"), subtitle: qsTr("Umsatzliste · A4 Hochformat"), ext: "pdf", camt: false },
        { id: "csv", title: "CSV", subtitle: qsTr("Excel · Semikolon · UTF-8"), ext: "csv", camt: false },
        { id: "camt053v08", title: qsTr("CAMT.053"), subtitle: qsTr("ISO 20022 · aktueller Standard"), ext: "xml", camt: true },
        { id: "camt053v02", title: qsTr("CAMT.053 (älter)"), subtitle: qsTr("ISO 20022 · für ältere Programme"), ext: "xml", camt: true }
    ]
    function format(id) { return formats.find(f => f.id === id) || formats[0] }
    function isCamt(id) { return format(id).camt }

    // MARK: - ExportAccountOutput
    readonly property var outputs: [
        { id: "combined", title: qsTr("Alle in einer Datei"), help: qsTr("Eine Umsatzliste mit den gewählten Konten") },
        { id: "perAccount", title: qsTr("Jedes Konto einzeln"), help: qsTr("Eine Datei pro gewähltem Konto nach Downloads") }
    ]

    // MARK: - ExportPeriodPreset
    /// Presets shown as a flat list (custom uses date pickers below).
    readonly property var menuCases: ["current", "thisMonth", "lastMonth", "last3Months", "last12Months",
                                      "thisQuarter", "thisYear", "lastYear", "sinceSalary", "all", "custom"]

    /// Period.title of the list period a preset stands for (AppModelTypes.swift).
    function presetTitle(p, now) {
        now = now || new Date()
        const y = now.getFullYear(), m = now.getMonth()
        switch (p) {
        case "current": return qsTr("Aktuelle Ansicht")
        case "custom": return qsTr("Eigener Zeitraum")
        case "thisMonth": return new Date(y, m, 1).toLocaleDateString(Fmt.locale, "MMMM yyyy")
        case "lastMonth": return new Date(y, m - 1, 1).toLocaleDateString(Fmt.locale, "MMMM yyyy")
        case "last3Months": return qsTr("%1 Tage").arg(90)
        case "last12Months": return qsTr("12 Monate")
        case "thisQuarter": return qsTr("%1. Quartal %2").arg(Math.floor(m / 3) + 1).arg(String(y))
        case "thisYear": return qsTr("Jahr %1").arg(String(y))
        case "lastYear": return qsTr("Jahr %1").arg(String(y - 1))
        case "sinceSalary": return qsTr("Seit letztem Gehalt")
        case "all": return qsTr("Gesamter Zeitraum")
        }
        return ""
    }

    /// ISO from/to for vault queries; `null` means "use the list's period" (`current`).
    function presetRange(p, customFrom, customTo, salaryFrom, now) {
        now = now || new Date()
        const y = now.getFullYear(), m = now.getMonth()
        const I = Fmt.iso
        const span = (sy, sm, months) => [I(new Date(sy, sm, 1)), I(new Date(sy, sm + months, 0))]
        switch (p) {
        case "current": return null
        case "sinceSalary": return salaryFrom ? [salaryFrom, null] : [I(new Date(y, m, 1)), null]
        case "custom": return [customFrom || null, customTo || null]
        case "thisMonth": return span(y, m, 1)
        case "lastMonth": return span(y, m - 1, 1)
        case "last3Months": return [I(new Date(y, m, now.getDate() - 89)), null]
        case "last12Months": return [I(new Date(y - 1, m, now.getDate() + 1)), null]
        case "thisQuarter": return span(y, Math.floor(m / 3) * 3, 3)
        case "thisYear": return span(y, 0, 12)
        case "lastYear": return span(y - 1, 0, 12)
        case "all": return [null, null]
        }
        return null
    }

    /// Period.previousMonth().range (default of „Eigener Zeitraum“).
    function previousMonthRange() { return presetRange("lastMonth", "", "", null) }

    // MARK: - ExportOptions (remembered across launches, separate from print)
    function defaults() {
        return { format: "pdf", scope: "filtered", periodPreset: "lastMonth", customFrom: "", customTo: "",
                 selectedAccountIDs: [], accountOutput: "combined", showCategory: true, showNote: false,
                 showTotals: false, groupByAccount: true, showCurrentBalance: false, showDailyBalance: false,
                 firstLineOnly: true, direction: "all", newestFirst: false }
    }
    property Settings store: Settings {
        category: "export"
        /// ExportOptions as JSON (UserDefaults "export.options").
        property string options: ""
        /// Last save folder (UserDefaults "export.lastDirectory").
        property string lastDirectory: ""
    }
    function load() {
        const o = defaults()
        try {
            const saved = store.options ? JSON.parse(store.options) : {}
            for (const k in o) if (saved[k] !== undefined && typeof saved[k] === typeof o[k]) o[k] = saved[k]
            if (!formats.some(f => f.id === o.format)) o.format = "pdf"
            if (menuCases.indexOf(o.periodPreset) < 0) o.periodPreset = "lastMonth"
        } catch (e) {}
        return o
    }
    function save(o) { store.options = JSON.stringify(o) }
    /// True when content toggles differ from a fresh default (opens „Weitere Optionen“).
    function hasNonDefaultContent(o) {
        const d = defaults()
        return o.showCategory !== d.showCategory || o.showNote !== d.showNote || o.showDailyBalance !== d.showDailyBalance
            || o.groupByAccount !== d.groupByAccount || o.firstLineOnly !== d.firstLineOnly || o.newestFirst !== d.newestFirst
    }
    /// Print options used for the PDF HTML (always white-label Umsatzliste).
    function asPrintOptions(o) {
        return { show_category: o.showCategory, show_note: o.showNote, show_totals: false,
                 group_by_account: o.groupByAccount, show_current_balance: false, show_daily_balance: o.showDailyBalance,
                 first_line_only: o.firstLineOnly, direction: o.direction, newest_first: o.newestFirst, logo: "off",
                 statement_style: true }
    }

    // MARK: - ExportDirectory
    function downloadsDirectory() { return AppModel.localPath(StandardPaths.writableLocation(StandardPaths.DownloadLocation)) }
    function preferredDirectory() { return store.lastDirectory || downloadsDirectory() }
    function remember(path) { store.lastDirectory = path.slice(0, path.lastIndexOf("/")) }

    // MARK: - BookingExportBuilder
    function safeFileComponent(s) {
        const trimmed = String(s || "").trim()
        if (trimmed === "") return "Umsaetze"
        return trimmed.replace(/[/:\\]/g, "-").replace(/\n/g, " ").split(/\s+/).filter(x => x !== "").join("-").slice(0, 80)
    }
    function byName(a, b) { return a.name.localeCompare(b.name, undefined, { sensitivity: "base" }) }
    function usedAccounts(accounts, rows) {
        const ids = {}
        for (const r of rows) ids[r.account_id] = true
        return accounts.filter(a => ids[a.id]).sort(byName)
    }

    /// Suggested save name, e.g. `Umsaetze-Girokonto-2025-03-01_2025-03-31.pdf`.
    /// `rangeFrom`/`rangeTo` (the CAMT.053 period, see camtRange) win over the rows' dates.
    function defaultFilename(accounts, rows, formatId, fallbackTitle, rangeFrom, rangeTo) {
        const used = usedAccounts(accounts, rows)
        const dates = rows.map(r => r.date).filter(d => !!d).sort()
        const from = rangeFrom ? rangeFrom : (dates.length ? dates[0] : null)
        const to = rangeTo ? rangeTo : (dates.length ? dates[dates.length - 1] : null)
        const range = from && to ? (from === to ? from : from + "_" + to) : Fmt.iso(new Date())
        const accountPart = used.length === 0 ? "Umsaetze-" + safeFileComponent(fallbackTitle || "Export")
                          : used.length === 1 ? "Umsaetze-" + safeFileComponent(used[0].name)
                          : "Umsaetze-" + used.length + "-Konten"
        if (formatId === "camt053v08") return accountPart + "-CAMT053-001-08-" + range + ".xml"
        if (formatId === "camt053v02") return accountPart + "-CAMT053-001-02-" + range + ".xml"
        return accountPart + "-" + range + "." + format(formatId).ext
    }

    /// Title, period range and IBAN notes for a white-label Umsatzliste.
    function prepareStatement(report) {
        const out = Object.assign({}, report)
        const used = usedAccounts(report.accounts, report.rows)
        const dates = report.rows.map(r => r.date).filter(d => !!d).sort()
        if (dates.length) {
            const from = dates[0], to = dates[dates.length - 1]
            out.period_label = from === to ? PrintReport.statementDate(from)
                             : PrintReport.statementDate(from) + " – " + PrintReport.statementDate(to)
        }
        if (used.length === 1) {
            out.title = used[0].name
            // the tag note stays even when the account name is part of it („Auto“ in „Tags: Auto-Kosten“)
            const tagNote = report.tag_note || ""
            out.filter_notes = (out.filter_notes || []).filter(n => n === tagNote || n.indexOf(used[0].name) < 0)
        } else {
            out.title = qsTr("Umsätze")
        }
        return out
    }

    /// CAMT.053 cannot state a balance after today: the core caps the end date,
    /// so the file name uses the same bound (`camtRange` passes it on).
    function camtEndDate(requested, today) {
        if (!requested) return null
        today = today || Fmt.iso(new Date())
        return requested < today ? requested : today
    }

    /// Request for the core's CAMT.053 export (#483). The core reads the entries and the
    /// booked balances itself; `from`/`to`/`bookingIDs` only narrow the statement.
    function camtRequest(accountID, formatId, from, to, bookingIDs, created) {
        return { account_id: accountID, date_from: from || null, date_to: to || null,
                 booking_ids: bookingIDs ? bookingIDs.slice().sort((a, b) => a - b) : null,
                 version: formatId === "camt053v02" ? "V02" : "V08",
                 created_at: (created || new Date()).toISOString().replace(/\.\d{3}Z$/, "Z") }
    }

    /// Excel CSV with optional Tagessaldo column; formatting and formula guard live in the core.
    function csv(report, o) {
        const rows = PrintReport.rows(report.rows, asPrintOptions(o))
        let lines = []
        if (o.showCurrentBalance) {
            lines = usedAccounts(report.accounts, rows).map(a => ({ name: a.name, balance_cents: a.balance_cents,
                                                                     currency: a.currency, date: a.balance_date }))
        }
        const balances = o.showDailyBalance ? (report.daily_balances[0] || {}) : null
        const out = rows.map((r, i) => {
            let day = null
            if (balances) {
                // Balance on the last row of a day (in output order).
                const next = i + 1 < rows.length ? rows[i + 1].date : null
                const prev = i > 0 ? rows[i - 1].date : null
                const edge = o.newestFirst ? prev !== r.date : next !== r.date
                day = edge && balances[r.date] !== undefined ? balances[r.date] : null
            }
            return { date: r.date, account_name: r.account_name, counterparty: r.counterparty,
                     purpose: PrintReport.displayPurpose(r, o.firstLineOnly), category: r.category || "",
                     note: r.note || "", amount_cents: r.amount_cents, currency: r.currency, day_balance_cents: day }
        })
        return AppModel.core("export_csv", { rows: out, balance_lines: lines,
            columns: { category: o.showCategory, note: o.showNote, daily_balance: o.showDailyBalance } })
    }

    // MARK: - AppModel+Printing (export queries)

    /// Vault filter for an export: the list's filter with the sheet's accounts and period.
    /// `ignoreListFilters` (CAMT.053) drops search, category, direction, tags, merchant and
    /// review status, so the closing balance matches the bank's.
    function exportFilter(accountIDs, preset, o, ignoreListFilters) {
        const filter = AppModel.currentFilter()
        filter.limit = 0
        filter.offset = 0
        if (accountIDs && accountIDs.length) filter.account_ids = accountIDs
        if (ignoreListFilters) {
            filter.category_id = null
            filter.include_subcategories = true
            filter.uncategorized_only = false
            filter.search = null
            filter.scope = "All"
            filter.merchant_key = null
            filter.direction = "All"
            filter.checked = null
            filter.tag_ids = []
            filter.untagged_only = false
        }
        const salary = AppModel.salaryCycle ? AppModel.salaryCycle.last_date : null
        const range = presetRange(preset, o ? o.customFrom : "", o ? o.customTo : "", salary)
        if (range) { filter.date_from = range[0]; filter.date_to = range[1] }
        return filter
    }

    /// Bookings for an export, read on a worker thread (`list_transactions` enriches every row).
    function loadExportBookings(accountIDs, preset, bookingIDs, o, ignoreListFilters, done, failed) {
        const filter = exportFilter(accountIDs, preset, o, ignoreListFilters)
        // the tag note of exactly this query, taken when it starts (`exportReport`)
        const tagNote = TagFilter.noteFor(filter)
        AppModel.coreAsync("list_transactions", { filter: filter }, page => {
            let rows = page.transactions
            if (bookingIDs) rows = rows.filter(b => bookingIDs.indexOf(b.id) >= 0)
            done(rows, tagNote)
        }, failed)
    }

    function exportPeriodLabel(preset, bookingIDs, o) {
        if (bookingIDs) return qsTr("Auswahl")
        if (preset === "current") return AppModel.periodTitle
        if (preset === "custom" && o && o.customFrom)
            return PrintReport.statementDate(o.customFrom) + " – " + PrintReport.statementDate(o.customTo)
        if (preset === "sinceSalary" && AppModel.salaryCycle)
            return qsTr("Seit Gehalt am %1").arg(PrintReport.statementDate(AppModel.salaryCycle.last_date))
        return presetTitle(preset)
    }

    /// Report for already loaded export bookings (cheap: no vault access).
    /// `tagNote`: from `loadExportBookings`, the tag filter its query applied.
    function exportReport(bookings, accountIDs, preset, bookingIDs, o, tagNote) {
        const accounts = AppModel.allAccounts.map(a => ({ id: a.id, name: a.name, iban: a.iban, balance_cents: a.balance_cents,
                                                          balance_date: a.balance_date, currency: a.currency, owner: a.owner }))
        let title = AppModel.title
        if (accountIDs && accountIDs.length === 1) {
            const a = AppModel.account(accountIDs[0])
            if (a) title = a.name
        }
        // The list's own income/expense filter is named in the printout header.
        const notes = AppModel.directionFilter === "Income" ? [PrintReport.directionTitle("income")]
                    : AppModel.directionFilter === "Expense" ? [PrintReport.directionTitle("expenses")] : []
        // and the tag filter (#228)
        if (tagNote) notes.push(tagNote)
        return { title: title, period_label: exportPeriodLabel(preset, bookingIDs, o),
                 rows: bookings.map(b => PrintReport.rowFromBooking(b)), accounts: accounts, daily_balances: ({}),
                 generated_at: new Date(), filter_notes: notes, tag_note: tagNote || "" }
    }

    // MARK: - ExportSheet › buildFileOptional

    /// Builds one export file and calls `done(file)` with { format, filename, text | from_path },
    /// `done(null)` when there are no bookings, or `failed(message)`.
    function buildFile(accountIDs, bookingIDs, o, done, failed) {
        const preset = o.scope === "selection" ? "current" : o.periodPreset
        const camt = isCamt(o.format)
        loadExportBookings(accountIDs.length ? accountIDs : null, preset, bookingIDs, o, camt, (bookings, tagNote) => {
            if (!bookings.length) { done(null); return }
            try {
                let report = exportReport(bookings, accountIDs, preset, bookingIDs, o, tagNote)
                report.generated_at = new Date()
                report = prepareStatement(report)
                // CAMT.053: the core reads the booked balances itself (#483)
                const needsDaily = o.format === "pdf" || (o.showDailyBalance && !camt)
                if (needsDaily && report.rows.length) {
                    const earliest = report.rows.map(r => r.date).reduce((a, b) => (a < b ? a : b))
                    const ids = Object.keys(report.rows.reduce((m, r) => { m[r.account_id] = true; return m }, {}))
                        .map(Number).sort((a, b) => a - b)
                    report.daily_balances = AppModel.printDailyBalances(ids, earliest)
                }
                const range = camtRange(accountIDs, preset, bookingIDs, o)
                const filename = defaultFilename(report.accounts, report.rows, o.format, report.title,
                                                 range ? range.from : null, range ? range.to : null)
                if (o.format === "csv") {
                    done({ format: o.format, filename: filename, text: csv(report, o) })
                } else if (camt) {
                    done({ format: o.format, filename: filename, text: camtXML(accountIDs, preset, bookingIDs, o) })
                } else {
                    renderPdf(report, o, path => done({ format: o.format, filename: filename, from_path: path }), failed)
                }
            } catch (e) {
                failed(e.message)
            }
        }, failed)
    }

    /// ExportSheet.swift › camtRange: period the core states for a CAMT.053 export (`null` for
    /// other formats and marked bookings, where the rows set it). Also names the file, so name
    /// and OPBD/CLBD dates agree.
    function camtRange(accountIDs, preset, bookingIDs, o) {
        if (!isCamt(o.format) || bookingIDs) return null
        const f = exportFilter(accountIDs, preset, o, true)
        return { from: f.date_from || null, to: camtEndDate(f.date_to) }
    }
    /// The core's CAMT.053 export (#483): it reads the entries and the booked balances from the
    /// vault itself. Marked bookings export their own period; a period export uses the same
    /// bounds as the list, without its search/category filters.
    function camtXML(accountIDs, preset, bookingIDs, o) {
        if (accountIDs.length !== 1) throw new Error(qsTr("CAMT.053 nur für ein einzelnes Konto (#372)."))
        const range = camtRange(accountIDs, preset, bookingIDs, o)
        return AppModel.core("camt053_export", { request: camtRequest(
            accountIDs[0], o.format, range ? range.from : null, range ? range.to : null, bookingIDs) })
    }

    /// HTMLPDFRenderer: the Umsatzliste through WebPrinter.qml (QtWebEngine, A4 portrait) into a temp file.
    function renderPdf(report, o, done, failed) {
        const assets = AppModel.core("print_web_assets")
        const tmp = AppModel.localPath(StandardPaths.writableLocation(StandardPaths.TempLocation))
        AppModel.printJob = {
            html: PrintReport.html(report, asPrintOptions(o), "", assets),
            path: tmp + "/girocrab-export-" + Date.now() + ".pdf",
            job_name: "",
            done: (ok, failure, path) => ok ? done(path) : failed(failure || qsTr("PDF konnte nicht erzeugt werden."))
        }
    }

    // MARK: - AppModel+Printing › presentExportFile / presentExportFiles

    /// Writes an export: straight into Downloads (or the last folder), or via the save dialog (Main.qml).
    function presentExportFile(file, askWhere) {
        if (askWhere) { AppModel.exportSaveRequest = file; return }
        try {
            const path = AppModel.core("save_export_file", { text: file.text, from_path: file.from_path,
                dir: preferredDirectory(), fallback_dir: downloadsDirectory(), filename: file.filename })
            remember(path)
            AppModel.toast = qsTr("Gespeichert: %1").arg(path.slice(path.lastIndexOf("/") + 1))
        } catch (e) {
            AppModel.errorTitle = qsTr("Export fehlgeschlagen")
            AppModel.errorMessage = e.message
        }
    }
    /// The save dialog's answer: `path` chosen (the dialog asked about replacing), or `null` = cancelled.
    function saveExportRequest(path) {
        const file = AppModel.exportSaveRequest
        AppModel.exportSaveRequest = null
        if (!file) return
        try {
            if (path === null) {
                if (file.from_path) AppModel.core("save_export_file", { from_path: file.from_path, discard: true })
                return
            }
            AppModel.core("save_export_file", { text: file.text, from_path: file.from_path, path: path })
            remember(path)
        } catch (e) {
            AppModel.errorTitle = qsTr("Export fehlgeschlagen")
            AppModel.errorMessage = e.message
        }
    }
    /// Saves several files into Downloads (or the last folder) without a dialog.
    function presentExportFiles(files) {
        if (!files.length) return
        let saved = 0, firstName = files[0].filename
        for (const file of files) {
            try {
                const path = AppModel.core("save_export_file", { text: file.text, from_path: file.from_path,
                    dir: preferredDirectory(), fallback_dir: downloadsDirectory(), filename: file.filename })
                if (saved === 0) firstName = path.slice(path.lastIndexOf("/") + 1)
                remember(path)
                saved++
            } catch (e) {
                AppModel.errorTitle = qsTr("Export fehlgeschlagen")
                AppModel.errorMessage = e.message
                return
            }
        }
        AppModel.toast = saved === 1 ? qsTr("Gespeichert: %1").arg(firstName) : qsTr("%1 Dateien in Downloads gespeichert").arg(saved)
    }
}
