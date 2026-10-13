//! Text measurement and line breaking, by the fonts' own widths: the complete faces the form
//! carries for its typefaces ([`Fonts`]), and otherwise the standard 14 fonts that stand in for
//! them (none are bundled here).

use std::sync::Arc;

use pdfcraft_cos::Document;
use pdfcraft_fonts::form_fonts::{FormFace, form_faces};

use crate::model::{Font, HAlign, Para, Rect, RichText, Run};

/// The complete faces a form carries in its default resources, which its text is set in.
/// Empty for a form that embeds none.
#[derive(Clone, Default)]
pub struct Fonts(Arc<Vec<Arc<FormFace>>>);

impl Fonts {
    pub fn of(doc: &Document) -> Fonts {
        Fonts(Arc::new(form_faces(doc).into_iter().map(Arc::new).collect()))
    }

    /// The face of `family` (as `face_key` spells it) in this weight and posture.
    fn exact(&self, family: &str, bold: bool, italic: bool) -> Option<Arc<FormFace>> {
        self.0.iter().find(|f| f.family == family && f.bold == bold && f.italic == italic).cloned()
    }

    /// The face that draws `c` for text of `family` the standard fonts can't: of that family if
    /// one has it, and as close in weight and posture as there is.
    fn fallback(&self, c: char, family: &str, bold: bool, italic: bool) -> Option<&Arc<FormFace>> {
        self.0.iter().filter(|f| f.advance(c).is_some()).min_by_key(|f| (f.family != family, u8::from(f.bold != bold) + u8::from(f.italic != italic)))
    }
}

/// Two sets are equal when they are the same set: faces are read once per document.
impl PartialEq for Fonts {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0) || self.0.is_empty() && other.0.is_empty()
    }
}

impl std::fmt::Debug for Fonts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_list().entries(self.0.iter().map(|face| &face.name)).finish()
    }
}

/// Whether the standard fonts draw `c` (in WinAnsiEncoding or the extended encoding).
fn standard_has(c: char) -> bool {
    pdfcraft_fonts::win_ansi_byte(c).is_some() || pdfcraft_fonts::standard14::has_glyph(c)
}

/// The standard-14 family a typeface maps to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Family {
    Helvetica,
    Courier,
    Times,
}

impl Family {
    pub fn of(typeface: &str) -> Family {
        let t = typeface.to_ascii_lowercase();
        if t.contains("courier") || t.contains("mono") {
            Family::Courier
        } else if t.contains("times")
            || t.contains("georgia")
            || t.contains("garamond")
            || t.contains("minion")
            || t.contains("serif") && !t.contains("sans")
        {
            Family::Times
        } else {
            Family::Helvetica
        }
    }

    /// The PDF base font name for a face.
    pub fn base_font(self, bold: bool, italic: bool) -> &'static str {
        match (self, bold, italic) {
            (Family::Helvetica, false, false) => "Helvetica",
            (Family::Helvetica, true, false) => "Helvetica-Bold",
            (Family::Helvetica, false, true) => "Helvetica-Oblique",
            (Family::Helvetica, true, true) => "Helvetica-BoldOblique",
            (Family::Courier, false, false) => "Courier",
            (Family::Courier, true, false) => "Courier-Bold",
            (Family::Courier, false, true) => "Courier-Oblique",
            (Family::Courier, true, true) => "Courier-BoldOblique",
            (Family::Times, false, false) => "Times-Roman",
            (Family::Times, true, false) => "Times-Bold",
            (Family::Times, false, true) => "Times-Italic",
            (Family::Times, true, true) => "Times-BoldItalic",
        }
    }

    /// The resource name used in content streams and `/DA` strings (Acrobat's conventions).
    pub fn resource_name(self, bold: bool, italic: bool) -> &'static str {
        match (self, bold, italic) {
            (Family::Helvetica, false, false) => "Helv",
            (Family::Helvetica, true, false) => "HeBo",
            (Family::Helvetica, false, true) => "HeOb",
            (Family::Helvetica, true, true) => "HeBO",
            (Family::Courier, false, false) => "Cour",
            (Family::Courier, true, false) => "CoBo",
            (Family::Courier, false, true) => "CoOb",
            (Family::Courier, true, true) => "CoBO",
            (Family::Times, false, false) => "TiRo",
            (Family::Times, true, false) => "TiBo",
            (Family::Times, false, true) => "TiIt",
            (Family::Times, true, true) => "TiBI",
        }
    }
}

/// A resolved face: family, weight, posture, size and colour.
#[derive(Clone, Debug, PartialEq)]
pub struct Face {
    pub family: Family,
    pub bold: bool,
    pub italic: bool,
    pub size: f64,
    pub color: [f64; 3],
    pub underline: bool,
    /// The typeface, as `face_key` spells it, and the form's faces to find it in.
    typeface: Arc<str>,
    fonts: Fonts,
    /// The form's own face for this typeface, weight and posture, when it carries one.
    pub own: Option<Arc<FormFace>>,
}

impl Face {
    /// `font` among the standard fonts, and among `fonts` (the form's own) where it is there.
    pub fn of(font: &Font, fonts: &Fonts) -> Face {
        let typeface: Arc<str> = pdfcraft_fonts::embedding::face_key(&font.typeface).into();
        Face {
            family: Family::of(&font.typeface),
            bold: font.bold,
            italic: font.italic,
            size: font.size.clamp(1.0, 300.0),
            color: font.color.0,
            underline: font.underline,
            own: fonts.exact(&typeface, font.bold, font.italic),
            typeface,
            fonts: fonts.clone(),
        }
    }

    pub fn with_run(&self, run: &Run) -> Face {
        let (bold, italic) = (run.bold.unwrap_or(self.bold), run.italic.unwrap_or(self.italic));
        Face {
            family: self.family,
            bold,
            italic,
            size: run.size.unwrap_or(self.size).clamp(1.0, 300.0),
            color: self.color,
            underline: run.underline.unwrap_or(self.underline),
            own: if (bold, italic) == (self.bold, self.italic) { self.own.clone() } else { self.fonts.exact(&self.typeface, bold, italic) },
            typeface: Arc::clone(&self.typeface),
            fonts: self.fonts.clone(),
        }
    }

    /// The form's face that draws `c` in this face: its own where it has the character, else,
    /// for what the standard fonts lack too, the nearest of the form's faces that has it.
    /// `None`: the standard font draws it (as `?` when it can't).
    pub fn form_face(&self, c: char) -> Option<&Arc<FormFace>> {
        let c = if c == '\t' { ' ' } else { c };
        match &self.own {
            Some(own) if own.advance(c).is_some() => Some(own),
            _ if standard_has(c) => None,
            _ => self.fonts.fallback(c, &self.typeface, self.bold, self.italic),
        }
    }

    /// `text` as the runs to show, each in one font: a face of the form's, or (`None`) this
    /// face's standard font.
    pub fn runs(&self, text: &str) -> Vec<(Option<Arc<FormFace>>, String)> {
        let mut out: Vec<(Option<Arc<FormFace>>, String)> = Vec::new();
        for c in text.chars() {
            let face = self.form_face(c);
            match out.last_mut() {
                Some((last, run)) if last.as_ref().map(Arc::as_ptr) == face.map(Arc::as_ptr) => run.push(c),
                _ => out.push((face.cloned(), c.to_string())),
            }
        }
        out
    }

    pub fn resource_name(&self) -> &'static str {
        self.family.resource_name(self.bold, self.italic)
    }

    /// The PDF base font this face is set in.
    pub fn base_font(&self) -> &'static str {
        self.family.base_font(self.bold, self.italic)
    }

    /// Advance of `s` at this face's size, by the widths of the fonts that draw it. A character
    /// none of them can draw is shown as `?` and measured as one.
    pub fn width(&self, s: &str) -> f64 {
        let base = self.base_font();
        let units: u32 = s
            .chars()
            .map(|c| {
                let c = if c == '\t' { ' ' } else { c };
                if let Some(advance) = self.form_face(c).and_then(|f| f.advance(c)) {
                    return u32::from(advance);
                }
                u32::from(pdfcraft_fonts::standard14::advance(base, c).or_else(|| pdfcraft_fonts::standard14::advance(base, '?')).unwrap_or(600))
            })
            .fold(0, u32::saturating_add);
        f64::from(units) * self.size / 1000.0
    }
}

/// Default line height: XFA's font leading for the standard faces.
pub fn line_height(size: f64) -> f64 {
    size * 1.15
}

/// One positioned piece of text, ready to emit.
#[derive(Clone, Debug, PartialEq)]
pub struct Span {
    pub x: f64,
    /// Baseline, top-left based (y grows downwards).
    pub baseline: f64,
    pub text: String,
    pub face: Face,
}

/// A laid-out block of text.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Block {
    pub spans: Vec<Span>,
    /// Height used, from the top of the box.
    pub height: f64,
}

#[derive(Clone, Debug)]
struct Word {
    text: String,
    face: Face,
    width: f64,
    /// A tab: no text, and as wide as it takes to reach the next tab stop.
    tab: bool,
}

/// The distance between tab stops where a paragraph sets none: half an inch.
const TAB_INTERVAL: f64 = 36.0;

/// The width a tab has at `at`, measured from the start of the line's text.
fn tab_width(at: f64) -> f64 {
    let next = ((at / TAB_INTERVAL).floor() + 1.0) * TAB_INTERVAL;
    (next - at).clamp(0.0, TAB_INTERVAL)
}

/// The width of `words` set on one line.
fn line_width(words: &[Word]) -> f64 {
    words.iter().fold(0.0, |at, w| at + if w.tab { tab_width(at) } else { w.width })
}

/// Break the runs of a paragraph into words (spaces stay attached to the word before them).
fn words(runs: &[Run], base: &Face, embed: &dyn Fn(&str) -> String) -> Vec<Word> {
    let mut out = Vec::new();
    for run in runs {
        let face = base.with_run(run);
        let text: String = match &run.embed {
            Some(id) => embed(id),
            None => run
                .text
                .chars()
                .filter_map(|c| match c {
                    '\r' | '\n' | '\u{2028}' | '\u{2029}' | '\u{2009}' | '\u{202F}' | '\u{3000}' => Some(' '),
                    '\u{00AD}' | '\u{200B}' | '\u{200C}' | '\u{200D}' | '\u{FEFF}' => None,
                    '\u{2011}' | '\u{2010}' | '\u{2012}' | '\u{2013}' => Some('-'),
                    '\u{2018}' | '\u{2019}' => Some('\''),
                    _ => Some(c),
                })
                .collect(),
        };
        let mut cur = String::new();
        for ch in text.chars() {
            if ch == '\t' {
                if !cur.is_empty() {
                    let width = face.width(&cur);
                    out.push(Word { text: std::mem::take(&mut cur), face: face.clone(), width, tab: false });
                }
                out.push(Word { text: String::new(), face: face.clone(), width: 0.0, tab: true });
                continue;
            }
            cur.push(ch);
            if ch == ' ' {
                let width = face.width(&cur);
                out.push(Word { text: std::mem::take(&mut cur), face: face.clone(), width, tab: false });
            }
        }
        if !cur.is_empty() {
            let width = face.width(&cur);
            out.push(Word { text: cur, face, width, tab: false });
        }
    }
    out
}

/// Lay `rich` out inside `rect` with `para` and `base` face. `embed` resolves embedded fields to
/// their text. Returns the spans (absolute coordinates) and the height the text takes.
pub fn layout(rich: &RichText, rect: Rect, para: &Para, base: &Face, embed: &dyn Fn(&str) -> String) -> Block {
    struct Line {
        words: Vec<Word>,
        width: f64,
        size: f64,
        first: bool,
    }
    let avail = (rect.w - para.margin_left - para.margin_right).max(1.0);
    let mut lines: Vec<(Line, Option<HAlign>)> = Vec::new();
    let mut total = 0.0;
    for (pi, p) in rich.paragraphs.iter().enumerate() {
        let ws = words(&p.runs, base, embed);
        let mut line = Line { words: Vec::new(), width: 0.0, size: base.size, first: true };
        let mut first_in_para = true;
        let indent = |first: bool| if first { para.text_indent.max(0.0) } else { 0.0 };
        for mut w in ws {
            if w.tab {
                w.width = tab_width(line.width);
            }
            let fits = line.width + w.width <= avail - indent(line.first) || line.words.is_empty() && w.width <= avail - indent(line.first);
            if !fits && !line.words.is_empty() {
                let done = std::mem::replace(&mut line, Line { words: Vec::new(), width: 0.0, size: base.size, first: false });
                lines.push((done, p.h_align));
                if w.tab {
                    w.width = tab_width(0.0);
                }
            }
            if w.width > avail - indent(line.first) && line.words.is_empty() {
                // A word longer than a line: break it by character.
                let mut piece = String::new();
                for ch in w.text.chars() {
                    piece.push(ch);
                    if w.face.width(&piece) > avail - indent(line.first) && piece.chars().count() > 1 {
                        piece.pop();
                        let width = w.face.width(&piece);
                        line.words.push(Word { text: std::mem::take(&mut piece), face: w.face.clone(), width, tab: false });
                        line.width += width;
                        line.size = line.size.max(w.face.size);
                        let done = std::mem::replace(&mut line, Line { words: Vec::new(), width: 0.0, size: base.size, first: false });
                        lines.push((done, p.h_align));
                        piece.push(ch);
                    }
                }
                if !piece.is_empty() {
                    let width = w.face.width(&piece);
                    line.size = line.size.max(w.face.size);
                    line.width += width;
                    line.words.push(Word { text: piece, face: w.face.clone(), width, tab: false });
                }
                first_in_para = false;
                continue;
            }
            line.size = line.size.max(w.face.size);
            line.width += w.width;
            line.words.push(w);
            first_in_para = false;
        }
        if !line.words.is_empty() || first_in_para && pi + 1 < rich.paragraphs.len() {
            lines.push((line, p.h_align));
        }
        total += para.space_above + para.space_below;
    }
    let heights: Vec<f64> = lines.iter().map(|(l, _)| para.line_height.unwrap_or_else(|| line_height(l.size))).collect();
    total += heights.iter().sum::<f64>();
    let start_y = match para.v_align {
        crate::model::VAlign::Top => rect.y,
        crate::model::VAlign::Middle => rect.y + ((rect.h - total) / 2.0).max(0.0),
        crate::model::VAlign::Bottom => rect.y + (rect.h - total).max(0.0),
    };
    let mut spans = Vec::new();
    let mut y = start_y + para.space_above;
    for ((line, align), lh) in lines.into_iter().zip(heights) {
        let indent = if line.first { para.text_indent.max(0.0) } else { 0.0 };
        // Trailing spaces don't count for alignment.
        let trailing = line.words.last().map_or(0.0, |w| if w.text.ends_with(' ') { w.face.width(" ") } else { 0.0 });
        let used = (line.width - trailing).max(0.0);
        let mut x = rect.x + para.margin_left + indent;
        x += match align.unwrap_or(para.h_align) {
            HAlign::Left | HAlign::Justify => 0.0,
            HAlign::Center => ((avail - indent - used) / 2.0).max(0.0),
            HAlign::Right => (avail - indent - used).max(0.0),
        };
        // Baseline: ascent sits about 0.8 of the size below the line top for these faces.
        let baseline = y + lh - (lh - line.size * 0.8).max(0.0) / 2.0 - line.size * 0.08;
        let mut run: Option<Span> = None;
        for w in line.words {
            let width = w.width;
            match &mut run {
                // A tab ends the run: the next one starts at the stop.
                _ if w.tab => spans.extend(run.take()),
                Some(s) if s.face == w.face => s.text.push_str(&w.text),
                _ => {
                    spans.extend(run.take());
                    run = Some(Span { x, baseline, text: w.text, face: w.face });
                }
            }
            x += width;
        }
        if let Some(s) = run.take() {
            spans.push(s);
        }
        y += lh;
    }
    Block { spans, height: total }
}

/// The width of the longest paragraph of `rich` set on one line, with the paragraph margins.
pub fn natural_width(rich: &RichText, para: &Para, base: &Face, embed: &dyn Fn(&str) -> String) -> f64 {
    let widest = rich
        .paragraphs
        .iter()
        .map(|p| {
            let ws = words(&p.runs, base, embed);
            let trailing = ws.last().map_or(0.0, |w| if w.text.ends_with(' ') { w.face.width(" ") } else { 0.0 });
            (line_width(&ws) - trailing).max(0.0)
        })
        .fold(0.0, f64::max);
    widest + para.margin_left + para.margin_right + para.text_indent.max(0.0)
}

/// Height `rich` needs at `width` (for boxes without an explicit height).
pub fn measure_height(rich: &RichText, width: f64, para: &Para, base: &Face, embed: &dyn Fn(&str) -> String) -> f64 {
    let p = Para { v_align: crate::model::VAlign::Top, ..para.clone() };
    layout(rich, Rect::new(0.0, 0.0, width.max(1.0), 0.0), &p, base, embed).height
}

/// Plain text as rich text (paragraphs split on line breaks; a carriage return and line feed
/// together are one break, as form data often holds them).
pub fn plain(text: &str) -> RichText {
    RichText {
        paragraphs: text
            .replace("\r\n", "\n")
            .split(['\n', '\r', '\u{2028}', '\u{2029}'])
            .map(|l| crate::model::Paragraph { runs: vec![Run { text: l.to_string(), ..Run::default() }], h_align: None })
            .collect(),
    }
}
