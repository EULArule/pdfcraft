//! The complete font programs a form carries in its default resources (`/AcroForm /DR /Font`).
//! Form designers embed every typeface a form uses there, in full, so what is typed into it can
//! be drawn. Text PdfCraft writes into such a form (a laid-out XFA template, a field's value) is
//! set in those faces: they are the document's own, they have the letters the standard fonts
//! lack (Cyrillic, Greek), and nothing is bundled or copied to draw them.

use std::collections::HashMap;

use pdfcraft_cos::{Dict, Document, ObjRef, Object};
use skrifa::{
    FontRef, MetadataProvider,
    instance::{LocationRef, Size},
};

use crate::embedding;

/// Most font resources looked at.
const MAX_FONTS: usize = 256;
/// Most faces kept.
const MAX_FACES: usize = 32;
/// Most decoded program bytes read in total.
const MAX_TOTAL: usize = 64 << 20;
/// Largest program read (what [`embedding::validate`] accepts).
const MAX_PROGRAM: usize = 16 << 20;

/// One complete face of the form's default resources.
#[derive(Clone, Debug, PartialEq)]
pub struct FormFace {
    /// The family, as [`embedding::face_key`] spells it (`arialnarrow`).
    pub family: String,
    pub bold: bool,
    pub italic: bool,
    /// The PostScript name (`ArialNarrow-Bold`).
    pub name: String,
    /// The font file stream.
    pub program: ObjRef,
    /// Advance of each character the face has, in thousandths of the font size.
    advances: HashMap<char, u16>,
}

impl FormFace {
    /// The advance of `c` in thousandths of the font size, if the face has a glyph for it.
    pub fn advance(&self, c: char) -> Option<u16> {
        self.advances.get(&c).copied()
    }

    /// The name of this face's Unicode font among the form's default resources.
    pub fn resource_name(&self) -> String {
        resource_name(self.program)
    }
}

/// The resource name of the Unicode font over the face whose program is `program`.
pub fn resource_name(program: ObjRef) -> String {
    format!("PCF{}", program.num)
}

fn dict_of(doc: &Document, o: Option<&Object>) -> Option<Dict> {
    o.and_then(|o| doc.resolve(o).as_dict().cloned())
}

fn default_fonts(doc: &Document) -> Option<Dict> {
    let catalog = doc.get(doc.root()?);
    let acro = dict_of(doc, catalog.as_dict()?.get(b"AcroForm"))?;
    let dr = dict_of(doc, acro.get(b"DR"))?;
    dict_of(doc, dr.get(b"Font"))
}

/// The program of a font dictionary: its stream and decoded bytes, when it is a complete
/// TrueType or OpenType file.
fn program_of(doc: &Document, font: &Dict, budget: &mut usize) -> Option<(ObjRef, Vec<u8>)> {
    let descendant = font.get(b"DescendantFonts").and_then(|o| doc.resolve(o).as_array().and_then(|a| a.first().cloned()));
    let descriptor = dict_of(doc, dict_of(doc, descendant.as_ref()).as_ref().unwrap_or(font).get(b"FontDescriptor"))?;
    let program = [b"FontFile2".as_slice(), b"FontFile3".as_slice()].iter().find_map(|key| descriptor.get(key).and_then(Object::as_ref))?;
    let stream = doc.get(program);
    let Object::Stream(stream) = stream.as_ref() else { return None };
    let bytes = stream.decoded_within(MAX_PROGRAM.min(*budget)).ok()?;
    *budget = budget.saturating_sub(bytes.len());
    Some((program, bytes))
}

/// The complete, embeddable faces among the form's default font resources: not subsets (which
/// lack the glyphs nothing used yet), and not programs whose licence bits forbid editing.
pub fn form_faces(doc: &Document) -> Vec<FormFace> {
    let Some(fonts) = default_fonts(doc) else { return Vec::new() };
    let mut out: Vec<FormFace> = Vec::new();
    let mut budget = MAX_TOTAL;
    for (_, entry) in fonts.iter().take(MAX_FONTS) {
        if out.len() >= MAX_FACES || budget == 0 {
            break;
        }
        let Some(font) = dict_of(doc, Some(entry)) else { continue };
        let metrics = crate::pdf::Metrics::from_dict(doc, &font);
        if metrics.subset {
            continue;
        }
        let Some((program, bytes)) = program_of(doc, &font, &mut budget) else { continue };
        if out.iter().any(|f| f.program == program) || embedding::validate(&bytes).is_err() {
            continue;
        }
        let Ok((family, name)) = embedding::names(&bytes) else { continue };
        let Ok(face) = FontRef::new(&bytes) else { continue };
        let units = f64::from(face.metrics(Size::unscaled(), LocationRef::default()).units_per_em);
        if units <= 0.0 {
            continue;
        }
        let glyphs = face.glyph_metrics(Size::unscaled(), LocationRef::default());
        let advances: HashMap<char, u16> = face
            .charmap()
            .mappings()
            .take(65_535)
            .filter(|(_, gid)| gid.to_u32() > 0)
            .filter_map(|(cp, gid)| {
                let advance = f64::from(glyphs.advance_width(gid)?) * 1000.0 / units;
                Some((char::from_u32(cp)?, if advance.is_finite() { advance.clamp(0.0, 16_000.0).round() as u16 } else { 0 }))
            })
            .collect();
        if advances.is_empty() {
            continue;
        }
        out.push(FormFace { family: embedding::face_key(&family), bold: metrics.bold, italic: metrics.italic, name, program, advances });
    }
    out
}

/// The Unicode (Identity-H) font of `face` among the form's default resources, under
/// [`FormFace::resource_name`]: the one an earlier call added, or a new one over the face's
/// program. The caller puts it into the resources it writes.
pub fn unicode_font(doc: &mut Document, face: &FormFace) -> Result<ObjRef, String> {
    let existing = default_fonts(doc).and_then(|fonts| fonts.get(face.resource_name().as_bytes()).and_then(Object::as_ref));
    if let Some(font) = existing
        && let Some(d) = doc.get(font).as_dict()
        && d.name(b"Subtype") == Some(b"Type0")
        && program_ref(doc, d) == Some(face.program)
    {
        return Ok(font);
    }
    let stream = doc.get(face.program);
    let Object::Stream(stream) = stream.as_ref() else { return Err("the font program is gone".into()) };
    let bytes = stream.decoded_within(MAX_PROGRAM).map_err(|e| e.to_string())?;
    embedding::embed_existing(doc, face.program, &bytes, &face.name)
}

fn program_ref(doc: &Document, font: &Dict) -> Option<ObjRef> {
    let descendant = font.get(b"DescendantFonts").and_then(|o| doc.resolve(o).as_array().and_then(|a| a.first().cloned()));
    let descriptor = dict_of(doc, dict_of(doc, descendant.as_ref())?.get(b"FontDescriptor"))?;
    [b"FontFile2".as_slice(), b"FontFile3".as_slice()].iter().find_map(|key| descriptor.get(key).and_then(Object::as_ref))
}
