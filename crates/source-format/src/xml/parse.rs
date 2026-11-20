//! Streaming parser from XML text into [`XmlDocument`] using `quick-xml`.

use quick_xml::Reader;
use quick_xml::escape::resolve_xml_entity;
use quick_xml::events::{BytesStart, Event};

use super::dom::{XmlDocument, XmlElement, XmlNode};
use super::error::XmlError;

/// Parses an XML string into a lossless ordered [`XmlDocument`].
///
/// Preserves tag order, comments, CDATA, attributes, and raw string representations
/// (never coercing `"001"` or `"true"`). Whitespace around text values is trimmed to match
/// SDR's `fast-xml-parser` configuration, and whitespace-only formatting text is dropped.
///
/// # Errors
///
/// Returns [`XmlError`] if the XML is malformed, has mismatched or unclosed tags, or
/// contains zero or multiple root elements.
pub fn parse_xml(input: &str) -> Result<XmlDocument, XmlError> {
    let mut reader = Reader::from_str(input);
    let mut declaration = false;
    let mut stack: Vec<XmlElement> = Vec::new();
    let mut root: Option<XmlElement> = None;
    let mut pending_text = String::new();

    loop {
        match reader.read_event()? {
            Event::Decl(_) => {
                declaration = true;
            }
            Event::Start(ref start) => {
                flush_pending_text(&mut pending_text, stack.last_mut());
                let elem = build_element(start)?;
                stack.push(elem);
            }
            Event::Empty(ref start) => {
                flush_pending_text(&mut pending_text, stack.last_mut());
                let elem = build_element(start)?;
                attach_element(elem, &mut stack, &mut root)?;
            }
            Event::End(ref end) => {
                flush_pending_text(&mut pending_text, stack.last_mut());
                let found = end.name().as_ref().to_owned();
                let popped = stack.pop().ok_or_else(|| XmlError::MismatchedEndTag {
                    expected: "<root>".to_owned(),
                    found: found.clone(),
                })?;
                if popped.name != found {
                    return Err(XmlError::MismatchedEndTag {
                        expected: popped.name,
                        found,
                    });
                }
                attach_element(popped, &mut stack, &mut root)?;
            }
            Event::Text(ref text) => {
                let decoded = text.xml10_content();
                pending_text.push_str(&decoded);
            }
            Event::GeneralRef(ref entity) => {
                if let Some(ch) = entity.resolve_char_ref()? {
                    pending_text.push(ch);
                } else if let Some(resolved) = resolve_xml_entity(entity) {
                    pending_text.push_str(resolved);
                } else {
                    pending_text.push('&');
                    pending_text.push_str(entity);
                    pending_text.push(';');
                }
            }
            Event::CData(ref cdata) => {
                flush_pending_text(&mut pending_text, stack.last_mut());
                if let Some(current) = stack.last_mut() {
                    current
                        .children
                        .push(XmlNode::CData(cdata.as_ref().to_owned()));
                }
            }
            Event::Comment(ref comment) => {
                flush_pending_text(&mut pending_text, stack.last_mut());
                if let Some(current) = stack.last_mut() {
                    current
                        .children
                        .push(XmlNode::Comment(comment.as_ref().to_owned()));
                }
            }
            Event::PI(_) | Event::DocType(_) => {}
            Event::Eof => break,
        }
    }

    if let Some(open) = stack.pop() {
        return Err(XmlError::UnexpectedEof { tag: open.name });
    }

    let root = root.ok_or(XmlError::EmptyDocument)?;
    Ok(XmlDocument { declaration, root })
}

fn build_element(start: &BytesStart<'_>) -> Result<XmlElement, XmlError> {
    let name = start.name().as_ref().to_owned();
    let mut attributes = Vec::new();
    for attr_res in start.attributes() {
        let attr = attr_res?;
        let key = attr.key.as_ref().to_owned();
        let value = attr
            .normalized_value(quick_xml::XmlVersion::Implicit1_0)?
            .into_owned();
        attributes.push((key, value));
    }
    Ok(XmlElement {
        name,
        attributes,
        children: Vec::new(),
    })
}

fn attach_element(
    elem: XmlElement,
    stack: &mut [XmlElement],
    root: &mut Option<XmlElement>,
) -> Result<(), XmlError> {
    if let Some(parent) = stack.last_mut() {
        parent.children.push(XmlNode::Element(elem));
        return Ok(());
    }
    if let Some(existing) = root {
        return Err(XmlError::MultipleRoots {
            first: existing.name.clone(),
            second: elem.name,
        });
    }
    *root = Some(elem);
    Ok(())
}

fn flush_pending_text(pending: &mut String, current: Option<&mut XmlElement>) {
    if pending.is_empty() {
        return;
    }
    let trimmed = pending.trim();
    if !trimmed.is_empty()
        && let Some(el) = current
    {
        el.children.push(XmlNode::Text(trimmed.to_owned()));
    }
    pending.clear();
}
