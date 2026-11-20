//! PaymentParser.swift › parseFile: the EPC QR code ("GiroCode") on a dropped invoice. The
//! Mac app renders the image or PDF (`image(from:isPDF:)`) and asks Vision for QR codes
//! (`qrPayloads`), then takes the first payload starting with "BCD". Here Qt renders the page
//! (cpp/qrscan.cpp, QImageReader; PDFs through Qt's PDF image plugin) and `rqrr` (pure Rust)
//! finds and decodes the codes. Runs on Core.callAsync's worker thread (`read_invoice`).

use std::os::raw::{c_int, c_uchar, c_void};

/// image(from:isPDF:): a PDF page is rendered at 3× its size in points.
const PDF_SCALE: f64 = 3.0;
/// Larger photos are scaled down first (a phone photo stays readable, the search stays fast).
const MAX_SIDE: c_int = 4096;
/// The Mac app looks at the first PDF page only; a GiroCode on a later page (e.g. the
/// payment slip at the end) is found here too, up to this page.
const MAX_PAGES: c_int = 10;
/// The second, sharper pass (bounded by MAX_SIDE: about 4.9× for A4) covers this many pages.
const SHARP_PAGES: usize = 2;
const SHARP_SCALE: f64 = 8.0;

type GraySink = extern "C" fn(
    ctx: *mut c_void,
    pixels: *const c_uchar,
    width: c_int,
    height: c_int,
    stride: c_int,
);

unsafe extern "C" {
    fn girocrab_scan_page(
        data: *const c_uchar,
        len: usize,
        index: c_int,
        pdf_scale: f64,
        max_side: c_int,
        ctx: *mut c_void,
        sink: GraySink,
    ) -> c_int;
}

/// An 8-bit grey image, rows without padding (0 = black, 255 = white).
struct Gray {
    width: usize,
    height: usize,
    pixels: Vec<u8>,
}

extern "C" fn collect_page(
    ctx: *mut c_void,
    pixels: *const c_uchar,
    width: c_int,
    height: c_int,
    stride: c_int,
) {
    let (Ok(w), Ok(h), Ok(stride)) = (
        usize::try_from(width),
        usize::try_from(height),
        usize::try_from(stride),
    ) else {
        return;
    };
    if pixels.is_null() || stride < w {
        return;
    }
    // SAFETY: `ctx` is the `Option<Gray>` that `epc_payload` passes for this call;
    // `pixels` holds `height` rows of `stride` bytes (QImage::constBits, bytesPerLine).
    let out = unsafe { &mut *ctx.cast::<Option<Gray>>() };
    let rows = unsafe { std::slice::from_raw_parts(pixels, stride * h) };
    let mut data = Vec::with_capacity(w * h);
    for row in rows.chunks(stride) {
        data.extend_from_slice(&row[..w]);
    }
    *out = Some(Gray {
        width: w,
        height: h,
        pixels: data,
    });
}

/// The first "BCD" payload (EPC QR) in the image or PDF `data`, page by page; `None` when
/// Qt cannot read the data or no page carries a GiroCode. A small code (about 2 cm) is too
/// coarse at 3×: when no page had one, the first PDF pages are scanned again, sharper.
pub fn epc_payload(data: &[u8]) -> Option<String> {
    let mut sizes = Vec::new();
    let mut index = 0;
    loop {
        let (count, page) = render(data, index, PDF_SCALE);
        if count < 0 && index == 0 {
            return None;
        }
        if let Some(g) = page {
            sizes.push((g.width, g.height));
            if let Some(found) = first_epc(&g) {
                return Some(found);
            }
        }
        index += 1;
        if count < 0 || index >= count.min(MAX_PAGES) {
            break;
        }
    }
    for (index, size) in sizes.into_iter().take(SHARP_PAGES).enumerate() {
        let (_, page) = render(data, index as c_int, SHARP_SCALE);
        // a photo (or a page already at MAX_SIDE) comes back unchanged: nothing new to find
        if let Some(g) = page.filter(|g| (g.width, g.height) != size)
            && let Some(found) = first_epc(&g)
        {
            return Some(found);
        }
    }
    None
}

/// Page `index` of `data` as grey pixels, with the page count (-1: unreadable).
fn render(data: &[u8], index: c_int, pdf_scale: f64) -> (c_int, Option<Gray>) {
    let mut page: Option<Gray> = None;
    // SAFETY: `data` outlives the call; `collect_page` gets `&mut page` back as `ctx`.
    let count = unsafe {
        girocrab_scan_page(
            data.as_ptr(),
            data.len(),
            index,
            pdf_scale,
            MAX_SIDE,
            (&mut page as *mut Option<Gray>).cast(),
            collect_page,
        )
    };
    (count, page)
}

fn first_epc(g: &Gray) -> Option<String> {
    payloads(g.width, g.height, &g.pixels)
        .into_iter()
        .find(|p| p.starts_with("BCD"))
}

/// qrPayloads(_:): the text of every QR code in a grey image. A payload that is no UTF-8
/// (EPC character set 2–8, e.g. ISO 8859-1) is read as Latin-1.
pub fn payloads(width: usize, height: usize, pixels: &[u8]) -> Vec<String> {
    if width == 0 || height == 0 || pixels.len() < width * height {
        return Vec::new();
    }
    let mut img =
        rqrr::PreparedImage::prepare_from_greyscale(width, height, |x, y| pixels[y * width + x]);
    img.detect_grids()
        .into_iter()
        .filter_map(|grid| {
            let mut bytes = Vec::new();
            grid.decode_to(&mut bytes).ok()?;
            Some(
                String::from_utf8(bytes)
                    .unwrap_or_else(|e| e.into_bytes().iter().map(|&b| char::from(b)).collect()),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A QR code of `payload` as a grey image: `scale` pixels per module (quiet zone included).
    fn qr_image(payload: &str, scale: usize) -> (usize, Vec<u8>) {
        let code = girocrab_core::payment_input::girocode_matrix(payload.to_owned()).unwrap();
        let modules = code.width as usize;
        let side = modules * scale;
        let mut px = vec![255u8; side * side];
        for (i, &dark) in code.modules.iter().enumerate() {
            if !dark {
                continue;
            }
            let (mx, my) = (i % modules, i / modules);
            for y in my * scale..(my + 1) * scale {
                px[y * side + mx * scale..y * side + (mx + 1) * scale].fill(0);
            }
        }
        (side, px)
    }

    const EPC: &str = "BCD\n002\n1\nSCT\nCOBADEFFXXX\nMax Müller\nDE89370400440532013000\nEUR123.45\n\n\nRechnung 2024-17";

    #[test]
    fn decodes_epc_payload() {
        let (side, px) = qr_image(EPC, 6);
        assert_eq!(payloads(side, side, &px), vec![EPC.to_owned()]);
    }

    #[test]
    fn decodes_epc_payload_through_qt() {
        // a PGM (Qt reads it without plugins) of the code, rendered and scanned like a drop
        let (side, px) = qr_image(EPC, 4);
        let mut pgm = format!("P5\n{side} {side}\n255\n").into_bytes();
        pgm.extend_from_slice(&px);
        assert_eq!(epc_payload(&pgm).as_deref(), Some(EPC));
    }

    #[test]
    fn no_code_no_payload() {
        assert!(payloads(64, 64, &[255u8; 64 * 64]).is_empty());
        assert_eq!(epc_payload(b"no image at all"), None);
    }
}
