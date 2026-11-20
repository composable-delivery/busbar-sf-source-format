//! Lossless ordered XML DOM and Salesforce Metadata API parity serializer.
//!
//! Models XML elements as ordered node trees rather than schema-specific structs so all
//! 543+ Salesforce metadata types round-trip with exact tag order, comments, CDATA,
//! attributes, numeric character entities, and leading-zero string values preserved.

mod compare;
mod dom;
mod error;
mod parse;
mod serialize;

pub use compare::semantic_eq;
pub use dom::{METADATA_XML_NS, XmlDocument, XmlElement, XmlNode};
pub use error::XmlError;
pub use parse::parse_xml;
pub use serialize::{correct_comments, handle_special_entities};
