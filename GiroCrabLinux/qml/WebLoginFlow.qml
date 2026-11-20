import QtQuick
import QtQuick.Layouts

// One web login of WebLoginSheet.qml (the footer row plus what the sheet shows
// above the browser). Subtypes: TrWebLoginFlow (ConnectionsUI.swift › WebLoginSheet),
// PortalWebLoginFlow (PortalUI.swift), AmazonWebLoginFlow (AmazonUI.swift),
// FnzWebLoginFlow (FnzUI.swift), QuirionWebLoginFlow (QuirionUI.swift),
// SchwaebischHallWebLoginFlow (SchwaebischHallUI.swift), EquatePlusWebLoginFlow (EquatePlusUI.swift).
RowLayout {
    /// The sheet (browser API: load, evaluate, runAsync, currentURL, onPortal, …).
    property var browser
    property var connection
    // header
    property string title
    property string info
    property bool infoWarning: false
    property string symbol: "safari"
    property string badgeText: ""
    property string badgeSymbol: ""
    property color badgeColor: Theme.secondary
    property bool busy: false
    property bool showBatchProgress: false
    // browser
    /// The page is loaded only once this is true (Trade Republic drops an old session first).
    property bool readyToLoad: true
    property string startURL: connection ? AppModel.portalURL(connection.connector) : ""
    /// Domains the browser may read from (PortalDomains).
    property var domains: connection ? AppModel.portalDomains(connection.connector) : []
    /// PortalBrowser(captureTraffic:): mirror the portal's JSON fetch/XHR responses.
    property bool captureTraffic: false
    /// PortalBrowser(userScript:): main frame, document start, page world.
    property string userScript: ""
    /// PortalNavigationGuard: main frame https only.
    property bool navigationGuard: true
    /// AccountSelectionPanel over the browser (FNZ, #346); `endAccountChoice` when done.
    property bool choosingAccounts: false
    function endAccountChoice() { choosingAccounts = false }

    /// Esc / window close.
    function cancel() { AppModel.closeWebLogin() }
    /// A page finished loading in the main frame.
    function pageLoaded(url) {}
    /// A JSON response captured by the traffic hook (main frame of the portal).
    function captured(body, url) {}

    Layout.fillWidth: true
    spacing: 8
}
