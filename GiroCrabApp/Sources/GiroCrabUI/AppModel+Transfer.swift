import Foundation
import SwiftUI
import GiroCrabCore

// MARK: - SEPA-Überweisung

extension AppModel {
    /// Checking accounts that are connected via FinTS (HKCCS-capable banks).
    var transferAccounts: [Account] { sendableAccounts(of: .checking) }

    /// Savings accounts (Tagesgeld) the bank lets us book from, but only as an
    /// internal transfer (Umbuchung) to an own account (GitHub #582). Kept apart
    /// from `transferAccounts` so standing orders, batches and direct debits
    /// do not offer them.
    var savingsTransferSources: [Account] { sendableAccounts(of: .savings) }

    /// Accounts of `kind` with an IBAN that a connected, write-capable bank can send from.
    private func sendableAccounts(of kind: AccountKind) -> [Account] {
        let readOnly = Set(connections.filter { usesRah10($0) }.map(\.id))
        return allAccounts.filter {
            $0.kind == kind && !$0.iban.isEmpty && !$0.isArchived
                && !($0.connectionId.map { readOnly.contains($0) } ?? false)
                && [.volksbankKasselGoettingen, .direkt1822, .finTsGeneric, .bunq].contains($0.connector)
        }
    }

    /// Own accounts a savings account may transfer to: checking accounts only,
    /// the bank's own reference account first (same connection).
    func internalTransferTargets(from source: Account) -> [Account] {
        allAccounts.filter {
            $0.kind == .checking && !$0.iban.isEmpty && !$0.isArchived && $0.id != source.id
        }.sorted { a, b in
            let aSame = a.connectionId != nil && a.connectionId == source.connectionId
            let bSame = b.connectionId != nil && b.connectionId == source.connectionId
            return aSame && !bSame
        }
    }

    /// Checking accounts GiroCrab cannot send from (e.g. C24 via Enable Banking,
    /// which is read-only): the transfer form ends with a GiroCode to scan in
    /// the bank's app instead of a TAN.
    var giroCodeAccounts: [Account] {
        let sending = Set(transferAccounts.map(\.id))
        // Archived accounts are out of the form entirely (like transferAccounts);
        // otherwise an archived FinTS account would fall through to here.
        return allAccounts.filter {
            $0.kind == .checking && !$0.iban.isEmpty && !$0.isArchived && !sending.contains($0.id)
        }
    }

    /// Accounts offered in the transfer form: sending ones first.
    var transferFormAccounts: [Account] { transferAccounts + savingsTransferSources + giroCodeAccounts }

    func canSendTransfer(from accountID: Int64?) -> Bool {
        (transferAccounts + savingsTransferSources).contains { $0.id == accountID } && bankingGate(forAccount: accountID) == nil
    }

    var canTransfer: Bool { vault != nil && !transferFormAccounts.isEmpty && syncingConnectionID == nil && activeTransfer == nil }

    /// "Neue Überweisung": an empty form, only the account is kept (#881). The
    /// sheet's own reopenings (bank error, declined instant, name check) keep
    /// the draft; they set `showTransferSheet` directly.
    func openTransfer() {
        guard canTransfer else { return }
        transferDraft = TransferDraft(accountID: transferDraft.accountID)
        presentTransfer()
    }

    /// Pre-selects the sidebar account if it is offered in the form, else the first one.
    func presentTransfer() {
        guard canTransfer else { return }
        let offered = transferFormAccounts
        var selected: Int64?
        if case .account(let id) = sidebarSelection, offered.contains(where: { $0.id == id }) { selected = id }
        if transferDraft.accountID == nil || !offered.contains(where: { $0.id == transferDraft.accountID }) {
            transferDraft.accountID = selected ?? offered.first?.id
        } else if let selected {
            transferDraft.accountID = selected
        }
        transferError = nil
        transferInstantDeclined = nil
        showTransferSheet = true
    }

    /// After an instant transfer was declined (nothing sent): send the same
    /// draft as a regular SEPA transfer. A new order with its own approval,
    /// only ever started by the user.
    func sendAsRegularTransfer() {
        transferInstantDeclined = nil
        transferDraft.instant = false
        startTransfer()
    }

    /// Context menu: new transfer pre-filled from a booking.
    func openTransfer(from b: Booking) {
        guard canTransfer else { return }
        transferDraft = TransferDraft(accountID: transferDraft.accountID)
        transferDraft.name = b.counterparty.isEmpty ? b.merchantName : b.counterparty
        transferDraft.iban = TransferSheetView.groupedIBAN(b.counterpartyIban)
        transferDraft.bic = ""
        transferDraft.amountText = Money.plain(abs(b.amountCents))
        transferDraft.purpose = String(b.purpose.prefix(140))
        presentTransfer()
    }

    /// A template, a paste or an invoice names a recipient: that is a transfer to
    /// someone else, so it leaves Umbuchung first (#881); the form would otherwise
    /// show an own account while the order goes to this IBAN. A savings account
    /// may only book to own accounts: from there only amount and purpose are taken.
    func fillTransferDraft(_ p: ParsedPayment, onlyEmpty: Bool = false) {
        let savings = allAccounts.first { $0.id == transferDraft.accountID }?.kind == .savings
        if savings {
            if let amount = p.amountText, !onlyEmpty || transferDraft.amountText.isEmpty {
                transferDraft.amountText = amount
            }
            if let purpose = p.purpose, !onlyEmpty || transferDraft.purpose.isEmpty {
                transferDraft.purpose = String(purpose.prefix(140))
            }
            return
        }
        transferDraft.ownTransfer = false
        p.apply(to: &transferDraft, onlyEmpty: onlyEmpty)
    }

    /// One per IBAN, newest first, hidden ones left out. Synchronous on purpose (every
    /// keystroke): the core keeps the list until the data changes (~11 ms to rebuild on
    /// 36,000 outgoing bookings with an IBAN, `category_perf`).
    func suggestRecipients(_ query: String) -> [KnownRecipient] {
        (try? vault?.suggestRecipients(query: query, limit: 5)) ?? []
    }

    /// Every known recipient, hidden ones included ("Empfänger verwalten", #978). Nil when the
    /// vault was locked or swapped meanwhile; a failed read throws, so it never reads as "none".
    func knownRecipients(_ query: String) async throws -> [KnownRecipient]? {
        try await vaultOffMain { try $0.listKnownRecipients(query: query) }
    }

    /// "Nicht mehr vorschlagen" or show again; the bookings stay. A failure ends up in the alert.
    func setRecipientHidden(_ iban: String, _ hidden: Bool) {
        runCore { try vault?.setRecipientHidden(iban: iban, hidden: hidden) }
    }

    func startTransfer() {
        withFreshBankingPolicy { [self] in
            if let gate = bankingGate(forAccount: transferDraft.accountID) {
                transferError = gate.message
                return
            }
            startTransferChecked()
        }
    }

    private func startTransferChecked() {
        guard let vault, let accID = transferDraft.accountID, canSendTransfer(from: accID),
              let account = allAccounts.first(where: { $0.id == accID }),
              let connection = connections.first(where: { $0.id == account.connectionId }) else { return }
        do {
            let cents = try parseAmountToCents(text: transferDraft.amountText)
            let bic = transferDraft.bic.trimmingCharacters(in: .whitespaces)
            // Instant and scheduled transfers are FinTS-only.
            let offersInstant = account.connector?.offersInstantChoice ?? false
            let request = SepaTransferRequest(
                accountId: accID,
                recipientName: transferDraft.name,
                recipientIban: transferDraft.iban,
                recipientBic: bic.isEmpty ? nil : bic,
                amountCents: cents,
                purpose: transferDraft.purpose,
                instant: transferDraft.instant && offersInstant,
                executionDate: offersInstant ? transferDraft.executionDateISO : nil)
            let stored = connectionStatus(connection.id)?.hasPin ?? false
            if !isDemo && !stored && transferDraft.pin.isEmpty {
                transferError = String(localized: "Bitte die Banking-PIN eingeben.")
                return
            }
            let session = try vault.beginTransfer(request: request, secret: isDemo ? "demo" : transferDraft.pin, demo: isDemo)
            transferError = nil
            transferInstantDeclined = nil
            transferBusy = true
            showTransferSheet = false
            activeTransfer = session
            Task {
                do {
                    self.handleTransfer(try await session.start(), connection: connection)
                } catch {
                    self.finishTransfer(error: error)
                }
            }
        } catch {
            transferError = describe(error)
        }
    }

    func handleTransfer(_ step: TransferStep, connection: BankConnection) {
        if case .needsPayeeConfirmation = step {} else { payeeCheck = nil }
        switch step {
        case .needsPayeeConfirmation(let check):
            pollTask?.cancel()
            activeChallenge = nil
            payeeCheck = PendingPayeeCheck(connection: connection, check: check)
        case .needsChallenge(let c):
            activeChallenge = ActiveChallenge(connection: connection, challenge: c, waiting: false)
            if c.kind == .decoupledPush || c.kind == .progress { schedulePoll(connection: connection, interval: c.pollIntervalSeconds) }
        case .stillWaiting(let c):
            if activeChallenge == nil {
                activeChallenge = ActiveChallenge(connection: connection, challenge: c, waiting: true)
            } else {
                activeChallenge?.waiting = true
                activeChallenge?.busy = false
            }
            schedulePoll(connection: connection, interval: c.pollIntervalSeconds)
        case .finished(let outcome):
            // Executed but not stored: still finish as sent (no resend), then warn.
            let isDebit = activeTransferIsDebit
            defer { if let error = outcome.recordError { reportUnrecordedOrder(error, debit: isDebit) } }
            if activeTransferIsDebit {
                finishDebitGroup(connection: connection)
                return
            }
            if activeTransferIsStanding {
                finishStandingOrder()
                return
            }
            plannedVersion += 1 // a scheduled transfer now shows under „Geplant“
            pollTask?.cancel()
            activeChallenge = nil
            activeTransfer = nil
            transferBusy = false
            transferDraft = TransferDraft(accountID: transferDraft.accountID, instant: false)
            if activeTransferIsBatch { resetBatchDraft() }
            if outcome.instant {
                toast = String(localized: "Echtzeitüberweisung freigegeben und vorgemerkt")
            } else {
                toast = String(localized: "Überweisung freigegeben und vorgemerkt")
            }
            reloadAll()
        case .instantDeclined(let reason):
            // Nothing was sent. Keep the draft and let the user decide.
            pollTask?.cancel()
            activeChallenge = nil
            activeTransfer = nil
            transferBusy = false
            transferError = nil
            transferInstantDeclined = reason
            showTransferSheet = true
        case .outcomeUnknown(let reason, let endToEndId):
            if activeTransferIsDebit {
                debitOutcomeUnknown(reason: reason)
                return
            }
            if activeTransferIsStanding {
                standingOrderOutcomeUnknown(reason: reason)
                return
            }
            // May have been executed: no retry button, clear the draft so it
            // cannot be resent with one click, and say so clearly.
            pollTask?.cancel()
            activeChallenge = nil
            activeTransfer = nil
            transferBusy = false
            transferDraft = TransferDraft(accountID: transferDraft.accountID, instant: false)
            if activeTransferIsBatch { resetBatchDraft() }
            transferUnknown = TransferUnknownNotice(reason: reason, endToEndId: endToEndId)
            reloadAll()
        }
    }

    /// Name check: send anyway (the bank then asks for the approval).
    func confirmPayee() {
        guard let transfer = activeTransfer, let pending = payeeCheck else { return }
        payeeCheck?.busy = true
        Task {
            do {
                let step = try await transfer.confirmPayee()
                guard self.activeTransfer === transfer else { return }  // cancelled meanwhile
                self.handleTransfer(step, connection: pending.connection)
            } catch {
                guard self.activeTransfer === transfer else { return }
                self.finishTransfer(error: error)
            }
        }
    }

    /// Name check: do not send. `useBankName` takes over the account
    /// holder's name reported by the bank and reopens the form.
    func cancelPayee(useBankName: Bool = false) {
        guard let pending = payeeCheck else { return }
        let transfer = activeTransfer
        payeeCheck = nil
        activeTransfer = nil
        transferBusy = false
        if useBankName, let name = pending.check.bankName {
            transferDraft.name = name
            transferError = String(localized: "Name des Kontoinhabers übernommen – bitte prüfen und erneut freigeben. Es wurde nichts gesendet.")
        } else {
            transferError = String(localized: "Überweisung abgebrochen – es wurde nichts gesendet.")
        }
        showTransferSheet = true
        if let transfer { cancelTransferSession(transfer, confirmedMessage: nil) }
    }

    /// Cancels the session and tells apart a confirmed cancellation from one the
    /// bank has not confirmed. `confirmedMessage` replaces the sheet's error line
    /// only when confirmed.
    func cancelTransferSession(_ transfer: TransferSession, confirmedMessage: String?) {
        Task {
            let outcome = await transfer.cancel()
            settleTransferCancel(transfer, outcome, confirmedMessage: confirmedMessage)
        }
    }

    private func settleTransferCancel(_ transfer: TransferSession, _ outcome: CancelOutcome, confirmedMessage: String?) {
        if case .cancelled = outcome {
            transferCancelNotice = nil
            if let confirmedMessage {
                transferError = confirmedMessage
                showTransferSheet = true
            }
        } else {
            transferError = nil
            transferCancelNotice = UnconfirmedTransferCancel(session: transfer, outcome: outcome)
        }
    }

    /// "Status erneut prüfen": cancel again; the core re-queries before it resends anything.
    func recheckTransferCancel() {
        guard let notice = transferCancelNotice, !notice.busy else { return }
        transferCancelNotice?.busy = true
        Task {
            let outcome = await notice.session.cancel()
            settleTransferCancel(notice.session, outcome,
                                 confirmedMessage: String(localized: "Überweisung bei bunq storniert (bestätigt)."))
        }
    }

    private func reportUnrecordedOrder(_ error: String, debit: Bool) {
        let message = debit
            ? String(localized: "Die Bank hat die Lastschriften ausgeführt, GiroCrab konnte sie aber nicht speichern. Die Mandate sind nicht als eingezogen markiert: Prüfen Sie sie vor dem nächsten Einzug, sonst wird doppelt eingezogen.")
            : String(localized: "Die Bank hat den Auftrag ausgeführt, GiroCrab konnte ihn aber nicht speichern. Er erscheint mit dem nächsten Abruf. Senden Sie ihn nicht erneut.")
        showError(String(localized: "Ausgeführt, aber nicht gespeichert"), "\(message)\n\n\(error)")
    }

    func finishTransfer(error: Error) {
        pollTask?.cancel()
        payeeCheck = nil
        activeChallenge = nil
        activeTransfer = nil
        transferBusy = false
        transferDraft.pin = ""
        transferError = describe(error)
        if activeTransferIsDebit {
            failDebitRun(error)
        } else if activeTransferIsStanding {
            failStandingOrder()
        } else if activeTransferIsBatch {
            batchDraft.pin = ""
            activeTransferIsBatch = false
            showBatchSheet = true  // nothing is lost: the rows are still there
        } else {
            showTransferSheet = true
        }
    }

    /// After a batch was sent: empty form, same account.
    func resetBatchDraft() {
        let account = batchDraft.accountID
        batchDraft = BatchDraft()
        batchDraft.accountID = account
        activeTransferIsBatch = false
    }
}
