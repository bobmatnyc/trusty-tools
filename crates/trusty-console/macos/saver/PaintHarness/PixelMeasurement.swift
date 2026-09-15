// PaintHarness — reading and measuring the view's rendered bitmap.
//
// Split from `main.swift` for the 500-SLOC cap (#7856); that file's header
// carries the Why/What/Test for the whole harness.

import AppKit

// MARK: - Pixel measurement

struct PaintStats {
    let width: Int
    let height: Int
    /// Pixels brighter than near-black, as a fraction of the frame.
    let nonBlackRatio: Double
    /// Pixels differing from the flat Foundry background, as a fraction of the
    /// frame — the frame's drawn content.
    let inkRatio: Double

    var summary: String {
        String(format: "%dx%d nonBlack=%.4f ink=%.4f", width, height, nonBlackRatio, inkRatio)
    }
}

/// Read the view's own rendering, not a screenshot of the window: `cacheDisplay`
/// runs `draw(_:)` into the rep synchronously, so this captures the drawing code
/// and nothing about the compositor.
///
/// Split from [`stats`] so the first-paint clock can be stamped the moment the
/// frame EXISTS. Counting a million pixels is the harness's own cost and has no
/// business inside a latency budget the view is being judged against.
func capture(_ view: NSView) -> NSBitmapImageRep? {
    guard let rep = view.bitmapImageRepForCachingDisplay(in: view.bounds) else { return nil }
    view.cacheDisplay(in: view.bounds, to: rep)
    return rep
}

/// Decodes a captured rep into a tightly-packed sRGB RGBA buffer. Shared by the
/// whole-frame ratios and #6871's five edge samples so both read the same
/// pixels through the same colour space.
func pixels(of rep: NSBitmapImageRep) -> (width: Int, height: Int, buffer: [UInt8])? {
    guard let cgImage = rep.cgImage else { return nil }

    let width = cgImage.width
    let height = cgImage.height
    guard width > 0, height > 0 else { return nil }

    var buffer = [UInt8](repeating: 0, count: width * height * 4)
    let drew: Bool = buffer.withUnsafeMutableBytes { raw -> Bool in
        guard let base = raw.baseAddress,
              let space = CGColorSpace(name: CGColorSpace.sRGB),
              let context = CGContext(
                data: base,
                width: width,
                height: height,
                bitsPerComponent: 8,
                bytesPerRow: width * 4,
                space: space,
                bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)
        else { return false }
        context.draw(cgImage, in: CGRect(x: 0, y: 0, width: width, height: height))
        return true
    }
    guard drew else { return nil }
    return (width, height, buffer)
}

func stats(of rep: NSBitmapImageRep) -> PaintStats? {
    guard let (width, height, buffer) = pixels(of: rep) else { return nil }

    var nonBlack = 0
    var ink = 0
    var index = 0
    let total = width * height
    while index < total {
        let offset = index * 4
        let r = Int(buffer[offset])
        let g = Int(buffer[offset + 1])
        let b = Int(buffer[offset + 2])
        if max(r, max(g, b)) > nearBlackLevel { nonBlack += 1 }
        let distance = abs(r - backgroundRGB.r) + abs(g - backgroundRGB.g) + abs(b - backgroundRGB.b)
        if distance > 24 { ink += 1 }
        index += 1
    }

    return PaintStats(
        width: width,
        height: height,
        nonBlackRatio: Double(nonBlack) / Double(total),
        inkRatio: Double(ink) / Double(total))
}

/// #6871: one sample inside each corner plus the centre — the five points a web
/// view that failed to grow would leave unpainted along an edge.
///
/// This is the "never black" bar (#6838) restated at the target frame, and that
/// is ALL it is. `cacheDisplay` reads the view's own `draw(_:)`, and the live
/// page's background is the same `#201612` the view fills with, so these samples
/// cannot tell a correctly sized page from a letterboxed one. The frame equality
/// and the page-viewport check are what prove the fit.
func edgeSamples(of rep: NSBitmapImageRep) -> [(name: String, r: Int, g: Int, b: Int)]? {
    guard let (width, height, buffer) = pixels(of: rep) else { return nil }
    let inset = 8
    guard width > inset * 2, height > inset * 2 else { return nil }
    let points: [(String, Int, Int)] = [
        ("top-left", inset, inset),
        ("top-right", width - 1 - inset, inset),
        ("bottom-left", inset, height - 1 - inset),
        ("bottom-right", width - 1 - inset, height - 1 - inset),
        ("centre", width / 2, height / 2),
    ]
    return points.map { name, x, y in
        let offset = (y * width + x) * 4
        return (name, Int(buffer[offset]), Int(buffer[offset + 1]), Int(buffer[offset + 2]))
    }
}
