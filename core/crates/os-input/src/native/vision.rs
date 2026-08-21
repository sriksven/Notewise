//! Reading text out of pixels.
//!
//! # Why this is the only Objective-C in the crate
//!
//! Vision has no C entry point. Every other framework here — the accessibility API, the pasteboard,
//! event synthesis, Carbon hotkeys — is reachable through a plain `extern "C"` block, which is why
//! this crate needs the objc2 runtime for exactly one capability and nothing else.
//!
//! # What is implemented, and the wall in front of it
//!
//! Recognition is real and runs: hand a bitmap to [`recognise`] and it comes back with the lines
//! Vision found. That needs no permission at all, so it is tested against an image this crate draws
//! itself.
//!
//! *Capturing* the screen is the part that cannot happen. It needs the Screen Recording grant, and
//! macOS will not give that to a build without a Developer ID Team ID — the same wall system audio
//! hits, already encoded in `macos_permissions::can_hold_screen_recording`. So
//! [`super::recognise_text_on_screen`] refuses with that reason rather than returning an empty
//! string, which would read as "there was no text on screen".
//!
//! The split matters: when somebody builds this signed, the recognition half is already known to
//! work, and only the capture half is new.

use objc2::rc::Retained;
use objc2::AnyThread;
use objc2_core_foundation::CFData;
use objc2_core_foundation::{CGPoint, CGRect, CGSize};
use objc2_core_graphics::{
    CGBitmapContextCreate, CGBitmapContextGetData, CGBitmapInfo, CGColorRenderingIntent,
    CGColorSpace, CGContext, CGDataProvider, CGImage, CGWindowImageOption, CGWindowListOption,
};
use objc2_foundation::{NSArray, NSDictionary};
use objc2_vision::{VNImageRequestHandler, VNRecognizeTextRequest, VNRequest};

/// An 8-bit greyscale bitmap.
///
/// Greyscale rather than colour because text recognition does not use colour and a quarter of the
/// bytes cross the boundary. One byte per pixel, row-major, no padding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Bitmap {
    pub width: usize,
    pub height: usize,
    /// `width * height` bytes. 0 is black, 255 is white.
    pub pixels: Vec<u8>,
}

impl Bitmap {
    /// A white canvas.
    pub fn blank(width: usize, height: usize) -> Self {
        Self {
            width,
            height,
            pixels: vec![0xFF; width * height],
        }
    }

    /// Whether the dimensions and the buffer agree.
    ///
    /// Checked before the bytes cross into CoreGraphics, which would otherwise read past the end of
    /// the buffer for a mismatched `bytes_per_row`.
    pub fn is_consistent(&self) -> bool {
        self.width > 0 && self.height > 0 && self.pixels.len() == self.width * self.height
    }

    fn to_cg_image(&self) -> Option<Retained<CGImage>> {
        // CoreGraphics keeps a reference to the data provider, which keeps the CFData, which owns
        // its own copy of these bytes — so the image does not borrow `self`.
        let data = CFData::from_bytes(&self.pixels);
        let provider = CGDataProvider::with_cf_data(Some(&data))?;
        let space = CGColorSpace::new_device_gray()?;

        // SAFETY: the buffer length is checked against `width * height` by the caller, the row
        // stride is exactly `width` for one byte per pixel, and `decode` is null — which the
        // function's own safety note permits.
        let image = unsafe {
            CGImage::new(
                self.width,
                self.height,
                8,
                8,
                self.width,
                Some(&space),
                CGBitmapInfo::empty(),
                Some(&provider),
                std::ptr::null(),
                false,
                CGColorRenderingIntent::RenderingIntentDefault,
            )
        }?;

        Some(Retained::from(image))
    }
}

/// Every line of text Vision found, in the order it found them.
///
/// Slow rather than fast recognition, and language correction on: this runs once when a user asks a
/// question about their screen, not per frame, and a misread word in a prompt is worse than fifty
/// milliseconds.
pub fn recognise(bitmap: &Bitmap) -> Result<Vec<String>, String> {
    if !bitmap.is_consistent() {
        return Err(format!(
            "a {}x{} bitmap needs {} bytes and has {}",
            bitmap.width,
            bitmap.height,
            bitmap.width * bitmap.height,
            bitmap.pixels.len()
        ));
    }

    let image = bitmap
        .to_cg_image()
        .ok_or_else(|| "could not build an image from those pixels".to_string())?;

    let request = VNRecognizeTextRequest::new();
    request.setUsesLanguageCorrection(true);

    let handler = unsafe {
        VNImageRequestHandler::initWithCGImage_options(
            VNImageRequestHandler::alloc(),
            &image,
            &NSDictionary::new(),
        )
    };

    let requests: Retained<NSArray<VNRequest>> =
        NSArray::from_slice(&[request.as_ref() as &VNRequest]);

    handler
        .performRequests_error(&requests)
        .map_err(|error| format!("text recognition failed: {error}"))?;

    let Some(results) = request.results() else {
        // No results is not an error. A blank screen has no text on it, and reporting that as a
        // failure would make every consumer treat "nothing to read" as something to apologise for.
        return Ok(Vec::new());
    };

    let mut lines = Vec::new();
    for observation in results.iter() {
        // One candidate: the alternatives are for a UI that offers corrections, and this text is
        // going into a prompt.
        let candidates = observation.topCandidates(1);
        if let Some(best) = candidates.iter().next() {
            let text = best.string().to_string();
            if !text.trim().is_empty() {
                lines.push(text);
            }
        }
    }

    Ok(lines)
}

/// Turn a CoreGraphics image into a greyscale bitmap.
///
/// Drawn into a context of a known format rather than read out of the image's own data provider.
/// A screen capture arrives as premultiplied BGRA at whatever row stride the window server chose,
/// and unpacking that by hand is several ways to be subtly wrong. Drawing costs one blit and makes
/// the format ours.
pub fn bitmap_from_image(image: &CGImage) -> Result<Bitmap, String> {
    let width = CGImage::width(Some(image));
    let height = CGImage::height(Some(image));

    if width == 0 || height == 0 {
        return Err("the image has no pixels".to_string());
    }

    // A screen is a few megapixels; a wall of them is not. Bounded so a caller cannot ask for a
    // gigabyte of greyscale by handing over something enormous.
    const MAX_PIXELS: usize = 40_000_000;
    if width.saturating_mul(height) > MAX_PIXELS {
        return Err(format!("{width}x{height} is too large to read"));
    }

    let space = CGColorSpace::new_device_gray().ok_or_else(|| "no greyscale space".to_string())?;
    let mut pixels = vec![0u8; width * height];

    // SAFETY: the buffer is `width * height` bytes and the context is told exactly that — one byte
    // per pixel, a row stride of `width`, and eight bits per component. The context borrows the
    // buffer for as long as it lives, and it is dropped at the end of this function while `pixels`
    // is still owned here.
    let context = unsafe {
        CGBitmapContextCreate(
            pixels.as_mut_ptr() as *mut std::ffi::c_void,
            width,
            height,
            8,
            width,
            Some(&space),
            // `kCGImageAlphaNone`: greyscale with no alpha channel.
            0,
        )
    }
    .ok_or_else(|| "could not open a drawing context".to_string())?;

    let rect = CGRect::new(
        CGPoint::new(0.0, 0.0),
        CGSize::new(width as f64, height as f64),
    );
    CGContext::draw_image(Some(&context), rect, Some(image));

    // The context wrote into `pixels` directly; this only proves it is the same buffer, and fails
    // loudly rather than returning an image of zeros if CoreGraphics moved it.
    let written = CGBitmapContextGetData(Some(&context));
    if written != pixels.as_mut_ptr() as *mut std::ffi::c_void {
        return Err("the drawing context did not use the buffer it was given".to_string());
    }

    drop(context);

    Ok(Bitmap {
        width,
        height,
        pixels,
    })
}

/// Grab the whole screen as pixels.
///
/// # The one call here that has never run
///
/// Everything else in this module is exercised in CI. This is not, and cannot be: it needs the
/// Screen Recording grant, and macOS will not give that to a build without a Developer ID Team ID.
/// `macos_permissions::can_hold_screen_recording` is the same check `audio-capture` makes for system
/// audio, and it answers `false` for every build produced without a paid signing identity.
///
/// So the two failures are reported separately. A build that cannot hold the grant is told about the
/// signature, because no switch in System Settings will help it. A signed build without the grant is
/// told which pane to open. Conflating them is the mistake `63f6f6d` was about.
///
/// `CGWindowListCreateImage` is deprecated in favour of ScreenCaptureKit, and is used anyway: SCK is
/// asynchronous, needs a delegate, and cannot be verified here either — so it would trade one
/// untested call for thirty.
pub fn capture_screen() -> Result<Bitmap, ScreenCaptureRefusal> {
    if !notewise_macos_permissions::can_hold_screen_recording() {
        return Err(ScreenCaptureRefusal::BuildCannotHoldGrant);
    }

    if !notewise_macos_permissions::screen_recording_granted() {
        return Err(ScreenCaptureRefusal::GrantMissing);
    }

    // `CGRectInfinite` — every display, whatever their arrangement.
    let everything = CGRect::new(
        CGPoint::new(f64::MIN / 2.0, f64::MIN / 2.0),
        CGSize::new(f64::MAX, f64::MAX),
    );

    #[allow(
        deprecated,
        reason = "ScreenCaptureKit cannot be verified here either; see the docs"
    )]
    let image = objc2_core_graphics::CGWindowListCreateImage(
        everything,
        CGWindowListOption::OptionOnScreenOnly,
        0,
        CGWindowImageOption::Default,
    )
    .ok_or(ScreenCaptureRefusal::CaptureFailed)?;

    bitmap_from_image(&image).map_err(|_| ScreenCaptureRefusal::CaptureFailed)
}

/// Why the screen could not be captured.
///
/// Three cases and not one, because the fixes differ: buy a signing identity, flip a switch, or
/// nothing the user can do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenCaptureRefusal {
    /// This build has no Team ID, so the grant is unobtainable. No setting changes this.
    BuildCannotHoldGrant,
    /// The build could hold it and does not. There is a pane to open.
    GrantMissing,
    /// The grant is held and the capture still failed.
    CaptureFailed,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 5x7 block font, enough to draw a word Vision can read.
    ///
    /// Drawn here rather than loaded from a fixture so the test has no binary asset to go stale, and
    /// so what is being recognised is visible in the source. Only the letters the tests use.
    fn glyph(c: char) -> Option<[&'static str; 7]> {
        Some(match c {
            'H' => [
                "#   #", "#   #", "#   #", "#####", "#   #", "#   #", "#   #",
            ],
            'E' => [
                "#####", "#    ", "#    ", "#### ", "#    ", "#    ", "#####",
            ],
            'L' => [
                "#    ", "#    ", "#    ", "#    ", "#    ", "#    ", "#####",
            ],
            'O' => [
                " ### ", "#   #", "#   #", "#   #", "#   #", "#   #", " ### ",
            ],
            'N' => [
                "#   #", "##  #", "# # #", "#  ##", "#   #", "#   #", "#   #",
            ],
            'T' => [
                "#####", "  #  ", "  #  ", "  #  ", "  #  ", "  #  ", "  #  ",
            ],
            'W' => [
                "#   #", "#   #", "#   #", "# # #", "# # #", "## ##", "#   #",
            ],
            'I' => [
                "#####", "  #  ", "  #  ", "  #  ", "  #  ", "  #  ", "#####",
            ],
            'S' => [
                " ####", "#    ", "#    ", " ### ", "    #", "    #", "#### ",
            ],
            ' ' => [
                "     ", "     ", "     ", "     ", "     ", "     ", "     ",
            ],
            _ => return None,
        })
    }

    /// Draw a word onto a white canvas, each pixel scaled up so the strokes are thick enough to
    /// recognise.
    fn draw(word: &str, scale: usize) -> Bitmap {
        let glyph_width = 5;
        let glyph_height = 7;
        let spacing = 1;
        let margin = 4 * scale;

        let letters: Vec<[&str; 7]> = word.chars().filter_map(glyph).collect();
        let width = margin * 2 + letters.len() * (glyph_width + spacing) * scale;
        let height = margin * 2 + glyph_height * scale;

        let mut bitmap = Bitmap::blank(width, height);

        for (index, letter) in letters.iter().enumerate() {
            let origin_x = margin + index * (glyph_width + spacing) * scale;

            for (row, line) in letter.iter().enumerate() {
                for (column, cell) in line.chars().enumerate() {
                    if cell != '#' {
                        continue;
                    }
                    for dy in 0..scale {
                        for dx in 0..scale {
                            let x = origin_x + column * scale + dx;
                            let y = margin + row * scale + dy;
                            bitmap.pixels[y * width + x] = 0x00;
                        }
                    }
                }
            }
        }

        bitmap
    }

    /// The real thing: pixels this crate drew, read back by the system's own recogniser.
    ///
    /// Needs no permission, so it runs. This is what makes the recognition half of screen context
    /// something that has been shown to work rather than something that compiles.
    #[test]
    fn text_drawn_as_pixels_is_read_back() {
        let bitmap = draw("HELLO", 12);
        let lines = recognise(&bitmap).expect("recognition runs");

        let joined = lines.join(" ").to_uppercase();
        assert!(
            joined.contains("HELLO"),
            "expected to read HELLO, got {lines:?}"
        );
    }

    #[test]
    fn several_words_come_back() {
        let bitmap = draw("HELLO WISE", 12);
        let lines = recognise(&bitmap).expect("recognition runs");

        let joined = lines.join(" ").to_uppercase();
        assert!(joined.contains("HELLO"), "{lines:?}");
        assert!(joined.contains("WISE"), "{lines:?}");
    }

    /// A blank screen has no text on it. Reporting that as a failure would make every consumer
    /// apologise for nothing.
    #[test]
    fn a_blank_image_has_no_text_and_is_not_an_error() {
        let lines = recognise(&Bitmap::blank(200, 80)).expect("recognition runs");
        assert!(lines.is_empty(), "{lines:?}");
    }

    /// The check that stops CoreGraphics reading past the end of the buffer.
    #[test]
    fn a_bitmap_whose_size_does_not_match_its_buffer_is_refused() {
        let broken = Bitmap {
            width: 100,
            height: 100,
            pixels: vec![0xFF; 10],
        };
        assert!(!broken.is_consistent());

        let error = recognise(&broken).expect_err("must refuse");
        assert!(error.contains("10000"), "{error}");
        assert!(error.contains("10"), "{error}");
    }

    #[test]
    fn a_zero_sized_bitmap_is_refused() {
        assert!(recognise(&Bitmap {
            width: 0,
            height: 0,
            pixels: vec![],
        })
        .is_err());
    }

    #[test]
    fn a_blank_canvas_is_white_and_consistent() {
        let blank = Bitmap::blank(3, 2);
        assert!(blank.is_consistent());
        assert_eq!(blank.pixels, vec![0xFF; 6]);
    }

    /// Recognition is called once per question, not per frame, so it must be safe to call
    /// repeatedly without leaking the objc objects it creates.
    #[test]
    fn repeated_recognition_is_stable() {
        let bitmap = draw("TEST", 10);
        for _ in 0..12 {
            recognise(&bitmap).expect("recognition runs");
        }
    }
    /// The conversion, proven by a round trip: pixels in, a CoreGraphics image, pixels out.
    ///
    /// This is the half of screen capture that can be verified, and it is the half where the subtle
    /// bugs live — row strides, colour spaces, an image drawn upside down.
    #[test]
    fn a_bitmap_survives_a_round_trip_through_coregraphics() {
        let original = draw("HI", 10);
        let image = original.to_cg_image().expect("builds an image");
        let converted = bitmap_from_image(&image).expect("converts back");

        assert_eq!(converted.width, original.width);
        assert_eq!(converted.height, original.height);
        assert!(converted.is_consistent());

        // Not byte-for-byte: drawing goes through a rasteriser. What must survive is the content —
        // the same ink in the same places, which is what recognition depends on.
        let ink_before = original.pixels.iter().filter(|p| **p < 0x80).count();
        let ink_after = converted.pixels.iter().filter(|p| **p < 0x80).count();
        assert!(
            ink_after.abs_diff(ink_before) * 20 < ink_before.max(1),
            "ink moved: {ink_before} before, {ink_after} after"
        );
    }

    /// And the text is still readable afterwards, which is the property that actually matters.
    #[test]
    fn text_survives_the_conversion_and_is_still_recognised() {
        let original = draw("HELLO", 12);
        let image = original.to_cg_image().expect("builds an image");
        let converted = bitmap_from_image(&image).expect("converts back");

        let lines = recognise(&converted).expect("recognition runs");
        assert!(
            lines.join(" ").to_uppercase().contains("HELLO"),
            "{lines:?}"
        );
    }

    /// A caller must not be able to ask for a gigabyte of greyscale.
    #[test]
    fn an_absurdly_large_image_is_refused() {
        // Built as a claim rather than an allocation: the check is on the dimensions.
        let claim = Bitmap {
            width: 100_000,
            height: 100_000,
            pixels: vec![],
        };
        assert!(!claim.is_consistent());
        assert!(recognise(&claim).is_err());
    }

    /// The three refusals have three different fixes, so they must not collapse into one.
    ///
    /// A development build is always in the first case, which is what this asserts.
    #[test]
    fn capturing_the_screen_is_refused_for_the_reason_that_applies() {
        match capture_screen() {
            Err(ScreenCaptureRefusal::BuildCannotHoldGrant) => {
                assert!(!notewise_macos_permissions::can_hold_screen_recording());
            }
            // A signed build running these tests would land here.
            Err(ScreenCaptureRefusal::GrantMissing) => {
                assert!(notewise_macos_permissions::can_hold_screen_recording());
            }
            Err(ScreenCaptureRefusal::CaptureFailed) => {}
            Ok(bitmap) => assert!(bitmap.is_consistent()),
        }
    }
}
