//! The tray icon, as make_icon in asus_gpu_tray.py: the NVIDIA logo or a vendor badge, a purple dot
//! when an external GPU is attached, a red dot when a GPU is lost or the XG Mobile is unlocked.

use resvg::{tiny_skia, usvg};

use crate::gpu::pci::Gpu;

const SIZE: u32 = 64;
const NVIDIA_SVG: &str = include_str!("../../../icons/nvidia.svg");
const APP_SVG: &str = include_str!("../../../icons/asus-gpu-tray.svg");

thread_local! {
    static OPTIONS: usvg::Options<'static> = {
        let mut opt = usvg::Options::default();
        let db = opt.fontdb_mut();
        db.load_system_fonts();
        if let Some(family) = sans_family(db) {
            db.set_sans_serif_family(family);
        }
        opt
    };
}

/// The desktop's sans-serif font. fontdb maps sans-serif to Arial, which most Linux systems lack,
/// and text in a missing font is silently left out.
fn sans_family(db: &usvg::fontdb::Database) -> Option<String> {
    let installed = |name: &str| db.faces().any(|f| f.families.iter().any(|(n, _)| n == name));
    let fc = std::process::Command::new("fc-match")
        .args(["-f", "%{family[0]}", "sans-serif"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|f| !f.is_empty());
    fc.into_iter()
        .chain(["Noto Sans", "DejaVu Sans", "Liberation Sans", "Cantarell"].map(String::from))
        .find(|f| installed(f))
        .or_else(|| db.faces().next().and_then(|f| f.families.first().map(|(n, _)| n.clone())))
}

fn badge(vendor: Option<&str>) -> (&'static str, &'static str) {
    match vendor {
        Some("AMD") => ("AMD", "#ed1c24"),
        Some("Intel") => ("Intel", "#0071c5"),
        Some(_) => ("GPU", "#7f8c8d"),
        None => ("?", "#7f8c8d"),
    }
}

fn text_svg(label: &str, px: u32) -> String {
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{SIZE}" height="{SIZE}"><text x="32" y="32"
        font-family="sans-serif" font-weight="900" font-size="{px}" fill="white" text-anchor="middle"
        dominant-baseline="central">{label}</text></svg>"#
    )
}

/// The largest font size up to 44 px at which the label fits in 56 px, as the Python tray does.
fn fitting_size(label: &str, opt: &usvg::Options) -> u32 {
    (10..=44)
        .rev()
        .find(|&px| {
            usvg::Tree::from_str(&text_svg(label, px), opt).is_ok_and(|t| t.root().bounding_box().width() <= 56.0)
        })
        .unwrap_or(10)
}

fn content_svg(g: Option<&Gpu>, xg: bool, alert: bool, opt: &usvg::Options) -> String {
    let mut svg = format!(r#"<svg xmlns="http://www.w3.org/2000/svg" width="{SIZE}" height="{SIZE}">"#);
    if !g.is_some_and(|g| g.vendor == "NVIDIA") {
        let (label, bg) = badge(g.map(|g| g.vendor.as_str()));
        let px = fitting_size(label, opt);
        svg += &format!(
            r#"<rect x="1" y="1" width="62" height="62" rx="12" fill="{bg}"/><text x="32" y="32"
            font-family="sans-serif" font-weight="900" font-size="{px}" fill="white" text-anchor="middle"
            dominant-baseline="central">{label}</text>"#
        );
    }
    if xg {
        // purple dot = an external GPU is attached (XG Mobile mode / Thunderbolt eGPU)
        svg += r##"<circle cx="51" cy="13" r="12.5" fill="#9b59b6" stroke="white"/>"##;
    }
    if alert {
        // red dot = a GPU is lost or the XG Mobile is unlocked while in use
        svg += r##"<circle cx="49" cy="49" r="14.5" fill="#e74c3c" stroke="white"/>"##;
    }
    svg + "</svg>"
}

pub fn render(g: Option<&Gpu>, xg: bool, alert: bool) -> tiny_skia::Pixmap {
    let mut pix = tiny_skia::Pixmap::new(SIZE, SIZE).expect("non-zero size");
    OPTIONS.with(|opt| {
        if g.is_some_and(|g| g.vendor == "NVIDIA") {
            if let Ok(tree) = usvg::Tree::from_str(NVIDIA_SVG, opt) {
                let s = tree.size();
                let scale = tiny_skia::Transform::from_scale(SIZE as f32 / s.width(), SIZE as f32 / s.height());
                resvg::render(&tree, scale, &mut pix.as_mut());
            }
        }
        if let Ok(tree) = usvg::Tree::from_str(&content_svg(g, xg, alert, opt), opt) {
            resvg::render(&tree, tiny_skia::Transform::identity(), &mut pix.as_mut());
        }
    });
    pix
}

/// The application icon for windows, rendered at `size` px.
pub fn app_logo(size: u32) -> slint::Image {
    let mut pix = tiny_skia::Pixmap::new(size, size).expect("non-zero size");
    OPTIONS.with(|opt| {
        if let Ok(tree) = usvg::Tree::from_str(APP_SVG, opt) {
            let s = tree.size();
            let scale = tiny_skia::Transform::from_scale(size as f32 / s.width(), size as f32 / s.height());
            resvg::render(&tree, scale, &mut pix.as_mut());
        }
    });
    let rgba: Vec<u8> = pix
        .pixels()
        .iter()
        .flat_map(|p| {
            let c = p.demultiply();
            [c.red(), c.green(), c.blue(), c.alpha()]
        })
        .collect();
    slint::Image::from_rgba8(slint::SharedPixelBuffer::clone_from_slice(&rgba, size, size))
}

/// ARGB32 in network byte order, as StatusNotifierItem wants it.
pub fn to_argb(pix: &tiny_skia::Pixmap) -> Vec<u8> {
    pix.pixels()
        .iter()
        .flat_map(|p| {
            let c = p.demultiply();
            [c.alpha(), c.red(), c.green(), c.blue()]
        })
        .collect()
}

pub fn sni_icon(g: Option<&Gpu>, xg: bool, alert: bool) -> ksni::Icon {
    let pix = render(g, xg, alert);
    ksni::Icon { width: SIZE as i32, height: SIZE as i32, data: to_argb(&pix) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gpu::testutil::{egpu, igpu};

    fn rgb(pix: &tiny_skia::Pixmap, x: u32, y: u32) -> (u8, u8, u8) {
        let c = pix.pixel(x, y).unwrap().demultiply();
        (c.red(), c.green(), c.blue())
    }

    #[test]
    fn badges_and_dots() {
        let amd = render(Some(&igpu()), false, false);
        assert_eq!(rgb(&amd, 4, 32), (0xed, 0x1c, 0x24)); // red AMD badge
        let white =
            (16..48).flat_map(|y| (8..56).map(move |x| (x, y))).filter(|&(x, y)| rgb(&amd, x, y) == (255, 255, 255));
        assert!(white.count() > 50, "the AMD label is drawn");
        let nvidia = render(Some(&egpu()), true, true);
        assert_eq!(rgb(&nvidia, 4, 32), (0x76, 0xb9, 0x00)); // NVIDIA green
        assert_eq!(rgb(&nvidia, 51, 13), (0x9b, 0x59, 0xb6)); // eGPU dot
        assert_eq!(rgb(&nvidia, 49, 49), (0xe7, 0x4c, 0x3c)); // alert dot
        let none = render(None, false, false);
        assert_eq!(rgb(&none, 4, 32), (0x7f, 0x8c, 0x8d));
        assert_eq!(to_argb(&none).len(), (SIZE * SIZE * 4) as usize);
    }
}
