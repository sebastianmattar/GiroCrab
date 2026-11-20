pragma Singleton
import QtQuick
import QtCore

// PrivacyRedaction.swift › PrivacyMode (#719): hides amounts (or all personal data) in every
// window while the app is shown to someone. A view preference, stored like the Mac's
// UserDefaults (`privacy.enabled`, `privacy.scope`) in GiroCrab.conf, section [privacy].
// Toggled in Darstellung › Privatmodus (Strg+Umschalt+H), with the eye in the toolbar and in
// Einstellungen › Allgemein; AppModel.redaction adds `redaction` to what `Sensitive {}` hides.
QtObject {
    id: mode

    property Settings store: Settings {
        category: "privacy"
        property bool enabled: false
        /// PrivacyMode.Scope: "amounts" | "personal"
        property string scope: ""
    }

    readonly property bool enabled: store.enabled
    function setEnabled(on) { store.enabled = on }
    function toggle() { store.enabled = !store.enabled }

    /// PrivacyMode.Scope, with its title (Einstellungen › Allgemein › „Im Privatmodus ausblenden“).
    readonly property var scopes: [
        { id: "amounts", title: qsTr("Beträge") },
        { id: "personal", title: qsTr("Beträge und persönliche Angaben") }]
    /// The stored choice; until one is picked: names, IBANs and purposes too (PrivacyMode.defaultScope, #1007).
    readonly property string scope: store.scope === "amounts" ? "amounts" : "personal"
    function setScope(id) { store.scope = id }

    /// `Scope.redaction` as AppModel bits (redactPersonal 1, redactAmounts 4): `.amounts`, or
    /// `[.personal, .amounts]`. Plain numbers: AppModel reads this while it is being built.
    function scopeRedaction(id) { return id === "personal" ? 1 | 4 : 4 }
    /// `PrivacyMode.redaction(enabled:scope:)`: what privacy mode adds to every window.
    readonly property int redaction: enabled ? scopeRedaction(scope) : 0
}
