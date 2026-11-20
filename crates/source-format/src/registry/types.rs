//! Typed representation of `metadataRegistry.json`, presets, and `stdValueSetRegistry.json`.

use std::fmt;
use std::str::FromStr;

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};

use super::error::RegistryError;

/// Root metadata registry mapping types, suffixes, strict directories, and child types.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetadataRegistry {
    /// Map of lowercased metadata type ID to [`MetadataType`].
    pub types: IndexMap<String, MetadataType>,
    /// Map of file suffix (without leading `.`) to metadata type ID.
    pub suffixes: IndexMap<String, String>,
    /// Map of strict directory name to metadata type ID.
    #[serde(rename = "strictDirectoryNames")]
    pub strict_directory_names: IndexMap<String, String>,
    /// Map of lowercased child type ID to parent type ID.
    #[serde(rename = "childTypes")]
    pub child_types: IndexMap<String, String>,
}

/// Partial registry overlay used by presets and `registryCustomizations` in `sfdx-project.json`.
///
/// Empty string values in `suffixes`, `strict_directory_names`, or `child_types` remove the
/// corresponding entries from the merged registry, matching SDR's preset semantics.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetadataRegistryPatch {
    /// Metadata types to add or override by lowercased type ID.
    #[serde(default)]
    pub types: IndexMap<String, MetadataType>,
    /// File suffixes to add, override, or remove (`""`).
    #[serde(default)]
    pub suffixes: IndexMap<String, String>,
    /// Strict directory names to add, override, or remove (`""`).
    #[serde(rename = "strictDirectoryNames", default)]
    pub strict_directory_names: IndexMap<String, String>,
    /// Child-to-parent type mappings to add, override, or remove (`""`).
    #[serde(rename = "childTypes", default)]
    pub child_types: IndexMap<String, String>,
}

/// Definition of a Salesforce metadata type or child metadata type.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetadataType {
    /// Unique identifier of the metadata type (usually the lowercased API name).
    pub id: String,
    /// API / XML root name of the metadata type.
    pub name: String,
    /// Directory name where components of this type reside.
    #[serde(rename = "directoryName")]
    pub directory_name: String,
    /// Deprecated indicator that components reside in folders; prefer `folder_type`.
    #[serde(rename = "inFolder", default)]
    pub in_folder: Option<bool>,
    /// File suffix (without leading `.`).
    #[serde(default)]
    pub suffix: Option<String>,
    /// Override for the default `-meta.xml` suffix (e.g., `_meta.json` or `schema.json`).
    #[serde(rename = "metaFileSuffix", default)]
    pub meta_file_suffix: Option<String>,
    /// Whether components must reside directly inside `directory_name`.
    #[serde(rename = "strictDirectoryName", default)]
    pub strict_directory_name: Option<bool>,
    /// Separator between component name and version (e.g., `#` for `AiAgentDefinitionVersion`).
    #[serde(rename = "versionSeparator", default)]
    pub version_separator: Option<String>,
    /// Whether to ignore the path-derived name and always use `name` (e.g., `CustomLabels`).
    #[serde(rename = "ignoreParsedFullName", default)]
    pub ignore_parsed_full_name: Option<bool>,
    /// For folder types, the ID of the content type stored in those folders.
    #[serde(rename = "folderContentType", default)]
    pub folder_content_type: Option<String>,
    /// For in-folder or nested types, the ID of the containing folder/parent type.
    #[serde(rename = "folderType", default)]
    pub folder_type: Option<String>,
    /// Whether the parent name should be omitted when constructing child `fullName`.
    #[serde(rename = "ignoreParentName", default)]
    pub ignore_parent_name: Option<bool>,
    /// XML element name used for this child type inside its parent XML.
    #[serde(rename = "xmlElementName", default)]
    pub xml_element_name: Option<String>,
    /// Upstream alias of `xmlElementName` present in `decomposeSharingRulesBeta.json`.
    #[serde(rename = "xmlElement", default)]
    pub xml_element: Option<String>,
    /// Deprecated file suffix rewritten to `suffix` when converting to metadata format.
    #[serde(rename = "legacySuffix", default)]
    pub legacy_suffix: Option<String>,
    /// XML element name providing the unique identifier of a child element.
    #[serde(rename = "uniqueIdElement", default)]
    pub unique_id_element: Option<String>,
    /// Whether this type is addressable in a Metadata API manifest (`true` by default).
    #[serde(rename = "isAddressable", default)]
    pub is_addressable: Option<bool>,
    /// Whether child components require the parent when deploying or retrieving.
    #[serde(rename = "unaddressableWithoutParent", default)]
    pub unaddressable_without_parent: Option<bool>,
    /// Whether `*` and explicit member names can coexist in a manifest.
    #[serde(rename = "supportsWildcardAndName", default)]
    pub supports_wildcard_and_name: Option<bool>,
    /// Whether individual files inside a multi-file component can be deleted independently.
    #[serde(rename = "supportsPartialDelete", default)]
    pub supports_partial_delete: Option<bool>,
    /// Target type ID when this entry is an alias.
    #[serde(rename = "aliasFor", default)]
    pub alias_for: Option<String>,
    /// Child metadata type definitions for composite/decomposed parent types.
    #[serde(default)]
    pub children: Option<ChildrenConfig>,
    /// Adapter, transformer, decomposition, and recomposition strategy configuration.
    #[serde(default)]
    pub strategies: Option<TypeStrategies>,
}

impl MetadataType {
    /// Returns whether components of this type are addressable in a manifest.
    pub fn addressable(&self) -> bool {
        self.is_addressable.unwrap_or(true)
    }

    /// Returns whether components of this type require a strict directory name.
    pub fn strict_directory(&self) -> bool {
        self.strict_directory_name.unwrap_or(false)
    }

    /// Returns whether components of this type are stored inside folders.
    pub fn stored_in_folder(&self) -> bool {
        self.in_folder.unwrap_or(false)
    }

    /// Returns the XML element tag name used inside a parent document.
    pub fn effective_xml_element(&self) -> &str {
        self.xml_element_name
            .as_deref()
            .or(self.xml_element.as_deref())
            .unwrap_or(&self.directory_name)
    }
}

/// Child metadata type indices under a parent [`MetadataType`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChildrenConfig {
    /// Map of child type ID to child [`MetadataType`].
    pub types: IndexMap<String, MetadataType>,
    /// Map of child file suffix to child type ID.
    pub suffixes: IndexMap<String, String>,
    /// Map of child directory name (or XML tag name) to child type ID.
    #[serde(default)]
    pub directories: Option<IndexMap<String, String>>,
}

/// Resolution and conversion strategies declared for a [`MetadataType`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TypeStrategies {
    /// Source resolution adapter strategy.
    pub adapter: AdapterStrategy,
    /// Conversion transformer strategy.
    #[serde(default)]
    pub transformer: Option<TransformerStrategy>,
    /// Child file layout strategy for decomposed types.
    #[serde(default)]
    pub decomposition: Option<DecompositionStrategy>,
    /// Parent initialization strategy when recomposing children.
    #[serde(default)]
    pub recomposition: Option<RecompositionStrategy>,
}

/// Source resolution adapter strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub enum AdapterStrategy {
    /// Single file or folder-based metadata (`DefaultSourceAdapter`).
    #[serde(rename = "default")]
    #[default]
    Default,
    /// Content file paired with `-meta.xml` sharing the same extension.
    #[serde(rename = "matchingContentFile")]
    MatchingContentFile,
    /// Directory or arbitrary content file paired with `-meta.xml`.
    #[serde(rename = "mixedContent")]
    MixedContent,
    /// Directory bundle (LWC, Aura, `WaveTemplate`, `GenAi`, etc.).
    #[serde(rename = "bundle")]
    Bundle,
    /// Parent metadata decomposed into child `-meta.xml` files.
    #[serde(rename = "decomposed")]
    Decomposed,
    /// Parent `-meta.xml` paired with non-XML schema sibling (`.yaml`).
    #[serde(rename = "partiallyDecomposed")]
    PartiallyDecomposed,
    /// Nested `DigitalExperienceBundle` / `DigitalExperience` tree.
    #[serde(rename = "digitalExperience")]
    DigitalExperience,
    /// `UIBundle` directory bundle requiring `<name>.uibundle-meta.xml`.
    #[serde(rename = "uiBundles")]
    UiBundles,
}

/// Conversion transformer strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[non_exhaustive]
pub enum TransformerStrategy {
    /// Standard path/meta-suffix rewriting (`DefaultMetadataTransformer`).
    #[serde(rename = "standard")]
    #[default]
    Standard,
    /// Parent/child XML decomposition and recomposition.
    #[serde(rename = "decomposed")]
    Decomposed,
    /// Non-decomposed parent XML with addressable child elements (`CustomLabels`).
    #[serde(rename = "nonDecomposed")]
    NonDecomposed,
    /// Static resource MIME extension mapping and zip/unzip handling.
    #[serde(rename = "staticResource")]
    StaticResource,
    /// `CustomLabels` beta2 file-per-label decomposition.
    #[serde(rename = "decomposedLabels")]
    DecomposedLabels,
    /// `PermissionSet` beta2 object-settings decomposition.
    #[serde(rename = "decomposedPermissionSet")]
    DecomposedPermissionSet,
    /// `ExternalServiceRegistration` OAS schema extraction.
    #[serde(rename = "decomposeExternalServiceRegistration")]
    DecomposeExternalServiceRegistration,
    /// `UIBundle` deploy-time descriptor validation.
    #[serde(rename = "uiBundle")]
    UiBundle,
    /// `Profile` multi-package-directory merging.
    #[serde(rename = "profile")]
    Profile,
}

/// Child file placement strategy during decomposition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum DecompositionStrategy {
    /// Child files sit directly in the parent component directory.
    #[serde(rename = "topLevel")]
    TopLevel,
    /// Child files sit inside subdirectories named after each child type.
    #[serde(rename = "folderPerType")]
    FolderPerType,
}

/// Parent initialization strategy during recomposition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum RecompositionStrategy {
    /// Start recomposition from an empty parent XML element rather than existing parent XML.
    #[serde(rename = "startEmpty")]
    StartEmpty,
}

/// Built-in source behavior option presets shipped with SDR.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum Preset {
    /// Decompose `CustomLabels` inside a `CustomLabels/` directory (deprecated beta1).
    DecomposeCustomLabelsBeta,
    /// Decompose `CustomLabels` directly as `labels/*.label-meta.xml`.
    DecomposeCustomLabelsBeta2,
    /// Decompose `ExternalServiceRegistration` schema into sibling `.yaml`.
    DecomposeExternalServiceRegistrationBeta,
    /// Decompose `PermissionSet` into individual child files per type directory.
    DecomposePermissionSetBeta,
    /// Decompose `PermissionSet` with `objectSettings/` grouping.
    DecomposePermissionSetBeta2,
    /// Decompose `SharingRules` into rule subdirectories.
    DecomposeSharingRulesBeta,
    /// Decompose `Workflow` into rule/alert/task subdirectories.
    DecomposeWorkflowBeta,
}

impl Preset {
    /// All built-in presets in canonical order.
    pub const ALL: &[Self] = &[
        Self::DecomposeCustomLabelsBeta,
        Self::DecomposeCustomLabelsBeta2,
        Self::DecomposeExternalServiceRegistrationBeta,
        Self::DecomposePermissionSetBeta,
        Self::DecomposePermissionSetBeta2,
        Self::DecomposeSharingRulesBeta,
        Self::DecomposeWorkflowBeta,
    ];

    /// Returns the preset identifier string used in `sfdx-project.json`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::DecomposeCustomLabelsBeta => "decomposeCustomLabelsBeta",
            Self::DecomposeCustomLabelsBeta2 => "decomposeCustomLabelsBeta2",
            Self::DecomposeExternalServiceRegistrationBeta => {
                "decomposeExternalServiceRegistrationBeta"
            }
            Self::DecomposePermissionSetBeta => "decomposePermissionSetBeta",
            Self::DecomposePermissionSetBeta2 => "decomposePermissionSetBeta2",
            Self::DecomposeSharingRulesBeta => "decomposeSharingRulesBeta",
            Self::DecomposeWorkflowBeta => "decomposeWorkflowBeta",
        }
    }
}

impl fmt::Display for Preset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Preset {
    type Err = RegistryError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "decomposeCustomLabelsBeta" => Ok(Self::DecomposeCustomLabelsBeta),
            "decomposeCustomLabelsBeta2" => Ok(Self::DecomposeCustomLabelsBeta2),
            "decomposeExternalServiceRegistrationBeta" => {
                Ok(Self::DecomposeExternalServiceRegistrationBeta)
            }
            "decomposePermissionSetBeta" => Ok(Self::DecomposePermissionSetBeta),
            "decomposePermissionSetBeta2" => Ok(Self::DecomposePermissionSetBeta2),
            "decomposeSharingRulesBeta" => Ok(Self::DecomposeSharingRulesBeta),
            "decomposeWorkflowBeta" => Ok(Self::DecomposeWorkflowBeta),
            _ => Err(RegistryError::InvalidPreset { name: s.to_owned() }),
        }
    }
}

/// Contents of `stdValueSetRegistry.json`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StandardValueSetRegistry {
    /// Full names of all standard value sets supported by the Metadata API.
    pub fullnames: Vec<String>,
}
