// TrustyConsoleSaverView — the bundled preview asset (#6839) and the native fallback.
//
// Split from `TrustyConsoleSaver.swift` for the 500-SLOC cap (#7856). That
// file's header carries the Why/What/Test for the whole view.

import AppKit
import Foundation
import ScreenSaver
import WebKit
import os.log

extension TrustyConsoleSaverView {
    // MARK: - Static preview asset (#6839)

    /// Basename of the PNG in `Contents/Resources/`, produced by
    /// `scripts/render-console-saver-preview.sh` and copied in by
    /// `scripts/build-console-saver.sh`.
    private static let previewAssetName = "ConsolePreview"
    /// How much of the asset shows through while offline. Dim enough that a
    /// photograph of last week's numbers cannot pass for live ones, bright
    /// enough that the screen is unmistakably the console and not a fault.
    static let offlineAssetFraction: CGFloat = 0.35

    static func loadPreviewAsset() -> NSImage? {
        let bundle = Bundle(for: TrustyConsoleSaverView.self)
        guard let url = bundle.url(forResource: previewAssetName, withExtension: "png"),
              let image = NSImage(contentsOf: url) else {
            os_log("preview asset %{public}@.png missing or unreadable — falling back to the wordmark",
                   log: saverLog, type: .error, previewAssetName)
            return nil
        }
        return image
    }

    /// Draws the asset scaled to fit, centred, at `fraction` opacity.
    /// Returns `false` when there is no usable asset, which is the caller's cue
    /// to draw the text wordmark instead.
    func drawPreviewAsset(fraction: CGFloat) -> Bool {
        guard let image = previewAsset else { return false }
        let source = image.size
        guard source.width > 0, source.height > 0, bounds.width > 0, bounds.height > 0 else {
            return false
        }
        // Fit, not fill: a screen saver that crops the dashboard's own edges
        // loses the header and the service table's last column.
        let scale = min(bounds.width / source.width, bounds.height / source.height)
        let drawn = NSSize(width: source.width * scale, height: source.height * scale)
        let target = NSRect(x: bounds.midX - drawn.width / 2,
                            y: bounds.midY - drawn.height / 2,
                            width: drawn.width,
                            height: drawn.height)
        image.draw(in: target, from: .zero, operation: .sourceOver, fraction: fraction)
        return true
    }

    /// The "offline" line #6838 asks for, over a scrim so it stays legible
    /// against whatever part of the dashboard sits behind it.
    func drawOfflineBanner() {
        let size = max(14, bounds.height * 0.035)
        let paragraph = NSMutableParagraphStyle()
        paragraph.alignment = .center

        let headline = NSAttributedString(
            string: "TRUSTY CONSOLE · OFFLINE",
            attributes: [
                .font: NSFont.monospacedSystemFont(ofSize: size, weight: .medium),
                .foregroundColor: Foundry.textPrimary,
                .kern: size * 0.18,
                .paragraphStyle: paragraph,
            ])
        let detail = NSAttributedString(
            string: "showing a saved preview — retrying",
            attributes: [
                .font: NSFont.monospacedSystemFont(ofSize: size * 0.55, weight: .regular),
                .foregroundColor: Foundry.textMuted,
                .kern: size * 0.10,
                .paragraphStyle: paragraph,
            ])

        let headlineSize = headline.size()
        let detailSize = detail.size()
        let stackHeight = headlineSize.height + detailSize.height + size * 0.6
        let band = NSRect(x: bounds.minX,
                          y: bounds.midY - stackHeight,
                          width: bounds.width,
                          height: stackHeight * 2)
        Foundry.background.withAlphaComponent(0.82).setFill()
        band.fill()

        headline.draw(at: NSPoint(x: bounds.midX - headlineSize.width / 2,
                                  y: bounds.midY + size * 0.2))
        detail.draw(at: NSPoint(x: bounds.midX - detailSize.width / 2,
                                y: bounds.midY - size * 0.2 - detailSize.height))
    }

    // MARK: - Native fallback

    /// The text card drawn when the bundled asset is missing — a broken build,
    /// not a state the operator should ever reach.
    func drawWordmark() {
        let size = max(14, bounds.height * 0.035)
        let font = NSFont.monospacedSystemFont(ofSize: size, weight: .medium)
        let paragraph = NSMutableParagraphStyle()
        paragraph.alignment = .center

        let text = NSMutableAttributedString(
            string: "TRUSTY CONSOLE",
            attributes: [
                .font: font,
                .foregroundColor: state == .preview ? Foundry.accent : Foundry.textPrimary,
                .kern: size * 0.18,
                .paragraphStyle: paragraph,
            ])
        if state == .offline {
            text.append(NSAttributedString(
                string: " · offline",
                attributes: [
                    .font: font,
                    .foregroundColor: Foundry.textMuted,
                    .kern: size * 0.18,
                    .paragraphStyle: paragraph,
                ]))
        }

        let drawn = text.size()
        let origin = NSPoint(x: bounds.midX - drawn.width / 2, y: bounds.midY - drawn.height / 2)
        text.draw(at: origin)
    }
}
