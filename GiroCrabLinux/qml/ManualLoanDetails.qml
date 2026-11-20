import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

// ManualLoansUI.swift › ManualLoanDetails: Stand, payments and interest of a manual
// loan (below the repayment bar of LoanProgressCard).
ColumnLayout {
    id: d
    property var loan: null
    readonly property var s: loan ? loan.status : null
    spacing: 6
    function paymentsLine() {
        if (!s) return ""
        if (s.payment_count === 0)
            return loan.setup.match_expr === "" ? qsTr("Noch keine Raten zugeordnet") : qsTr("Noch keine Raten seit dem Stichtag")
        return qsTr("%1 Raten seit Stichtag · %2").arg(s.payment_count).arg(Fmt.money(s.paid_cents))
    }
    Text {
        text: d.loan ? qsTr("Stand %1: %2").arg(Fmt.medium(d.loan.setup.anchor_date)).arg(Fmt.money(d.loan.setup.anchor_balance_cents)) : ""
        font.pixelSize: Theme.caption; color: Theme.text; wrapMode: Text.WordWrap; Layout.fillWidth: true
        Sensitive { amounts: true }
    }
    Text { text: d.paymentsLine(); font.pixelSize: Theme.caption; color: Theme.secondary; wrapMode: Text.WordWrap; Layout.fillWidth: true; Sensitive { amounts: true } }
    Text {
        visible: d.loan !== null && d.loan.setup.interest_rate_ppm !== null
        text: d.loan && d.loan.setup.interest_rate_ppm !== null
              ? qsTr("Sollzins %1 % · Zinsen seit Stichtag %2").arg(Fmt.percentText(d.loan.setup.interest_rate_ppm)).arg(Fmt.money(d.s.interest_cents)) : ""
        font.pixelSize: Theme.caption; color: Theme.secondary; wrapMode: Text.WordWrap; Layout.fillWidth: true
        Sensitive { amounts: true }
    }
    Text {
        visible: d.s !== null && d.s.months_left !== null && d.s.months_left > 0
        text: d.s && d.s.months_left ? qsTr("Voraussichtlich noch %1 Monatsraten").arg(d.s.months_left) : ""
        font.pixelSize: Theme.caption; color: Theme.secondary; wrapMode: Text.WordWrap; Layout.fillWidth: true
    }
    RowLayout {
        Layout.topMargin: 2
        spacing: 8
        Button { text: qsTr("Stand neu setzen …"); font.pixelSize: Theme.caption; onClicked: AppModel.openLoanEditor("anchor", d.loan.account_id) }
        Button { text: qsTr("Bearbeiten …"); font.pixelSize: Theme.caption; onClicked: AppModel.openLoanEditor("edit", d.loan.account_id) }
    }
}
