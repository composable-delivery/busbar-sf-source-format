//! Salesforce Metadata API manifest (`package.xml`, `destructiveChanges*.xml`) parsing
//! and serialization.

use std::fmt;
use std::path::Path;

use crate::xml::{XmlDocument, XmlElement, XmlNode, parse_xml};
use thiserror::Error;

/// Errors returned while parsing or validating a Metadata API manifest XML file.
#[derive(Debug, Error)]
#[non_exhaustive]
pub enum ManifestError {
    /// Failed to parse manifest XML syntax.
    #[error("invalid manifest XML in {path}: {source}")]
    InvalidXml {
        /// Manifest path or description.
        path: String,
        /// Underlying XML error.
        #[source]
        source: crate::xml::XmlError,
    },

    /// Root element is not `<Package>` or a `<types>` block is missing `<name>`.
    #[error("invalid manifest structure in {path}: {reason}")]
    InvalidStructure {
        /// Manifest path or description.
        path: String,
        /// Structural validation reason.
        reason: String,
    },
}

/// Controls how empty decomposed parents are represented when generating `package.xml`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
#[non_exhaustive]
pub enum ManifestPurpose {
    /// Standard conversion manifest (omits empty decomposed parents, matching default SDR deploy).
    #[default]
    Standard,
    /// Deployment manifest (omits empty decomposed parents that have independently addressable children).
    Deploy,
    /// Retrieval manifest (retains empty decomposed parents so org retrieves fetch them).
    Retrieve,
}

/// Destructive changes phase relative to metadata deployment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum DestructiveChangesKind {
    /// Deleted before deploying other changes (`destructiveChangesPre.xml`).
    Pre,
    /// Deleted after deploying other changes (`destructiveChangesPost.xml`).
    Post,
}

impl DestructiveChangesKind {
    /// Returns the canonical MDAPI manifest filename for this destructive change phase.
    pub fn filename(self) -> &'static str {
        match self {
            Self::Pre => "destructiveChangesPre.xml",
            Self::Post => "destructiveChangesPost.xml",
        }
    }
}

impl fmt::Display for DestructiveChangesKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.filename())
    }
}

/// A `<types>` section inside a Metadata API [`PackageManifest`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestTypeMembers {
    /// Metadata type API name (e.g., `"CustomField"`).
    pub name: String,
    /// Sorted list of member full names (or `"*"`).
    pub members: Vec<String>,
}

/// Parsed or generated Salesforce Metadata API manifest (`package.xml` or `destructiveChanges*.xml`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageManifest {
    /// Type/member sections sorted by type name.
    pub types: Vec<ManifestTypeMembers>,
    /// Metadata API version string (e.g., `"59.0"`).
    pub version: String,
    /// Optional package `fullName`.
    pub full_name: Option<String>,
}

/// Parses a Metadata API manifest XML string into a [`PackageManifest`].
///
/// # Errors
///
/// Returns [`ManifestError::InvalidXml`] if XML parsing fails or
/// [`ManifestError::InvalidStructure`] if the root is not `<Package>` or a `<types>`
/// entry lacks a non-empty `<name>`.
pub fn parse_manifest(
    contents: &str,
    source_label: &Path,
) -> Result<PackageManifest, ManifestError> {
    let label = source_label.display().to_string();
    let doc = parse_xml(contents).map_err(|source| ManifestError::InvalidXml {
        path: label.clone(),
        source,
    })?;

    if doc.root.name != "Package" {
        return Err(ManifestError::InvalidStructure {
            path: label,
            reason: format!("expected <Package> root element, found <{}>", doc.root.name),
        });
    }

    let mut types = Vec::new();
    for type_el in doc.root.child_elements().filter(|el| el.name == "types") {
        let Some(name) = type_el
            .child_element("name")
            .and_then(XmlElement::text_content)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            return Err(ManifestError::InvalidStructure {
                path: label,
                reason: "<types> element is missing a non-empty <name>".to_owned(),
            });
        };
        let members = type_el
            .child_elements()
            .filter(|el| el.name == "members")
            .filter_map(XmlElement::text_content)
            .map(ToOwned::to_owned)
            .collect();
        types.push(ManifestTypeMembers {
            name: name.to_owned(),
            members,
        });
    }

    let version = doc
        .root
        .child_element("version")
        .and_then(XmlElement::text_content)
        .unwrap_or_default()
        .to_owned();
    let full_name = doc
        .root
        .child_element("fullName")
        .and_then(XmlElement::text_content)
        .map(ToOwned::to_owned);

    Ok(PackageManifest {
        types,
        version,
        full_name,
    })
}

/// Serializes a [`PackageManifest`] to Salesforce `package.xml` format with 4-space indentation.
pub fn render_manifest_xml(manifest: &PackageManifest) -> String {
    let mut root = XmlElement::new("Package");

    for entry in &manifest.types {
        let mut type_el = XmlElement::new("types");
        for member in &entry.members {
            let mut m = XmlElement::new("members");
            m.children.push(XmlNode::Text(member.clone()));
            type_el.children.push(XmlNode::Element(m));
        }
        let mut name_el = XmlElement::new("name");
        name_el.children.push(XmlNode::Text(entry.name.clone()));
        type_el.children.push(XmlNode::Element(name_el));
        root.children.push(XmlNode::Element(type_el));
    }

    let mut ver_el = XmlElement::new("version");
    ver_el
        .children
        .push(XmlNode::Text(manifest.version.clone()));
    root.children.push(XmlNode::Element(ver_el));

    if let Some(full_name) = &manifest.full_name {
        let mut fn_el = XmlElement::new("fullName");
        fn_el.children.push(XmlNode::Text(full_name.clone()));
        root.children.push(XmlNode::Element(fn_el));
    }

    XmlDocument::new_metadata(root).to_xml_string()
}
