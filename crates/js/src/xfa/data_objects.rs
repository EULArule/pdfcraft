//! The document's attachments as a form's scripts reach them through `event.target` (in Acrobat
//! the document): `dataObjects`, `createDataObject`, `removeDataObject`, `getDataObject`,
//! `getDataObjectContents`, `setDataObjectContents`, `importDataObject`, `exportDataObject`, and
//! `util.stringFromStream` / `util.streamFromString` for what they hold.
//!
//! Scripts work on a copy of the list: what they create or remove comes back as effects for the
//! engine to apply to the document. A file can't be chosen while a script runs, so
//! `importDataObject` without a file at hand stops the script and asks for one
//! ([`XfaOutcome::import_wanted`](super::XfaOutcome)); the caller runs the event again with the
//! file (or with none, when the choice was cancelled) in [`XfaDoc::import`](super::XfaDoc).

use std::sync::Arc;

use boa_engine::object::ObjectInitializer;
use boa_engine::object::builtins::JsArray;
use boa_engine::property::Attribute;
use boa_engine::{Context, JsResult, JsString, JsValue, NativeFunction, js_string};

use super::{XfaEffect, host};
use crate::{arg, error, s, text};

/// Largest attachment a script may create, import or read.
pub const MAX_DATA: usize = 16 << 20;
/// Most bytes a script may attach in one run.
pub const MAX_RUN_DATA: usize = 64 << 20;
/// Most attachments scripts see or keep.
pub const MAX_OBJECTS: usize = 1_000;
/// Longest attachment name, in characters.
const MAX_NAME: usize = 255;

/// An attachment as scripts see it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct XfaDataObject {
    pub name: String,
    /// Its media type, or empty.
    pub mime: String,
    pub size: usize,
    /// Its bytes; `None` for one too large to hand to a script.
    pub data: Option<Arc<Vec<u8>>>,
}

/// The answer to an `importDataObject` a script asked for in an earlier run of the event.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct XfaImport {
    /// The name the script gave.
    pub name: String,
    /// The file chosen, or `None` when the choice was cancelled.
    pub file: Option<XfaDataObject>,
}

/// The property a stream object keeps its bytes in, one character per byte.
const BYTES: &str = "__pcBytes";

/// Argument `index` of a call, or the one named `key` when the call was made with one object
/// of named arguments (`{cName: "a.xml", cValue: "…"}`), as Acrobat's calls also take them.
fn param(args: &[JsValue], index: usize, key: &str, ctx: &mut Context) -> JsResult<JsValue> {
    if let Some(named) = arg(args, 0).as_object()
        && named.has_own_property(js_string!("cName"), ctx)?
    {
        return named.get(JsString::from(key), ctx);
    }
    Ok(arg(args, index))
}

fn name_of(args: &[JsValue], ctx: &mut Context) -> JsResult<String> {
    let v = param(args, 0, "cName", ctx)?;
    let name = String::from_utf16_lossy(&v.to_string(ctx)?.to_vec()).trim().to_string();
    if name.is_empty() || name.chars().count() > MAX_NAME || name.chars().any(char::is_control) {
        return Err(error("an attachment needs a name of at most 255 characters"));
    }
    Ok(name)
}

fn stream_object(ctx: &mut Context, bytes: &[u8]) -> JsValue {
    fn read(this: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
        // Acrobat's ReadStream: the next bytes as hexadecimal text.
        let o = this.as_object().ok_or_else(|| error("not a stream"))?;
        let all = o.get(js_string!(BYTES), ctx)?.to_string(ctx)?.to_vec();
        let at = o.get(js_string!("__pcAt"), ctx)?.to_number(ctx)?;
        let at = if at.is_finite() && at > 0.0 { (at as usize).min(all.len()) } else { 0 };
        let want = arg(args, 0).to_number(ctx)?;
        let want = if want.is_finite() && want > 0.0 { want as usize } else { 0 };
        let end = at.saturating_add(want).min(all.len());
        let hex: String = all.get(at..end).unwrap_or_default().iter().map(|u| format!("{:02X}", *u & 0xFF)).collect();
        o.set(js_string!("__pcAt"), JsValue::from(end as f64), false, ctx)?;
        Ok(s(&hex))
    }
    let units: Vec<u16> = bytes.iter().map(|b| u16::from(*b)).collect();
    ObjectInitializer::new(ctx)
        .property(js_string!(BYTES), JsValue::from(JsString::from(units.as_slice())), Attribute::READONLY)
        .property(js_string!("__pcAt"), JsValue::from(0.0), Attribute::WRITABLE)
        .function(NativeFunction::from_fn_ptr(read), js_string!("read"), 1)
        .build()
        .into()
}

/// The bytes of a stream object, or of a string (one byte per character, as text of bytes).
fn stream_bytes(v: &JsValue, ctx: &mut Context) -> JsResult<Vec<u8>> {
    let units = match v.as_object() {
        Some(o) if !v.is_string() => o.get(js_string!(BYTES), ctx)?.to_string(ctx)?.to_vec(),
        _ => v.to_string(ctx)?.to_vec(),
    };
    if units.len() > MAX_DATA {
        return Err(error("the stream is larger than 16 MB"));
    }
    Ok(units.iter().map(|u| (*u & 0xFF) as u8).collect())
}

fn descriptor(ctx: &mut Context, o: &XfaDataObject) -> JsValue {
    ObjectInitializer::new(ctx)
        .property(js_string!("name"), s(&o.name), Attribute::READONLY)
        .property(js_string!("path"), s(&o.name), Attribute::READONLY)
        .property(js_string!("size"), JsValue::from(o.size as f64), Attribute::READONLY)
        .property(js_string!("MIMEType"), s(&o.mime), Attribute::READONLY)
        .property(js_string!("description"), s(""), Attribute::all())
        .property(js_string!("creationDate"), JsValue::null(), Attribute::READONLY)
        .property(js_string!("modDate"), JsValue::null(), Attribute::READONLY)
        .build()
        .into()
}

/// `doc.dataObjects`: the attachments, or `null` when there are none (as Acrobat answers).
pub(super) fn list(ctx: &mut Context) -> JsResult<JsValue> {
    let all: Vec<XfaDataObject> = host(ctx)?.borrow().data_objects.clone();
    if all.is_empty() {
        return Ok(JsValue::null());
    }
    let items: Vec<JsValue> = all.iter().map(|o| descriptor(ctx, o)).collect();
    Ok(JsArray::from_iter(items, ctx).into())
}

/// Keep `data` as the attachment `name` in the script's list and tell the engine.
fn attach(ctx: &mut Context, name: String, mime: String, data: Vec<u8>) -> JsResult<()> {
    if data.len() > MAX_DATA {
        return Err(error(format!("the attachment {name:?} would be larger than 16 MB")));
    }
    let mut h = host(ctx)?.borrow_mut();
    h.attached = h.attached.saturating_add(data.len());
    if h.attached > MAX_RUN_DATA {
        return Err(error("the script attached more than 64 MB"));
    }
    let data = Arc::new(data);
    let object = XfaDataObject { name: name.clone(), mime: mime.clone(), size: data.len(), data: Some(Arc::clone(&data)) };
    h.data_objects.retain(|o| o.name != name);
    if h.data_objects.len() >= MAX_OBJECTS {
        return Err(error("the document has too many attachments for a script to add one"));
    }
    h.data_objects.push(object);
    h.push(XfaEffect::SetDataObject { name, mime, data });
    Ok(())
}

/// `doc.createDataObject(cName, cValue, cMIMEType)`: text attached as a file, in UTF-8.
pub(super) fn create(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let name = name_of(args, ctx)?;
    let value = param(args, 1, "cValue", ctx)?;
    let value = if value.is_undefined() || value.is_null() { String::new() } else { String::from_utf16_lossy(&value.to_string(ctx)?.to_vec()) };
    let mime = text(&param(args, 2, "cMIMEType", ctx)?, ctx)?;
    attach(ctx, name, mime, value.into_bytes())?;
    Ok(JsValue::undefined())
}

/// `doc.removeDataObject(cName)`: nothing happens when there is none of that name.
pub(super) fn remove(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let name = name_of(args, ctx)?;
    let mut h = host(ctx)?.borrow_mut();
    let before = h.data_objects.len();
    h.data_objects.retain(|o| o.name != name);
    if h.data_objects.len() != before {
        h.push(XfaEffect::RemoveDataObject { name });
    }
    Ok(JsValue::undefined())
}

fn find(ctx: &mut Context, name: &str) -> JsResult<XfaDataObject> {
    let found = host(ctx)?.borrow().data_objects.iter().find(|o| o.name == name).cloned();
    found.ok_or_else(|| error(format!("the document has no attachment named {name:?}")))
}

/// `doc.getDataObject(cName)`.
pub(super) fn get(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let name = name_of(args, ctx)?;
    let found = find(ctx, &name)?;
    Ok(descriptor(ctx, &found))
}

/// `doc.getDataObjectContents(cName)`: a stream of the attachment's bytes.
pub(super) fn contents(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let name = name_of(args, ctx)?;
    let found = find(ctx, &name)?;
    let data = found.data.ok_or_else(|| error(format!("the attachment {name:?} is larger than 16 MB; a script can't read it")))?;
    Ok(stream_object(ctx, &data))
}

/// `doc.setDataObjectContents(cName, oStream)`.
pub(super) fn set_contents(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let name = name_of(args, ctx)?;
    let found = find(ctx, &name)?;
    let data = stream_bytes(&param(args, 1, "oStream", ctx)?, ctx)?;
    attach(ctx, name, found.mime, data)?;
    Ok(JsValue::undefined())
}

/// `doc.importDataObject(cName)`: the file chosen for this name becomes the attachment, and
/// the answer is whether one was chosen. Before a file has been asked for, the script stops
/// here and the engine asks.
pub(super) fn import(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let name = name_of(args, ctx)?;
    let answer = host(ctx)?.borrow().doc.import.clone().filter(|i| i.name == name);
    match answer {
        Some(answer) => match answer.file {
            Some(file) => {
                let data = file.data.as_deref().cloned().unwrap_or_default();
                attach(ctx, name, file.mime, data)?;
                Ok(JsValue::from(true))
            }
            None => Ok(JsValue::from(false)),
        },
        None => {
            host(ctx)?.borrow_mut().import_wanted = Some(name);
            Err(error("a file has to be chosen before the script can go on"))
        }
    }
}

/// `doc.exportDataObject(cName)` (or `{cName: …}`): the engine is asked to save the
/// attachment where the user says. It is never opened or run, whatever `nLaunch` asks.
pub(super) fn export(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let name = name_of(args, ctx)?;
    find(ctx, &name)?;
    host(ctx)?.borrow_mut().push(XfaEffect::ExportDataObject { name });
    Ok(JsValue::undefined())
}

/// The character set a script names, as one of those read and written here.
fn charset(v: &JsValue, ctx: &mut Context) -> JsResult<String> {
    let name = text(v, ctx)?.trim().to_ascii_lowercase().replace('_', "-");
    Ok(if name.is_empty() { "utf-8".into() } else { name })
}

/// `util.stringFromStream(oStream, cCharSet)`: UTF-8 (the default), UTF-16 or Latin-1 text.
pub(super) fn string_from_stream(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let bytes = stream_bytes(&arg(args, 0), ctx)?;
    let text = match charset(&arg(args, 1), ctx)?.as_str() {
        "utf-8" | "utf8" => String::from_utf8_lossy(bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(&bytes)).into_owned(),
        "utf-16" | "utf16" | "utf-16be" | "utf-16le" | "ucs-2" => {
            let (little, body) = match bytes.as_slice() {
                [0xFF, 0xFE, rest @ ..] => (true, rest),
                [0xFE, 0xFF, rest @ ..] => (false, rest),
                all => (false, all),
            };
            let units: Vec<u16> =
                body.as_chunks::<2>().0.iter().map(|c| if little { u16::from_le_bytes(*c) } else { u16::from_be_bytes(*c) }).collect();
            String::from_utf16_lossy(&units)
        }
        "iso-8859-1" | "latin1" | "latin-1" | "windows-1252" | "cp1252" => bytes.iter().map(|b| char::from(*b)).collect(),
        other => return Err(error(format!("stringFromStream: the character set {other:?} is not read here (utf-8, utf-16 and iso-8859-1 are)"))),
    };
    Ok(JsValue::from(JsString::from(text.as_str())))
}

/// `util.streamFromString(cString, cCharSet)`.
pub(super) fn stream_from_string(_: &JsValue, args: &[JsValue], ctx: &mut Context) -> JsResult<JsValue> {
    let value = arg(args, 0);
    let text = if value.is_undefined() || value.is_null() { String::new() } else { String::from_utf16_lossy(&value.to_string(ctx)?.to_vec()) };
    if text.len() > MAX_DATA {
        return Err(error("the text is larger than 16 MB"));
    }
    let bytes: Vec<u8> = match charset(&arg(args, 1), ctx)?.as_str() {
        "utf-8" | "utf8" => text.into_bytes(),
        "utf-16" | "utf16" | "utf-16be" | "ucs-2" => text.encode_utf16().flat_map(u16::to_be_bytes).collect(),
        "utf-16le" => text.encode_utf16().flat_map(u16::to_le_bytes).collect(),
        "iso-8859-1" | "latin1" | "latin-1" | "windows-1252" | "cp1252" => text.chars().map(|c| u8::try_from(u32::from(c)).unwrap_or(b'?')).collect(),
        other => return Err(error(format!("streamFromString: the character set {other:?} is not written here (utf-8, utf-16 and iso-8859-1 are)"))),
    };
    Ok(stream_object(ctx, &bytes))
}
