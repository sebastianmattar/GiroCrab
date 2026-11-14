import Foundation
import SwiftUI
import GiroCrabCore

extension AppModel {
    // MARK: - Recurring bookings (GitHub #35)

    func refreshRecurring() {
        settlePendingSelectionLoad()
        guard let vault else { recurring = []; recurringMonth = nil; return }
        let ids = selectionAccountIDs
        let c = Calendar(identifier: .gregorian).dateComponents([.year, .month], from: Date())
        runCore {
            let snapshot = try vault.recurringSnapshot(accountIds: ids, year: Int32(c.year ?? 2026), month: UInt32(c.month ?? 1))
            recurring = snapshot.series
            recurringHidden = snapshot.ignored
            recurringMonth = snapshot.month
        }
    }

    /// The recurring series a booking belongs to, if any.
    func recurringSeries(for b: Booking) -> RecurringSeries? {
        guard !b.recurrenceKey.isEmpty else { return nil }
        return recurringByKey[b.recurrenceKey]?.first { s in
            (!s.amountSpecific || abs(b.amountCents - s.amountCents) * 100 <= abs(s.amountCents) * 15)
        }
    }

    func markRecurring(_ b: Booking, _ interval: RecurrenceInterval, customDates: [String] = []) {
        guard let vault else { return }
        runCore { try vault.markBookingRecurring(bookingId: b.id, interval: interval, customDates: customDates) }
        refreshRecurring()
    }

    /// "Nicht wiederkehrend" for a detected or marked series.
    func ignoreRecurring(_ s: RecurringSeries) {
        guard let vault else { return }
        runCore { try vault.ignoreRecurring(seriesKey: s.seriesKey) }
        refreshRecurring()
    }

    /// Back to automatic detection (also shows a hidden series again).
    func resetRecurring(_ s: RecurringSeries) { resetRecurring(key: s.seriesKey) }

    func resetRecurring(key: String) {
        guard let vault else { return }
        runCore { try vault.resetRecurring(seriesKey: key) }
        refreshRecurring()
    }

    /// Selects a booking in the list, widening the period if it is outside of it.
    func revealBooking(_ id: Int64) {
        if booking(withID: id) == nil {
            sidebarSelection = .allAccounts
            period = .all
            directionFilter = .all
            statusFilter = .all
            searchText = ""
        }
        // After a closing sheet / list reload, so the list can scroll to the row.
        Task { @MainActor [weak self] in
            try? await Task.sleep(nanoseconds: 350_000_000)
            guard let self else { return }
            // an older booking is not on the first page: load back to its day
            if booking(withID: id) == nil, let day = try? vault?.bookingDate(bookingId: id) {
                loadBookings(through: day)
            }
            if selectedBookingID == id { selectedBookingID = nil }
            selectedBookingID = id
        }
    }
}
