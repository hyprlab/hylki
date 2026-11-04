//! A PDF as an assistant can use it: each page's text, and a page with none,
//! a scan most likely, as a picture. Base64 of the file is of no use to one.
//! Nothing here runs what a PDF holds: poppler only draws and reads it, the
//! way the attachment thumbnails do.

use gtk::cairo;

/// The most text handed over at a time.
const TEXT_MAX: usize = 256 * 1024;
/// The most pages handed over as pictures at a time, and their size together.
const PICTURES_MAX: usize = 5;
const PICTURE_BYTES_MAX: usize = 5 * 1024 * 1024;
/// A page's picture: about 150 dpi on A4, which small print survives.
const PICTURE_WIDTH: f64 = 1240.0;
const PICTURE_HEIGHT_MAX: f64 = 4000.0;
/// Fewer letters than this on a page is a page number or a header, not its
/// content: it is a picture of something.
const PAGE_TEXT_MIN: usize = 16;

/// Part of what a PDF holds, in page order.
#[derive(Debug, PartialEq)]
pub(super) enum Part {
    Text(String),
    /// A PNG.
    Picture(Vec<u8>),
}

/// What was read of a PDF, a stretch of its pages.
#[derive(Debug)]
pub(super) struct PdfRead {
    pub(super) parts: Vec<Part>,
    /// The pages read, from 1, and how many it has.
    pub(super) first: usize,
    pub(super) last: usize,
    pub(super) pages: usize,
}

/// Why a PDF was not read.
#[derive(Debug, PartialEq)]
pub(super) enum PdfError {
    /// It needs a password, or is damaged.
    CannotOpen,
    /// There is no such page: it has this many.
    NoSuchPage(usize),
}

/// Whether these bytes are a PDF: by what they hold, not what the sender
/// called them. The format lets the mark come a little way in.
pub(super) fn is_pdf(data: &[u8]) -> bool {
    data[..data.len().min(1024)]
        .windows(5)
        .any(|w| w == b"%PDF-")
}

/// Read from page `from` (from 1) as far as fits in one answer. Slow on a
/// big file, so it belongs on a thread that may wait.
pub(super) fn read(data: &[u8], from: usize) -> Result<PdfRead, PdfError> {
    let _guard = crate::ui::thumbnails::pdf_render_lock();
    let bytes = gtk::glib::Bytes::from(data);
    let doc = poppler::Document::from_bytes(&bytes, None).map_err(|_| PdfError::CannotOpen)?;
    let pages = usize::try_from(doc.n_pages()).unwrap_or(0);
    let first = from.max(1);
    if first > pages {
        return Err(PdfError::NoSuchPage(pages));
    }
    let mut parts: Vec<Part> = Vec::new();
    let mut last = first - 1;
    let (mut text_len, mut pictures, mut picture_bytes) = (0, 0, 0);
    for number in first..=pages {
        let Some(page) = i32::try_from(number - 1).ok().and_then(|i| doc.page(i)) else {
            break;
        };
        let text = page.text().map(|t| t.to_string()).unwrap_or_default();
        let letters = text.chars().filter(|c| !c.is_whitespace()).count();
        let picture = (letters < PAGE_TEXT_MIN)
            .then(|| page_picture(&page))
            .flatten();
        if let Some(png) = picture {
            let full = pictures >= PICTURES_MAX || picture_bytes + png.len() > PICTURE_BYTES_MAX;
            // At least one page goes back, however big, or nothing moves on.
            if full && number > first {
                break;
            }
            pictures += 1;
            picture_bytes += png.len();
            parts.push(Part::Text(format!(
                "--- Page {number} of {pages} (picture) ---"
            )));
            parts.push(Part::Picture(png));
        } else if letters > 0 {
            parts.push(Part::Text(format!(
                "--- Page {number} of {pages} ---\n{text}"
            )));
            text_len += text.len();
        }
        last = number;
        if text_len >= TEXT_MAX {
            break;
        }
    }
    Ok(PdfRead {
        parts: joined(parts),
        first,
        last,
        pages,
    })
}

/// Run the text between pictures together: a client shows each block apart.
fn joined(parts: Vec<Part>) -> Vec<Part> {
    let mut out: Vec<Part> = Vec::with_capacity(parts.len());
    for part in parts {
        match (out.last_mut(), part) {
            (Some(Part::Text(text)), Part::Text(more)) => {
                text.push_str("\n\n");
                text.push_str(&more);
            }
            (_, part) => out.push(part),
        }
    }
    out
}

/// A page drawn as a PNG; `None` when it comes out blank.
fn page_picture(page: &poppler::Page) -> Option<Vec<u8>> {
    let (w, h) = page.size();
    if w <= 0.0 || h <= 0.0 {
        return None;
    }
    let scale = (PICTURE_WIDTH / w).min(PICTURE_HEIGHT_MAX / h);
    let mut surface = cairo::ImageSurface::create(
        cairo::Format::ARgb32,
        (w * scale).round() as i32,
        (h * scale).round() as i32,
    )
    .ok()?;
    let cr = cairo::Context::new(&surface).ok()?;
    // A page's background is transparent.
    cr.set_source_rgb(1.0, 1.0, 1.0);
    cr.paint().ok()?;
    cr.scale(scale, scale);
    page.render(&cr);
    drop(cr);
    surface.flush();
    if surface.data().ok()?.iter().all(|b| *b == 0xff) {
        return None;
    }
    let mut png = Vec::new();
    surface.write_to_png(&mut png).ok()?;
    Some(png)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A PDF of these pages: each its text, if any, on a page that has a dark
    /// square on it when asked for. Made with cairo, as printing makes them.
    fn pdf(pages: &[(&str, bool)]) -> Vec<u8> {
        let dir = std::env::temp_dir().join(format!(
            "hylki-pdf-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("made.pdf");
        {
            let surface = cairo::PdfSurface::new(400.0, 500.0, &path).unwrap();
            let cr = cairo::Context::new(&surface).unwrap();
            for (text, square) in pages {
                if *square {
                    cr.set_source_rgb(0.2, 0.2, 0.2);
                    cr.rectangle(50.0, 50.0, 200.0, 200.0);
                    cr.fill().unwrap();
                }
                if !text.is_empty() {
                    cr.set_source_rgb(0.0, 0.0, 0.0);
                    cr.move_to(30.0, 300.0);
                    cr.show_text(text).unwrap();
                }
                cr.show_page().unwrap();
            }
            drop(cr);
            surface.finish();
        }
        let data = std::fs::read(&path).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
        data
    }

    fn text_of(read: &PdfRead) -> String {
        read.parts
            .iter()
            .filter_map(|p| match p {
                Part::Text(t) => Some(t.as_str()),
                Part::Picture(_) => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn a_pdf_with_text_comes_as_its_text() {
        let read = read(
            &pdf(&[
                ("The invoice comes to 412 euros", false),
                ("Paid in full by March", false),
            ]),
            1,
        )
        .unwrap();
        let text = text_of(&read);
        assert!(text.contains("The invoice comes to 412 euros"), "{text}");
        assert!(text.contains("--- Page 2 of 2 ---"), "{text}");
        assert!(text.contains("Paid in full by March"), "{text}");
        assert_eq!((read.first, read.last, read.pages), (1, 2, 2));
        assert_eq!(read.parts.len(), 1);
    }

    #[test]
    fn a_page_with_no_text_comes_as_a_picture_and_a_blank_one_not_at_all() {
        let data = pdf(&[
            ("A page that has words on it", false),
            ("", true),
            ("", false),
        ]);
        let read = read(&data, 1).unwrap();
        assert_eq!((read.first, read.last, read.pages), (1, 3, 3));
        let [Part::Text(text), Part::Picture(png)] = &read.parts[..] else {
            panic!("{:?}", read.parts.len());
        };
        assert!(text.contains("words on it") && text.contains("Page 2 of 3 (picture)"));
        assert!(png.starts_with(b"\x89PNG"));
    }

    #[test]
    fn a_page_can_be_asked_for_and_one_past_the_end_is_refused() {
        let data = pdf(&[
            ("The first of two pages", false),
            ("The second of two pages", false),
        ]);
        let second = read(&data, 2).unwrap();
        assert!(!text_of(&second).contains("first"));
        assert!(text_of(&second).contains("second"));
        assert_eq!(read(&data, 3).unwrap_err(), PdfError::NoSuchPage(2));
    }

    #[test]
    fn pictures_stop_at_the_limit_and_say_where() {
        let pages = vec![("", true); PICTURES_MAX + 2];
        let read = read(&pdf(&pages), 1).unwrap();
        assert_eq!(read.last, PICTURES_MAX);
        assert_eq!(read.pages, PICTURES_MAX + 2);
    }

    #[test]
    fn what_is_not_a_pdf_is_not_opened() {
        assert_eq!(
            read(b"%PDF-1.7 damaged", 1).unwrap_err(),
            PdfError::CannotOpen
        );
        assert!(is_pdf(&pdf(&[("x", false)])));
        assert!(!is_pdf(b"hello"));
    }
}
