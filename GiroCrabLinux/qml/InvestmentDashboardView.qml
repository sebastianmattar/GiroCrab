import QtQuick
import QtQuick.Controls
import QtQuick.Layouts

// InvestmentDashboardView.swift: savings plans, Trade Republic perks, consumption vs. investment.
ColumnLayout {
    id: dash
    readonly property var d: AppModel.dashboard
    visible: d !== null
    spacing: 14
    FontMetrics { id: sumMetrics; font.pixelSize: Theme.callout + 1; font.weight: Font.DemiBold; font.features: { "tnum": 1 } }
    readonly property real sumColumnWidth: {
        let w = 0
        if (d) for (const c of d.plans.map(p => p.monthly_cents).concat([d.total_monthly_cents])) w = Math.max(w, sumMetrics.advanceWidth(Fmt.money(c)))
        return Math.ceil(w)
    }

    RowLayout {
        Layout.fillWidth: true; spacing: 8
        KPI { Layout.fillWidth: true; Layout.preferredWidth: 1; Layout.fillHeight: true; title: qsTr("Monatlich investiert")
              subtitle: dash.d ? qsTr("%1 aktive Sparpläne").arg(dash.d.plans.filter(p => p.active).length) : ""; value: dash.d ? Fmt.money(dash.d.total_monthly_cents) : ""; tint: Theme.invest }
        KPI { Layout.fillWidth: true; Layout.preferredWidth: 1; Layout.fillHeight: true; title: qsTr("Saveback (1 %)")
              subtitle: dash.d ? qsTr("%1× gutgeschrieben").arg(dash.d.saveback_count) : ""; value: dash.d ? Fmt.money(dash.d.saveback_earned_cents) : ""; tint: Theme.inflow }
        KPI { Layout.fillWidth: true; Layout.preferredWidth: 1; Layout.fillHeight: true; title: qsTr("Round up")
              subtitle: dash.d ? qsTr("%1× investiert").arg(dash.d.roundup_count) : ""; value: dash.d ? Fmt.money(dash.d.roundup_invested_cents) : ""; tint: Theme.invest }
    }

    // PlanGrid
    Card {
        Layout.fillWidth: true
        ColumnLayout {
            width: parent.width; spacing: 0
            RowLayout { Layout.bottomMargin: 10
                Text { text: qsTr("Monatliche Sparpläne"); font.pixelSize: Theme.subheadline + 1; font.weight: Font.DemiBold; color: Theme.text; Layout.fillWidth: true }
                Text { text: dash.d ? qsTr("Ausführungen der letzten %1 Monate").arg(dash.d.months.length) : ""; font.pixelSize: Theme.caption; color: Theme.secondary } }
            // Grid: header, then per plan a divider and the row (.padding(.vertical, 7)), divider, sum.
            RowLayout {
                Layout.fillWidth: true; Layout.bottomMargin: 6
                spacing: 10
                Text { text: qsTr("Plan"); font.pixelSize: Theme.caption2; color: Theme.tertiary; Layout.fillWidth: true }
                Repeater { model: dash.d ? dash.d.months : []
                    Text { required property string modelData; text: Fmt.monthLabel(modelData); font.pixelSize: Theme.caption2; color: Theme.tertiary; Layout.preferredWidth: 40; horizontalAlignment: Text.AlignHCenter } }
                Text { text: qsTr("pro Monat"); font.pixelSize: Theme.caption2; color: Theme.tertiary; horizontalAlignment: Text.AlignRight; Layout.minimumWidth: dash.sumColumnWidth }
            }
            Repeater {
                model: dash.d ? dash.d.plans : []
                ColumnLayout {
                    id: planItem
                    required property var modelData
                    Layout.fillWidth: true
                    spacing: 0
                    Rectangle { Layout.fillWidth: true; height: 1; color: Theme.border }
                    RowLayout {
                        Layout.fillWidth: true; Layout.topMargin: 7; Layout.bottomMargin: 7
                        spacing: 10
                        RowLayout {
                            Layout.fillWidth: true; spacing: 8
                            MerchantSignet { merchant: planItem.modelData.merchant; size: 28 }
                            ColumnLayout { spacing: 1; Layout.fillWidth: true
                                Text { text: planItem.modelData.merchant.name; font.pixelSize: Theme.callout + 1; font.weight: Font.Medium; color: Theme.text; elide: Text.ElideRight; Layout.fillWidth: true }
                                Text { font.family: "monospace"; font.pixelSize: Theme.caption2; color: Theme.secondary; elide: Text.ElideRight; Layout.fillWidth: true
                                       text: { const p = planItem.modelData
                                               const parts = []; if (p.merchant.isin) parts.push(p.merchant.isin); if (p.merchant.wkn) parts.push("WKN " + p.merchant.wkn)
                                               if (!parts.length) parts.push(p.merchant.asset_class || qsTr("Sparplan"))
                                               parts.push(qsTr("am %1.").arg(p.day_of_month)); return parts.join(" · ") } } }
                        }
                        Repeater {
                            model: dash.d ? dash.d.months : []
                            Item {
                                required property string modelData
                                readonly property var exec: planItem.modelData.executions.find(x => x.month === modelData)
                                readonly property int cents: exec ? exec.amount_cents : 0
                                Layout.preferredWidth: 40; implicitHeight: 12
                                Rectangle {
                                    anchors.centerIn: parent; width: 12; height: 12; radius: 6
                                    color: parent.cents > 0 ? Theme.withAlpha(Theme.invest, 0.85) : "transparent"
                                    border.color: parent.cents > 0 ? "transparent" : Theme.border; border.width: 1.5
                                }
                                ToolTip.visible: tipArea.containsMouse
                                ToolTip.text: cents > 0 ? Fmt.monthLabel(modelData) + ": " + AppModel.masking(Fmt.money(cents), AppModel.redactAmounts) : qsTr("nicht ausgeführt")
                                MouseArea { id: tipArea; anchors.fill: parent; hoverEnabled: true }
                            }
                        }
                        Text { text: Fmt.money(planItem.modelData.monthly_cents); font.pixelSize: Theme.callout + 1; font.features: { "tnum": 1 }
                               horizontalAlignment: Text.AlignRight; Layout.minimumWidth: dash.sumColumnWidth
                               color: planItem.modelData.active ? Theme.text : Theme.tertiary; Sensitive { amounts: true } }
                    }
                }
            }
            Rectangle { Layout.fillWidth: true; height: 1; color: Theme.border }
            RowLayout { Layout.topMargin: 8
                Text { text: qsTr("Summe aktive Pläne"); font.pixelSize: Theme.callout + 1; font.weight: Font.DemiBold; color: Theme.text; Layout.fillWidth: true }
                Text { text: dash.d ? Fmt.money(dash.d.total_monthly_cents) : ""; font.pixelSize: Theme.callout + 1; font.weight: Font.DemiBold; font.features: { "tnum": 1 }; color: Theme.text
                       horizontalAlignment: Text.AlignRight; Layout.minimumWidth: dash.sumColumnWidth; Sensitive { amounts: true } } }
        }
    }

    // PerksCard
    Card {
        Layout.fillWidth: true
        ColumnLayout {
            width: parent.width; spacing: 10
            RowLayout {
                SFIcon { symbol: "gift"; size: 14; color: Theme.text }
                Text { text: qsTr("Trade Republic Perks"); font.pixelSize: Theme.subheadline + 1; font.weight: Font.DemiBold; color: Theme.text; Layout.fillWidth: true }
                Text { text: dash.d ? qsTr("diesen Monat %1").arg(Fmt.money(dash.d.perks_this_month_cents)) : ""; font.pixelSize: Theme.caption; color: Theme.secondary; Sensitive { amounts: true } } }
            RowLayout {
                spacing: 12; Layout.fillWidth: true
                Repeater {
                    model: dash.d ? [[qsTr("1 % Saveback"), dash.d.saveback_earned_cents, dash.d.saveback_count, qsTr("Bonus auf Kartenzahlungen, automatisch in den Sparplan investiert.")],
                                     [qsTr("Round up"), dash.d.roundup_invested_cents, dash.d.roundup_count, qsTr("Aufgerundete Cent-Beträge jeder Kartenzahlung, monatlich investiert.")]] : []
                    Rectangle {
                        required property var modelData
                        Layout.fillWidth: true; Layout.preferredWidth: 1
                        implicitHeight: perk.implicitHeight + 20; radius: 9; color: Theme.canvas
                        ColumnLayout { id: perk; anchors.left: parent.left; anchors.right: parent.right; anchors.top: parent.top; anchors.margins: 10; spacing: 4
                            Text { text: modelData[0]; font.pixelSize: Theme.caption; font.weight: Font.Medium; color: Theme.secondary }
                            Text { text: Fmt.money(modelData[1]); font.pixelSize: Theme.title3; font.weight: Font.DemiBold; font.features: { "tnum": 1 }; color: Theme.invest; Sensitive { amounts: true } }
                            Text { text: qsTr("%1 Ausführungen").arg(modelData[2]); font.pixelSize: Theme.caption2; color: Theme.tertiary }
                            Text { text: modelData[3]; font.pixelSize: Theme.caption2; color: Theme.secondary; wrapMode: Text.WordWrap; Layout.fillWidth: true } }
                    }
                }
            }
        }
    }

    // FlowChartCard
    Card {
        id: flowCard
        Layout.fillWidth: true
        /// Largest bar in euros.
        readonly property real maxValue: { let m = 1; if (dash.d) for (const x of dash.d.monthly) m = Math.max(m, Math.abs(x.consumption_cents) / 100, Math.abs(x.investment_cents) / 100); return m }
        /// Axis (step 1, 2, 2.5 or 5 × 10^n for about four grid lines, like Swift Charts'
        /// automatic marks), computed in ONE binding from maxValue. As separate bindings,
        /// axisMax was once re-evaluated with the new maximum but the old step (0.25 € from
        /// the empty start state): 49,000 € / 0.25 € = 196,000 grid lines, minutes of work.
        readonly property var axis: {
            const max = maxValue
            if (!isFinite(max) || max <= 0) return { step: 1, max: 1, count: 2 }
            const raw = max / 4, mag = Math.pow(10, Math.floor(Math.log10(raw)))
            let step = 10 * mag
            for (const f of [1, 2, 2.5, 5, 10]) if (raw <= f * mag) { step = f * mag; break }
            const top = Math.ceil(max / step) * step
            return { step: step, max: top, count: Math.min(12, Math.round(top / step) + 1) }
        }
        readonly property real tickStep: axis.step
        readonly property real axisMax: axis.max
        /// Number of grid lines; a plain number as Repeater model (no rebuild for the same value).
        readonly property int tickCount: axis.count
        function tickValue(i) { return i * axis.step }
        ColumnLayout {
            width: parent.width; spacing: 8
            Text { text: qsTr("Konsum vs. Investition"); font.pixelSize: Theme.subheadline + 1; font.weight: Font.DemiBold; color: Theme.text }
            // Chart: bars per month, AxisMarks(position: .leading) with grid lines and labels.
            Item {
                id: chart
                Layout.fillWidth: true; Layout.preferredHeight: 140
                readonly property real plotTop: 6
                readonly property real plotBottom: height - 18
                readonly property real axisWidth: { let w = 0; for (let i = 0; i < flowCard.tickCount; ++i) w = Math.max(w, axisMetrics.advanceWidth(Fmt.count(flowCard.tickValue(i)))); return Math.ceil(w) + 6 }
                function yFor(euros) { return plotBottom - euros / flowCard.axisMax * (plotBottom - plotTop) }
                FontMetrics { id: axisMetrics; font.pixelSize: Theme.caption2 }
                Repeater {
                    model: flowCard.tickCount
                    Item {
                        required property int index
                        readonly property real value: flowCard.tickValue(index)
                        width: chart.width; height: 1
                        y: chart.yFor(value)
                        Text { anchors.verticalCenter: parent.verticalCenter; width: chart.axisWidth - 6; horizontalAlignment: Text.AlignRight
                               text: Fmt.count(parent.value); font.pixelSize: Theme.caption2; color: Theme.secondary; font.features: { "tnum": 1 }; Sensitive { amounts: true } }
                        Rectangle { x: chart.axisWidth; width: chart.width - chart.axisWidth; height: 1; color: Theme.border }
                    }
                }
                Row {
                    x: chart.axisWidth; width: chart.width - chart.axisWidth; height: chart.height
                    Repeater {
                        model: dash.d ? dash.d.monthly : []
                        Item {
                            required property var modelData
                            width: parent.width / Math.max(1, dash.d.monthly.length); height: chart.height
                            Row { y: chart.plotBottom - height; height: chart.plotBottom - chart.plotTop; anchors.horizontalCenter: parent.horizontalCenter; spacing: 4
                                Repeater { model: [[Math.abs(modelData.consumption_cents), Theme.tertiary], [Math.abs(modelData.investment_cents), Theme.invest]]
                                    Rectangle { required property var modelData; anchors.bottom: parent.bottom; width: 18; radius: 3; color: modelData[1]
                                                height: Math.max(1, modelData[0] / 100 / flowCard.axisMax * (chart.plotBottom - chart.plotTop)) } } }
                            Text { anchors.bottom: parent.bottom; anchors.horizontalCenter: parent.horizontalCenter; text: Fmt.monthLabel(modelData.month); font.pixelSize: Theme.caption; color: Theme.secondary }
                        }
                    }
                }
            }
            RowLayout { spacing: 14
                Repeater { model: [[Theme.tertiary, qsTr("Konsum")], [Theme.invest, qsTr("Investiert")]]
                    RowLayout { required property var modelData; spacing: 5
                        Rectangle { width: 8; height: 8; radius: 4; color: modelData[0] }
                        Text { text: modelData[1]; font.pixelSize: Theme.caption; color: Theme.secondary } } } }
        }
    }
    Text { text: qsTr("Investment-Buchungen"); font.pixelSize: Theme.subheadline + 1; font.weight: Font.DemiBold; color: Theme.secondary; Layout.topMargin: 4 }
}
