import SwiftUI
import GiroCrabCore

/// One inspector card for a booking, as placed by `InspectorLayout` (#448).
struct InspectorSectionCard: View {
    let section: InspectorSection
    let booking: Booking
    /// In the edit mode a card that would not show for this booking stands in as a placeholder.
    var placeholderWhenEmpty = false
    var scrollToNote: (() -> Void)?

    var body: some View {
        switch section {
        case .receipt: ReceiptCard(booking: booking)
        case .note: NoteCard(booking: booking, scrollToNote: scrollToNote)
        case .tags: TagsCard(booking: booking)
        case .salary:
            if booking.amountCents > 0 {
                SalaryCard(booking: booking)
            } else if placeholderWhenEmpty {
                InspectorPlaceholderCard(section: section, hint: "Nur bei Einnahmen sichtbar")
            }
        case .recurring: RecurringCard(booking: booking)
        case .merchantHistory: MerchantHistoryCard(booking: booking)
        case .categories: CategoryGridCard(booking: booking)
        case .documents: DocumentsCard(booking: booking)
        case .rawData: RawDataCard(booking: booking)
        }
    }
}

/// Bar at the top of the inspector (#448): „Anpassen“ switches to the edit mode, which shows
/// a hint, „Zurücksetzen“ once something changed, and „Fertig“.
struct InspectorLayoutBar: View {
    @Binding var editing: Bool
    let canReset: Bool
    let reset: () -> Void

    var body: some View {
        HStack(spacing: 12) {
            if editing {
                Text("Ziehen zum Verschieben")
                    .appFont(.caption)
                    .foregroundStyle(Theme.secondary)
                    .lineLimit(2)
            }
            Spacer(minLength: 0)
            if editing && canReset {
                Button("Zurücksetzen", action: reset)
                    .buttonStyle(.borderless)
                    .appFont(.caption)
                    .help(Text("Standard-Reihenfolge wiederherstellen und alle Bereiche einblenden"))
            }
            Button {
                withAnimation(.snappy(duration: 0.2)) { editing.toggle() }
            } label: {
                if editing {
                    Text("Fertig").appFont(.caption, weight: .semibold)
                } else {
                    Label("Anpassen", systemImage: "slider.horizontal.3").appFont(.caption)
                }
            }
            .buttonStyle(.borderless)
            .help(editing ? Text("Bearbeitung der Seitenleiste beenden") : Text("Reihenfolge der Bereiche ändern und Bereiche ausblenden"))
        }
        .padding(.horizontal, 16)
        .padding(.top, 8)
        .padding(.bottom, 4)
        .frame(maxWidth: .infinity)
        .background(Theme.canvas)
    }
}

/// Edit mode of the inspector: drag cards to reorder (blue insertion line), hide with ⊖,
/// show again with ⊕ from the „Ausgeblendet“ list below (#448).
struct InspectorLayoutEditor: View {
    let booking: Booking
    let layout: InspectorLayout
    let update: ((inout InspectorLayout) -> Void) -> Void

    /// Gap in the visible list where a drop would land (0 = above the first card).
    @State private var dropGap: Int?
    @State private var hiddenAreaTargeted = false

    var body: some View {
        let visible = layout.visible
        VStack(alignment: .leading, spacing: 14) {
            if visible.isEmpty {
                InspectorDropHint(text: "Bereiche hierher ziehen, um sie einzublenden", highlighted: dropGap == 0)
                    .dropDestination(for: String.self) { items, _ in
                        accept(items, gap: 0)
                    } isTargeted: { setTarget($0, gap: 0) }
            }
            ForEach(Array(visible.enumerated()), id: \.element) { index, section in
                InspectorEditableCard(
                    section: section, booking: booking, isHidden: false,
                    toggle: { update { $0.hide(section) } }
                ) {
                    // Upper half drops above this card, lower half below it.
                    VStack(spacing: 0) {
                        dropZone(gap: index)
                        dropZone(gap: index + 1)
                    }
                }
                .overlay(alignment: .top) {
                    if dropGap == index { InsertionLine().offset(y: -8.5) }
                }
                .overlay(alignment: .bottom) {
                    if index == visible.count - 1, dropGap == visible.count { InsertionLine().offset(y: 8.5) }
                }
                .accessibilityAction(named: Text("Nach oben")) {
                    if index > 0 { update { $0.move(section, toVisibleIndex: index - 1) } }
                }
                .accessibilityAction(named: Text("Nach unten")) {
                    if index < visible.count - 1 { update { $0.move(section, toVisibleIndex: index + 1) } }
                }
            }
            hiddenArea
        }
    }

    private var hiddenArea: some View {
        VStack(alignment: .leading, spacing: 14) {
            Text("Ausgeblendet")
                .appFont(.caption, weight: .semibold)
                .foregroundStyle(Theme.secondary)
                .padding(.top, 8)
            if layout.hiddenSections.isEmpty {
                InspectorDropHint(text: "Bereiche hierher ziehen oder mit ⊖ ausblenden", highlighted: hiddenAreaTargeted)
            }
            ForEach(layout.hiddenSections) { section in
                InspectorEditableCard(
                    section: section, booking: booking, isHidden: true,
                    toggle: { update { $0.show(section) } }
                ) { EmptyView() }
            }
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .contentShape(Rectangle())
        .dropDestination(for: String.self) { items, _ in
            guard let section = items.lazy.compactMap(InspectorSection.init(dragPayload:)).first else { return false }
            update { $0.hide(section) }
            return true
        } isTargeted: { hiddenAreaTargeted = $0 }
    }

    private func dropZone(gap: Int) -> some View {
        Color.clear
            .contentShape(Rectangle())
            .frame(maxWidth: .infinity, maxHeight: .infinity)
            .dropDestination(for: String.self) { items, _ in
                accept(items, gap: gap)
            } isTargeted: { setTarget($0, gap: gap) }
    }

    private func setTarget(_ targeted: Bool, gap: Int) {
        if targeted { dropGap = gap } else if dropGap == gap { dropGap = nil }
    }

    private func accept(_ items: [String], gap: Int) -> Bool {
        dropGap = nil
        guard let section = items.lazy.compactMap(InspectorSection.init(dragPayload:)).first else { return false }
        update { $0.insert(section, atGap: gap) }
        return true
    }
}

/// A card in the edit mode: content is not interactive, the whole card can be dragged, a round
/// ⊖ (visible) or ⊕ (hidden) button sits on its top-left corner. Hidden cards are drawn colourless,
/// faded and with a dashed frame, without changing their content.
private struct InspectorEditableCard<DropZones: View>: View {
    let section: InspectorSection
    let booking: Booking
    let isHidden: Bool
    let toggle: () -> Void
    @ViewBuilder let dropZones: () -> DropZones

    private var title: String { String(localized: section.title) }

    var body: some View {
        ZStack {
            InspectorSectionCard(section: section, booking: booking, placeholderWhenEmpty: true)
                .allowsHitTesting(false)
                .saturation(isHidden ? 0 : 1)
                .opacity(isHidden ? 0.45 : 1)
                .overlay {
                    if isHidden {
                        RoundedRectangle(cornerRadius: 12, style: .continuous)
                            .strokeBorder(Theme.tertiary, style: StrokeStyle(lineWidth: 1.5, dash: [5, 4]))
                    }
                }
            dropZones()
        }
        .contentShape(RoundedRectangle(cornerRadius: 12, style: .continuous))
        .draggable(section.dragPayload) {
            Label(title, systemImage: section.systemImage)
                .appFont(.callout, weight: .semibold)
                .padding(.horizontal, 12).padding(.vertical, 8)
                .background(Theme.card, in: RoundedRectangle(cornerRadius: 8, style: .continuous))
        }
        .overlay(alignment: .topLeading) {
            Button(action: { withAnimation(.snappy(duration: 0.2)) { toggle() } }) {
                Image(systemName: isHidden ? "plus.circle.fill" : "minus.circle.fill")
                    .font(.system(size: 20))
                    .symbolRenderingMode(.palette)
                    .foregroundStyle(.white, isHidden ? Theme.inflow : Theme.negative)
                    .shadow(color: .black.opacity(0.2), radius: 1, y: 0.5)
            }
            .buttonStyle(.borderless)
            .offset(x: -8, y: -8)
            .help(isHidden ? Text("„\(title)“ einblenden") : Text("„\(title)“ ausblenden"))
            .accessibilityLabel(isHidden ? Text("„\(title)“ einblenden") : Text("„\(title)“ ausblenden"))
        }
        .accessibilityElement(children: .contain)
        .accessibilityLabel(Text(title))
    }
}

/// Stand-in for a card that has nothing to show for the selected booking.
private struct InspectorPlaceholderCard: View {
    let section: InspectorSection
    let hint: LocalizedStringKey

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            Label { Text(section.title) } icon: { Image(systemName: section.systemImage) }
                .appFont(.subheadline, weight: .semibold)
                .foregroundStyle(Theme.text)
            Text(hint).appFont(.caption).foregroundStyle(Theme.secondary)
        }
        .card()
    }
}

/// Dashed drop area shown where nothing is listed yet.
private struct InspectorDropHint: View {
    let text: LocalizedStringKey
    let highlighted: Bool

    var body: some View {
        Text(text)
            .appFont(.caption)
            .foregroundStyle(Theme.secondary)
            .multilineTextAlignment(.center)
            .frame(maxWidth: .infinity, minHeight: 56)
            .background(
                RoundedRectangle(cornerRadius: 12, style: .continuous)
                    .fill(highlighted ? Theme.selection : Color.clear)
            )
            .overlay(
                RoundedRectangle(cornerRadius: 12, style: .continuous)
                    .strokeBorder(highlighted ? Theme.accent : Theme.border, style: StrokeStyle(lineWidth: 1.5, dash: [5, 4]))
            )
    }
}

/// Blue line marking where a dragged card will be inserted.
private struct InsertionLine: View {
    var body: some View {
        Capsule()
            .fill(Theme.accent)
            .frame(height: 3)
            .padding(.horizontal, 4)
            .allowsHitTesting(false)
            .accessibilityHidden(true)
    }
}

