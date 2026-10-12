//! The standard 14 text fonts' Latin glyphs: which characters Helvetica, Times and Courier
//! have, each one's glyph name and its advance.
//!
//! These faces hold more than WinAnsiEncoding reaches: the Central European, Baltic, Turkish
//! and Romanian letters among them. [`extended_code`] gives such a character a code in a second
//! encoding ([`extended_encoding`], [`extended_to_unicode`]), so text in the standard fonts can
//! show it without an embedded font program.
//!
//! The names and widths are the published standard-14 metrics (ISO 32000-2 §9.6.2.2). They are
//! read from the tables the vendored `hayro-interpret` carries (the one place AGENTS.md §1.1
//! allows them), not copied here.

use std::sync::OnceLock;

use hayro_interpret::font::StandardFont;

/// The name of the glyph the standard font `base_font` (`Times-Bold`, …) has for `c`, and its
/// advance in thousandths of the font size. `None` for a character the font has no glyph for,
/// for Symbol and ZapfDingbats, and for a name that is no standard font's.
pub fn glyph(base_font: &str, c: char) -> Option<(&'static str, u16)> {
    let (name, width) = StandardFont::from_postscript_name(base_font)?.glyph_for_char(c)?;
    // Widths are whole numbers well below a thousand ems; anything else is not a width.
    (width.is_finite() && (0.0..=10_000.0).contains(&width)).then(|| (name, width.round() as u16))
}

/// The advance of `c` in the standard font `base_font`, in thousandths of the font size.
pub fn advance(base_font: &str, c: char) -> Option<u16> {
    glyph(base_font, c).map(|(_, w)| w)
}

/// Whether the standard text fonts have a glyph for `c` (they share one glyph set).
pub fn has_glyph(c: char) -> bool {
    glyph("Helvetica", c).is_some()
}

/// Where the standard fonts' glyphs outside WinAnsiEncoding are: Latin-1 and Latin Extended,
/// the spacing accents, punctuation and symbols, the geometric shapes and the ligatures.
const EXTENDED_RANGES: [std::ops::RangeInclusive<u32>; 5] = [0xA0..=0x24F, 0x2B0..=0x2FF, 0x2000..=0x22FF, 0x25A0..=0x25FF, 0xFB00..=0xFB06];

/// The glyphs WinAnsiEncoding doesn't reach, in character order, with their names: the extended
/// encoding gives them the codes from [`EXTENDED_FIRST`] up.
fn extended() -> &'static [(char, &'static str)] {
    static EXTENDED: OnceLock<Vec<(char, &'static str)>> = OnceLock::new();
    EXTENDED.get_or_init(|| {
        EXTENDED_RANGES
            .into_iter()
            .flatten()
            .filter_map(char::from_u32)
            .filter(|c| crate::win_ansi_byte(*c).is_none())
            .filter_map(|c| glyph("Helvetica", c).map(|(name, _)| (c, name)))
            // One byte holds the codes: the fonts have about a hundred such glyphs (a test
            // checks that none is cut off here).
            .take(256 - usize::from(EXTENDED_FIRST))
            .collect()
    })
}

/// The first code of the extended encoding (the codes below it are WinAnsiEncoding's).
pub const EXTENDED_FIRST: u8 = 0x21;

/// The code of `c` in the extended encoding: `None` when WinAnsiEncoding has the character
/// (use that) or the standard fonts have no glyph for it.
pub fn extended_code(c: char) -> Option<u8> {
    let at = extended().binary_search_by_key(&c, |g| g.0).ok()?;
    u8::try_from(usize::from(EXTENDED_FIRST).checked_add(at)?).ok()
}

/// The extended encoding as a font's `/Encoding`: WinAnsiEncoding with `/Differences` naming
/// the glyphs of the codes [`extended_code`] gives.
pub fn extended_encoding() -> pdfcraft_cos::Dict {
    use pdfcraft_cos::Object;
    let mut differences = vec![Object::Int(i64::from(EXTENDED_FIRST))];
    differences.extend(extended().iter().map(|g| Object::name(g.1)));
    let mut d = pdfcraft_cos::Dict::new();
    d.set(b"Type".to_vec(), Object::name("Encoding"));
    d.set(b"BaseEncoding".to_vec(), Object::name("WinAnsiEncoding"));
    d.set(b"Differences".to_vec(), Object::Array(differences));
    d
}

/// A `/ToUnicode` CMap for a font in the extended encoding: what each of its codes means, for
/// readers that don't know every glyph name (and for ț and Ț, which share the glyphs of ţ and Ţ).
pub fn extended_to_unicode() -> Vec<u8> {
    let mut out = String::from(
        "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n/CMapType 2 def\n1 begincodespacerange\n<00> <FF>\nendcodespacerange\n",
    );
    // At most 100 entries in a block (ISO 32000-2 §9.10.3).
    for (b, block) in extended().chunks(100).enumerate() {
        out.push_str(&format!("{} beginbfchar\n", block.len()));
        for (i, (c, _)) in block.iter().enumerate() {
            let code = usize::from(EXTENDED_FIRST) + b * 100 + i;
            let mut units = [0u16; 2];
            let hex: String = c.encode_utf16(&mut units).iter().map(|u| format!("{u:04X}")).collect();
            out.push_str(&format!("<{code:02X}> <{hex}>\n"));
        }
        out.push_str("endbfchar\n");
    }
    out.push_str("endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n");
    out.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_font_in_the_extended_encoding_reads_back_as_the_text_written() {
        use pdfcraft_cos::{Dict, Document, Object, Stream};
        let mut doc = Document::new_empty();
        let encoding = doc.add(Object::Dict(extended_encoding()));
        let to_unicode = doc.add(Object::Stream(Stream::flate(Dict::new(), &extended_to_unicode())));
        let mut f = Dict::new();
        f.set(b"Type".to_vec(), Object::name("Font"));
        f.set(b"Subtype".to_vec(), Object::name("Type1"));
        f.set(b"BaseFont".to_vec(), Object::name("Times-Roman"));
        f.set(b"Encoding".to_vec(), Object::Ref(encoding));
        f.set(b"ToUnicode".to_vec(), Object::Ref(to_unicode));
        let m = crate::pdf::Metrics::from_dict(&doc, &f);
        let text = "ăĂșȘțȚşţőŁ";
        let bytes: Vec<u8> = text.chars().filter_map(extended_code).collect();
        assert_eq!(bytes.len(), text.chars().count());
        assert_eq!(m.decode(&bytes), text);
        // Every extended glyph has a code and reads back.
        for g in extended() {
            let code = extended_code(g.0).unwrap_or_else(|| panic!("{} has no code", g.1));
            assert_eq!(m.decode(&[code]), g.0.to_string(), "{}", g.1);
        }
    }

    #[test]
    fn widths_are_the_published_ones() {
        assert_eq!(advance("Helvetica", 'A'), Some(667));
        assert_eq!(advance("Helvetica-BoldOblique", 'A'), Some(722));
        assert_eq!(advance("Times-Roman", 'A'), Some(722));
        assert_eq!(advance("Times-Bold", 'm'), Some(833));
        assert_eq!(advance("Times-Italic", ' '), Some(250));
        assert_eq!(advance("Courier-Bold", 'i'), Some(600));
        assert_eq!(advance("Symbol", 'A'), None);
        assert_eq!(advance("Arial", 'A'), None, "not a standard font's name");
        assert_eq!(glyph("Times-Roman", 'ț'), Some(("tcommaaccent", 278)));
        assert_eq!(glyph("Times-Roman", 'ţ'), Some(("tcommaaccent", 278)));
        assert_eq!(advance("Helvetica", '日'), None);
    }
}
