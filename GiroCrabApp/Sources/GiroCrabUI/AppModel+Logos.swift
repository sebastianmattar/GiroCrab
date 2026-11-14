import Foundation
import SwiftUI
import ImageIO
import GiroCrabCore

extension AppModel {
    // MARK: - Logos (Tier 2)

    /// Dual-source pipeline, stage 2: cached PNG/ICO from the vault or the
    /// Google S2 / DuckDuckGo favicon for the first candidate domain that
    /// yields a real logo (default globe rejected in Rust). Requests for the
    /// same merchant are shared; results pop into every row automatically.
    func logo(for merchant: MerchantInfo) async -> (image: Image, pixels: Int)? {
        let candidates = Self.logoCandidates(merchant)
        guard !candidates.isEmpty, let vault else { return nil }
        let key = candidates.joined(separator: "|")
        if let hit = logoCache[key] { return hit }
        if let running = logoInflight[key] { return await running.value }
        let online = onlineLogos
        let task = Task<(image: Image, pixels: Int)?, Never> {
            // Vault read (decryption) and PNG decoding off the main thread: a
            // signet scrolling into view must not cost a frame.
            let cached = try? await Self.offMain { () -> (image: Image, pixels: Int)? in
                for d in candidates {
                    if let data = try? vault.getMerchantLogoPng(domain: d) { return Self.decodeLogo(data) }
                }
                return nil
            }
            if let cached { return cached }
            guard online, let data = (try? await vault.fetchMerchantLogo(candidates: candidates))??.bytes else { return nil }
            return try? await Self.offMain { Self.decodeLogo(data) }
        }
        logoInflight[key] = task
        let result = await task.value
        logoInflight[key] = nil
        if let result { logoCache[key] = result }
        return result
    }

    /// The logo of `merchant` if it is already loaded – for a signet's first frame,
    /// so a row built while scrolling shows its logo at once instead of one
    /// frame later.
    func cachedLogo(for merchant: MerchantInfo) -> (image: Image, pixels: Int)? {
        let candidates = Self.logoCandidates(merchant)
        return candidates.isEmpty ? nil : logoCache[candidates.joined(separator: "|")]
    }

    private nonisolated static func logoCandidates(_ merchant: MerchantInfo) -> [String] {
        if merchant.domainCandidates.isEmpty && !merchant.domain.isEmpty { return [merchant.domain] }
        return merchant.domainCandidates
    }

    /// Decodes a favicon (PNG/ICO) into a ready-to-draw bitmap once: `NSImage`
    /// would decode lazily at first draw, i.e. on the main thread mid-scroll.
    /// `pixels` is the largest representation (the "sharp logo" threshold).
    nonisolated static func decodeLogo(_ data: Data) -> (image: Image, pixels: Int)? {
        guard let src = CGImageSourceCreateWithData(data as CFData, nil) else { return nil }
        var best = -1, bestWidth = 0
        for i in 0..<CGImageSourceGetCount(src) {
            let props = CGImageSourceCopyPropertiesAtIndex(src, i, nil) as? [CFString: Any]
            let w = props?[kCGImagePropertyPixelWidth] as? Int ?? 0
            if w > bestWidth || best < 0 { best = i; bestWidth = w }
        }
        guard best >= 0 else { return nil }
        let opts: [CFString: Any] = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceShouldCacheImmediately: true,
            kCGImageSourceThumbnailMaxPixelSize: 128,
        ]
        guard let cg = CGImageSourceCreateThumbnailAtIndex(src, best, opts as CFDictionary) else { return nil }
        return (image: Image(decorative: cg, scale: 1), pixels: bestWidth)
    }

    /// Kept for callers that only know a domain.
    func logo(for domain: String) async -> (image: Image, pixels: Int)? {
        guard !domain.isEmpty, let vault else { return nil }
        if let hit = logoCache[domain] { return hit }
        var data = try? vault.getMerchantLogoPng(domain: domain)
        if data == nil, onlineLogos { data = try? await vault.fetchLogo(domain: domain) }
        guard let data, let img = Self.image(from: data) else { return nil }
        let entry = (image: img, pixels: Self.pixelWidth(of: data))
        logoCache[domain] = entry
        return entry
    }

    /// Sets / clears the original loan amount ("Ursprünglicher Kreditbetrag").
    func setAccountInitialBalance(_ accountId: Int64, cents: Int64?) {
        guard let vault else { return }
        runCore { try vault.setAccountInitialBalance(accountId: accountId, cents: cents) }
        reloadAccounts()
    }

    /// Manual domain from the inspector; nil/empty removes the override.
    func setMerchantDomain(_ merchant: MerchantInfo, _ domain: String?) {
        guard let vault else { return }
        runCore {
            let stored = try vault.setMerchantDomain(merchantKey: merchant.key, domain: domain)
            toast = stored.map { String(localized: "Domain für \(merchant.name): \($0) – Logo wird geladen") } ?? String(localized: "Domain-Zuordnung entfernt")
        }
        logoCache = [:]
        logoVersion += 1
        reloadBookings()
        reloadMerchantStats()
    }

    static func pixelWidth(of data: Data) -> Int {
        #if os(macOS)
        return NSImage(data: data)?.representations.map(\.pixelsWide).max() ?? 0
        #else
        return UIImage(data: data).map { Int($0.size.width * $0.scale) } ?? 0
        #endif
    }

    static func image(from data: Data) -> Image? {
        #if os(macOS)
        NSImage(data: data).map { Image(nsImage: $0) }
        #else
        UIImage(data: data).map { Image(uiImage: $0) }
        #endif
    }

    func clearLogoCache() {
        guard let vault else { return }
        runCore { _ = try vault.clearLogoCache() }
        logoCache = [:]
        toast = String(localized: "Logo-Cache geleert")
    }
}
