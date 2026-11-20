import Foundation

/// "Als geprüft markieren": when a selected unchecked booking becomes
/// checked on its own – like a mail app marking a message as read.
/// Stored per device; the checked state itself syncs as usual.
enum AutoReviewDelay: String, CaseIterable, Identifiable {
    case manual
    case immediately = "now"
    case oneSecond = "1s"
    case fiveSeconds = "5s"
    #if os(macOS)
    case onAccountSwitch = "account"
    #endif

    static let defaultsKey = "bookings.autoReviewDelay"
    var id: String { rawValue }

    /// Delay before marking an individual booking.
    var seconds: Double? {
        switch self {
        case .manual: nil
        case .immediately: 0
        case .oneSecond: 1
        case .fiveSeconds: 5
        #if os(macOS)
        case .onAccountSwitch: nil
        #endif
        }
    }

    var title: String {
        switch self {
        case .manual: String(localized: "Nur manuell")
        case .immediately: String(localized: "Sofort")
        case .oneSecond: String(localized: "Nach 1 Sekunde")
        case .fiveSeconds: String(localized: "Nach 5 Sekunden")
        #if os(macOS)
        case .onAccountSwitch: String(localized: "Bei Kontoauswahl")
        #endif
        }
    }

    var helpText: String {
        #if os(macOS)
        if self == .onAccountSwitch {
            return String(localized: "Angeklickte Umsätze werden sofort als gelesen markiert. Beim Wechsel auf ein anderes Konto werden alle übrigen ungelesenen Umsätze des vorherigen Kontos als gelesen markiert, unabhängig von Zeitraum und Filtern.")
        }
        #endif
        return String(localized: "Eine ausgewählte Buchung gilt nach dieser Zeit als geprüft – wie eine gelesene E-Mail. Bei Mehrfachauswahl nie.")
    }

    /// The choice stored on this device; "Nur manuell" until one is made.
    static var stored: AutoReviewDelay {
        AppDefaults.store.string(forKey: defaultsKey).flatMap(AutoReviewDelay.init(rawValue:)) ?? .manual
    }
}
