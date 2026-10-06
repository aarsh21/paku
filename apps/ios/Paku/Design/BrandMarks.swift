// Pi harness mark, rendered natively from its SVG geometry. Model providers
// are not separate harnesses and never select a different mark.

import SwiftUI
import UIKit

enum BrandMark: Equatable {
    case pi

    var viewBox: CGSize { CGSize(width: 800, height: 800) }
    var evenOddFill: Bool { true }

    static func forHarness(_ harness: String) -> BrandMark? {
        switch harness {
        case "pi", "mock": return .pi
        default: return nil
        }
    }
}

/// The Pi SVG's three closed contours, including its even-odd cutout.
struct BrandMarkShape: Shape {
    let mark: BrandMark

    func path(in rect: CGRect) -> Path {
        var base = Path()
        base.move(to: CGPoint(x: 165.29, y: 165.29))
        for point in [CGPoint(x: 517.36, y: 165.29), CGPoint(x: 517.36, y: 400),
                      CGPoint(x: 400, y: 400), CGPoint(x: 400, y: 517.36),
                      CGPoint(x: 282.65, y: 517.36), CGPoint(x: 282.65, y: 634.72),
                      CGPoint(x: 165.29, y: 634.72)] {
            base.addLine(to: point)
        }
        base.closeSubpath()
        base.addRect(CGRect(x: 282.65, y: 282.65, width: 117.35, height: 117.35))
        base.addRect(CGRect(x: 517.36, y: 400, width: 117.36, height: 234.72))
        let box = mark.viewBox
        let scale = min(rect.width / box.width, rect.height / box.height)
        let dx = rect.minX + (rect.width - box.width * scale) / 2
        let dy = rect.minY + (rect.height - box.height * scale) / 2
        return base.applying(CGAffineTransform(scaleX: scale, y: scale)
            .concatenating(CGAffineTransform(translationX: dx, y: dy)))
    }
}

/// Cached monochrome templates, tinted by the caller. Unknown/removed
/// harnesses have no mark rather than silently displaying another brand.
enum BrandMarks {
    private static var cache: [CGFloat: UIImage] = [:]

    static func image(for harness: String?, side: CGFloat = 15) -> UIImage? {
        guard let harness, let mark = BrandMark.forHarness(harness), side > 0 else { return nil }
        if let hit = cache[side] { return hit }
        let rect = CGRect(x: 0, y: 0, width: side, height: side)
        let path = BrandMarkShape(mark: mark).path(in: rect).cgPath
        let image = UIGraphicsImageRenderer(size: rect.size).image { ctx in
            ctx.cgContext.addPath(path)
            ctx.cgContext.setFillColor(UIColor.black.cgColor)
            ctx.cgContext.fillPath(using: mark.evenOddFill ? .evenOdd : .winding)
        }.withRenderingMode(.alwaysTemplate)
        cache[side] = image
        return image
    }
}
