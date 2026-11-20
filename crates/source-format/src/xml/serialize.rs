//! XML serializer matching SDR's `JsToXml` (`fast-xml-parser` `XMLBuilder`) formatting.

use std::fmt::Write as _;
use std::sync::OnceLock;

use quick_xml::escape::escape;
use regex::Regex;

use super::dom::{XmlDocument, XmlElement, XmlNode};

const XML_DECL: &str = "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n";
const INDENT_UNIT: &str = "    ";

static COMMENT_FIX_RE: OnceLock<Regex> = OnceLock::new();
static SPECIAL_ENTITY_RE: OnceLock<Regex> = OnceLock::new();

impl XmlDocument {
    /// Serializes the document using SDR's `JsToXml` formatting rules.
    ///
    /// Emits `<?xml version="1.0" encoding="UTF-8"?>\n`, 4-space indentation, `<tag></tag>`
    /// for empty elements, inline comment cleanup (`correct_comments`), and numeric entity
    /// preservation (`handle_special_entities`).
    pub fn to_xml_string(&self) -> String {
        let mut raw = if self.declaration {
            let mut s = String::with_capacity(256);
            s.push_str(XML_DECL);
            s
        } else {
            String::with_capacity(256)
        };
        write_element(&mut raw, &self.root, 0);
        if raw.contains("&amp;#") {
            raw = handle_special_entities(&raw);
        }
        if raw.contains("<!--") {
            raw = correct_comments(&raw);
        }
        raw
    }
}

/// Collapses formatting whitespace surrounding XML comments, matching SDR's `correctComments`.
///
/// # Panics
///
/// Cannot panic; the static regex pattern is validated at compile time.
pub fn correct_comments(xml: &str) -> String {
    if !xml.contains("<!--") {
        return xml.to_owned();
    }
    let re = COMMENT_FIX_RE.get_or_init(|| {
        Regex::new(r"\s+<!--([^\n\r]*?)-->\s+").expect("comment regex is a valid literal")
    });
    re.replace_all(xml, "<!--$1-->").into_owned()
}

/// Restores numeric character references after XML escaping, matching SDR's `handleSpecialEntities`.
///
/// # Panics
///
/// Cannot panic; the static regex pattern is validated at compile time.
pub fn handle_special_entities(xml: &str) -> String {
    if !xml.contains("&amp;#") {
        return xml.to_owned();
    }
    let re = SPECIAL_ENTITY_RE.get_or_init(|| {
        Regex::new(r"&amp;#(x?[\da-fA-F]+);").expect("special entity regex is a valid literal")
    });
    re.replace_all(xml, "&#$1;").into_owned()
}

fn write_element(out: &mut String, elem: &XmlElement, depth: usize) {
    write_indent(out, depth);
    out.push('<');
    out.push_str(&elem.name);
    for (k, v) in &elem.attributes {
        let escaped_v = escape(v);
        let _ = write!(out, " {k}=\"{escaped_v}\"");
    }
    out.push('>');

    match elem.children.as_slice() {
        [] => {
            let _ = writeln!(out, "</{}>", elem.name);
        }
        [XmlNode::Text(text)] => {
            out.push_str(&escape(text));
            let _ = writeln!(out, "</{}>", elem.name);
        }
        [XmlNode::CData(cdata)] => {
            let _ = writeln!(out, "<![CDATA[{cdata}]]></{}>", elem.name);
        }
        children => {
            out.push('\n');
            for child in children {
                match child {
                    XmlNode::Element(sub) => write_element(out, sub, depth + 1),
                    XmlNode::Text(text) => {
                        write_indent(out, depth + 1);
                        out.push_str(&escape(text));
                        out.push('\n');
                    }
                    XmlNode::CData(cdata) => {
                        write_indent(out, depth + 1);
                        let _ = writeln!(out, "<![CDATA[{cdata}]]>");
                    }
                    XmlNode::Comment(comment) => {
                        write_indent(out, depth + 1);
                        let _ = writeln!(out, "<!--{comment}-->");
                    }
                }
            }
            write_indent(out, depth);
            let _ = writeln!(out, "</{}>", elem.name);
        }
    }
}

fn write_indent(out: &mut String, depth: usize) {
    for _ in 0..depth {
        out.push_str(INDENT_UNIT);
    }
}
