//! Lossless ordered XML DOM types and Salesforce metadata helpers.

/// Standard Salesforce Metadata API XML namespace URI.
pub const METADATA_XML_NS: &str = "http://soap.sforce.com/2006/04/metadata";

/// A parsed XML document with an optional XML declaration and a single root element.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XmlDocument {
    /// Whether an XML declaration (`<?xml version="1.0" encoding="UTF-8"?>`) is emitted.
    pub declaration: bool,
    /// Root element of the document.
    pub root: XmlElement,
}

impl XmlDocument {
    /// Creates a new Salesforce metadata XML document with the standard XML declaration.
    pub fn new_metadata(root: XmlElement) -> Self {
        Self {
            declaration: true,
            root: root.with_metadata_xmlns(),
        }
    }
}

/// An XML element preserving attribute order and heterogeneous child node order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XmlElement {
    /// Element tag name.
    pub name: String,
    /// Element attributes in insertion order.
    pub attributes: Vec<(String, String)>,
    /// Ordered child nodes (elements, text, CDATA, and comments).
    pub children: Vec<XmlNode>,
}

impl XmlElement {
    /// Creates an empty XML element with the given tag name.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            attributes: Vec::new(),
            children: Vec::new(),
        }
    }

    /// Appends or replaces an attribute on `self` in builder style.
    #[must_use]
    pub fn with_attr(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.set_attr(key, value);
        self
    }

    /// Sets or updates an attribute value in place.
    pub fn set_attr(&mut self, key: impl Into<String>, value: impl Into<String>) {
        let key = key.into();
        let value = value.into();
        if let Some((_, existing)) = self.attributes.iter_mut().find(|(k, _)| k == &key) {
            *existing = value;
        } else {
            self.attributes.push((key, value));
        }
    }

    /// Returns a copy of this element with `xmlns` removed (preserving other attributes).
    #[must_use]
    pub fn without_xmlns(&self) -> Self {
        Self {
            name: self.name.clone(),
            attributes: self
                .attributes
                .iter()
                .filter(|(k, _)| k != "xmlns")
                .cloned()
                .collect(),
            children: self.children.clone(),
        }
    }

    /// Ensures the `xmlns` attribute is set to [`METADATA_XML_NS`] as the first attribute if absent.
    #[must_use]
    pub fn with_metadata_xmlns(mut self) -> Self {
        if !self.attributes.iter().any(|(k, _)| k == "xmlns") {
            self.attributes
                .insert(0, ("xmlns".to_owned(), METADATA_XML_NS.to_owned()));
        }
        self
    }

    /// Iterates over immediate child [`XmlElement`] nodes.
    pub fn child_elements(&self) -> impl Iterator<Item = &Self> {
        self.children.iter().filter_map(|node| match node {
            XmlNode::Element(el) => Some(el),
            _ => None,
        })
    }

    /// Iterates mutably over immediate child [`XmlElement`] nodes.
    pub fn child_elements_mut(&mut self) -> impl Iterator<Item = &mut Self> {
        self.children.iter_mut().filter_map(|node| match node {
            XmlNode::Element(el) => Some(el),
            _ => None,
        })
    }

    /// Returns the first immediate child element with the matching tag name.
    pub fn child_element(&self, name: &str) -> Option<&Self> {
        self.child_elements().find(|el| el.name == name)
    }

    /// Returns the text value if this element holds a single text or CDATA node.
    pub fn text_content(&self) -> Option<&str> {
        match self.children.as_slice() {
            [XmlNode::Text(text) | XmlNode::CData(text)] => Some(text.as_str()),
            _ => None,
        }
    }

    /// Extracts the unique identifying text value for decomposed/non-decomposed child entries.
    ///
    /// Checks `unique_id` first if provided, then falls back to `fullName` and `name`,
    /// matching SDR's `extractUniqueElementValue`.
    pub fn extract_unique_element(&self, unique_id: Option<&str>) -> Option<&str> {
        if let Some(key) = unique_id
            && let Some(val) = self.child_element(key).and_then(Self::text_content)
        {
            return Some(val);
        }
        self.child_element("fullName")
            .and_then(Self::text_content)
            .or_else(|| self.child_element("name").and_then(Self::text_content))
    }

    /// Returns `true` if the element contains any child nodes or non-`xmlns` attributes.
    ///
    /// Matches SDR's `objectHasSomeRealValues` guard against overwriting existing non-empty
    /// decomposed parents with empty retrieve stubs.
    pub fn has_real_values(&self) -> bool {
        !self.children.is_empty() || self.attributes.iter().any(|(k, _)| k != "xmlns")
    }
}

/// A node inside an [`XmlElement`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum XmlNode {
    /// Nested child XML element.
    Element(XmlElement),
    /// Trimmed text content.
    Text(String),
    /// CDATA section content (`<![CDATA[...]]>`).
    CData(String),
    /// XML comment content (`<!--...-->`).
    Comment(String),
}
