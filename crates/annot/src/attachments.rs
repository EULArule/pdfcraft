//! The document's own attachments: the files in the catalog's `/Names /EmbeddedFiles` name tree
//! (ISO 32000-2 §7.11.4, §14.13), as opposed to the file a File Attachment comment carries on
//! a page. Listed, read, added, replaced and removed here by name.
//!
//! A change writes the tree again as one sorted leaf; file specifications that stay are kept as
//! they are (the same objects), so nothing about them is lost.

use pdfcraft_cos::{Dict, Document, ObjRef, Object, PdfString, Stream};

use crate::AnnotError;

/// Most attachments a document may have for one to be added or removed (and most listed).
pub const MAX_ATTACHMENTS: usize = 10_000;
/// Deepest name tree followed.
const MAX_DEPTH: usize = 32;
/// Longest attachment name, in characters.
pub const MAX_NAME: usize = 255;

/// One attachment of the document.
#[derive(Clone, Debug, PartialEq)]
pub struct DocAttachment {
    /// Its key in the name tree, as text: what it is found by.
    pub name: String,
    /// The file name its specification gives (`/UF`, else `/F`), which viewers show.
    pub file_name: String,
    /// The size its embedded file says it has, when it says.
    pub size: Option<usize>,
    /// Its media type (`/Subtype` of the embedded file stream), when it has one.
    pub mime: Option<String>,
    key: Vec<u8>,
    spec: Object,
}

fn dict_of(doc: &Document, o: Option<&Object>) -> Option<Dict> {
    o.and_then(|o| doc.resolve(o).as_dict().cloned())
}

/// The `/EmbeddedFiles` tree as the catalog has it.
fn tree_of(doc: &Document) -> Option<Object> {
    let catalog = doc.get(doc.root()?);
    dict_of(doc, catalog.as_dict()?.get(b"Names"))?.get(b"EmbeddedFiles").cloned()
}

/// The (key, value) pairs of a name tree, in order; `false` when there are more than the cap
/// (the tree is then not one to rewrite).
fn entries(doc: &Document, node: &Object, depth: usize, out: &mut Vec<(Vec<u8>, Object)>) -> bool {
    if depth > MAX_DEPTH {
        return true;
    }
    let Some(d) = dict_of(doc, Some(node)) else { return true };
    if let Some(names) = d.get(b"Names").map(|n| doc.resolve(n)).and_then(|n| n.as_array().cloned()) {
        for pair in names.chunks(2) {
            if let [k, v] = pair
                && let Some(k) = doc.resolve(k).as_string()
            {
                if out.len() >= MAX_ATTACHMENTS {
                    return false;
                }
                out.push((k.bytes.clone(), v.clone()));
            }
        }
    }
    for k in d.get(b"Kids").map(|k| doc.resolve(k)).and_then(|k| k.as_array().cloned()).unwrap_or_default() {
        if !entries(doc, &k, depth + 1, out) {
            return false;
        }
    }
    true
}

/// The embedded file stream of a file specification.
fn embedded(doc: &Document, spec: &Object) -> Option<ObjRef> {
    let ef = dict_of(doc, dict_of(doc, Some(spec))?.get(b"EF"))?;
    [b"UF".as_slice(), b"F".as_slice()].iter().find_map(|k| ef.get(k).and_then(Object::as_ref))
}

/// The document's attachments, in the order of their names (at most [`MAX_ATTACHMENTS`]).
pub fn document_attachments(doc: &Document) -> Vec<DocAttachment> {
    let Some(tree) = tree_of(doc) else { return Vec::new() };
    let mut pairs = Vec::new();
    entries(doc, &tree, 0, &mut pairs);
    pairs
        .into_iter()
        .map(|(key, spec)| {
            let name = PdfString::literal(key.clone()).to_text();
            let fs = dict_of(doc, Some(&spec)).unwrap_or_default();
            let text = |k: &[u8]| fs.get(k).map(|v| doc.resolve(v)).and_then(|v| v.as_string().map(PdfString::to_text)).filter(|t| !t.is_empty());
            let stream = embedded(doc, &spec).map(|r| doc.get(r));
            let stream_dict = stream.as_deref().and_then(|o| if let Object::Stream(s) = o { Some(&s.dict) } else { None });
            let size = stream_dict.and_then(|d| dict_of(doc, d.get(b"Params"))).and_then(|p| p.int(b"Size")).and_then(|n| usize::try_from(n).ok());
            let mime = stream_dict.and_then(|d| d.name(b"Subtype")).map(|m| String::from_utf8_lossy(m).into_owned());
            DocAttachment { file_name: text(b"UF").or_else(|| text(b"F")).unwrap_or_else(|| name.clone()), name, size, mime, key, spec }
        })
        .collect()
}

/// The bytes of the attachment named `name`: `Ok(None)` when the document has none of that
/// name, an error when it is larger than `max` or can't be decoded.
pub fn read_document_attachment(doc: &Document, name: &str, max: usize) -> Result<Option<Vec<u8>>, AnnotError> {
    let Some(found) = document_attachments(doc).into_iter().find(|a| a.name == name) else { return Ok(None) };
    let Some(stream) = embedded(doc, &found.spec) else {
        return Err(AnnotError::Invalid(format!("the attachment {name:?} is not embedded in the document")));
    };
    let stream = doc.get(stream);
    let Object::Stream(stream) = stream.as_ref() else {
        return Err(AnnotError::Invalid(format!("the attachment {name:?} has no data")));
    };
    stream.decoded_within(max).map(Some).map_err(|e| AnnotError::Invalid(format!("the attachment {name:?} could not be read: {e}")))
}

/// Write the tree again as one sorted leaf.
fn write(doc: &mut Document, mut pairs: Vec<(Vec<u8>, Object)>) -> Result<(), AnnotError> {
    pairs.sort_by(|a, b| a.0.cmp(&b.0));
    let mut names = Vec::with_capacity(pairs.len().saturating_mul(2));
    for (key, spec) in pairs {
        names.push(Object::String(PdfString::literal(key)));
        names.push(spec);
    }
    let mut tree = Dict::new();
    tree.set(b"Names".to_vec(), Object::Array(names));
    let root = doc.root().ok_or(AnnotError::NoPageTree)?;
    let catalog = doc.get(root).as_dict().cloned().unwrap_or_default();
    // The tree and the names dictionary stay where they are when they are objects of their own.
    let names_ref = catalog.get(b"Names").and_then(Object::as_ref).filter(|r| doc.get(*r).as_dict().is_some());
    let mut names_dict = dict_of(doc, catalog.get(b"Names")).unwrap_or_default();
    match names_dict.get(b"EmbeddedFiles").and_then(Object::as_ref) {
        Some(tree_ref) => doc.set(tree_ref, Object::Dict(tree)),
        None => {
            let tree_ref = doc.add(Object::Dict(tree));
            names_dict.set(b"EmbeddedFiles".to_vec(), Object::Ref(tree_ref));
            match names_ref {
                Some(r) => doc.set(r, Object::Dict(names_dict)),
                None => doc.update_dict(root, |c| c.set(b"Names".to_vec(), Object::Dict(names_dict)))?,
            }
        }
    }
    Ok(())
}

fn pairs_for_change(doc: &Document) -> Result<Vec<(Vec<u8>, Object)>, AnnotError> {
    let mut pairs = Vec::new();
    if let Some(tree) = tree_of(doc)
        && !entries(doc, &tree, 0, &mut pairs)
    {
        return Err(AnnotError::Invalid(format!("the document has more than {MAX_ATTACHMENTS} attachments; none can be added or removed")));
    }
    Ok(pairs)
}

/// Attach `data` to the document as `name`, in place of an attachment of that name if it has
/// one. `mime` is its media type (`text/plain`), `date` a PDF date for when it was made.
pub fn set_document_attachment(doc: &mut Document, name: &str, data: &[u8], mime: Option<&str>, date: Option<&str>) -> Result<(), AnnotError> {
    let name = name.trim();
    if name.is_empty() || name.chars().count() > MAX_NAME || name.chars().any(char::is_control) {
        return Err(AnnotError::Invalid("an attachment needs a name of at most 255 characters".into()));
    }
    let mut pairs = pairs_for_change(doc)?;
    pairs.retain(|(key, _)| PdfString::literal(key.clone()).to_text() != name);
    if pairs.len() >= MAX_ATTACHMENTS {
        return Err(AnnotError::Invalid(format!("the document already has {MAX_ATTACHMENTS} attachments")));
    }
    let mut params = Dict::new();
    params.set(b"Size".to_vec(), Object::Int(i64::try_from(data.len()).unwrap_or(i64::MAX)));
    if let Some(date) = date {
        params.set(b"ModDate".to_vec(), PdfString::literal(date.as_bytes().to_vec()));
    }
    let mut ef = Dict::new();
    ef.set(b"Type".to_vec(), Object::name("EmbeddedFile"));
    // A media type is a name such as `text/plain`; anything else is left out.
    if let Some(mime) = mime.map(str::trim).filter(|m| !m.is_empty() && m.len() <= 127 && m.bytes().all(|b| b.is_ascii_graphic())) {
        ef.set(b"Subtype".to_vec(), Object::name(mime));
    }
    ef.set(b"Params".to_vec(), Object::Dict(params));
    let ef = doc.add(Object::Stream(Stream::flate(ef, data)));
    let mut efd = Dict::new();
    efd.set(b"F".to_vec(), Object::Ref(ef));
    efd.set(b"UF".to_vec(), Object::Ref(ef));
    let mut fs = Dict::new();
    fs.set(b"Type".to_vec(), Object::name("Filespec"));
    fs.set(b"F".to_vec(), PdfString::text(name));
    fs.set(b"UF".to_vec(), PdfString::text(name));
    fs.set(b"EF".to_vec(), Object::Dict(efd));
    let spec = doc.add(Object::Dict(fs));
    pairs.push((PdfString::text(name).bytes, Object::Ref(spec)));
    write(doc, pairs)
}

/// Remove the attachment named `name`. Returns whether the document had one.
pub fn remove_document_attachment(doc: &mut Document, name: &str) -> Result<bool, AnnotError> {
    let mut pairs = pairs_for_change(doc)?;
    let before = pairs.len();
    pairs.retain(|(key, _)| PdfString::literal(key.clone()).to_text() != name);
    if pairs.len() == before {
        return Ok(false);
    }
    write(doc, pairs)?;
    Ok(true)
}
