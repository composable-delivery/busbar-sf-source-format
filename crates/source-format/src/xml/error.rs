//! Error types for XML parsing and DOM manipulation.

use thiserror::Error;

/// Errors returned when parsing malformed XML metadata files.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum XmlError {
    /// Underlying `quick-xml` parser failure.
    #[error("XML syntax error: {0}")]
    Syntax(#[from] quick_xml::Error),

    /// Invalid XML attribute encountered.
    #[error("invalid XML attribute: {0}")]
    Attribute(#[from] quick_xml::events::attributes::AttrError),

    /// Invalid character escape sequence.
    #[error("invalid XML character escape: {0}")]
    Escape(#[from] quick_xml::escape::EscapeError),

    /// Invalid UTF-8 or character encoding error while decoding XML.
    #[error("XML encoding error: {0}")]
    Encoding(#[from] quick_xml::encoding::EncodingError),

    /// End tag did not match the corresponding opening tag.
    #[error("mismatched closing tag '</{found}>', expected '</{expected}>'")]
    MismatchedEndTag {
        /// Tag name opened on the stack.
        expected: String,
        /// Tag name found in the closing tag.
        found: String,
    },

    /// Unexpected EOF while tags remained open.
    #[error("unexpected end of XML while element '<{tag}>' is still open")]
    UnexpectedEof {
        /// Unclosed element name.
        tag: String,
    },

    /// Document contained no root element.
    #[error("XML document has no root element")]
    EmptyDocument,

    /// Document contained more than one root element.
    #[error("XML document has multiple root elements ('{first}' and '{second}')")]
    MultipleRoots {
        /// First root element name.
        first: String,
        /// Second root element name.
        second: String,
    },
}
