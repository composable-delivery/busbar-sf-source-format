//! Metadata format transformers matching SDR's `src/convert/transformers/*`.

use std::collections::{BTreeSet, HashSet};
use std::io::{Cursor, Read as _, Write as _};
use std::path::{Component, Path, PathBuf};
use std::sync::OnceLock;

use crate::registry::{
    AdapterStrategy, DecompositionStrategy, MetadataType, RegistryAccess, TransformerStrategy,
};
use crate::tree::{TreeContainer, ensure_no_symlink_on_path, ensure_within_root};
use crate::xml::{XmlDocument, XmlElement, XmlNode, parse_xml};
use indexmap::IndexMap;
use rayon::prelude::*;
use regex::Regex;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, PreparedZipFile, ZipArchive, ZipWriter};

use crate::convert::error::ConvertError;
use crate::convert::finalizers::ConvertContext;
use crate::convert::replacements::apply_replacements;
use crate::convert::types::{TargetFormat, WriteAction, WriteInfo, WriteSource};
use crate::convert::ui_bundle::validate_ui_bundle_for_deploy;
use crate::convert::writer::resolve_write_source;
use crate::io_pool::io_install;
use crate::path_util::{
    TrimOccurrence, calculate_relative_path, ext_name, parent_name, path_base_name, trim_until,
};
use crate::resolve::{ComponentProps, ComponentSet, MetadataMember, ResolveError, SourceComponent};

const META_XML_SUFFIX: &str = "-meta.xml";
const DEFAULT_CONTENT_TYPE: &str = "application/octet-stream";

static ORIGINAL_SUFFIX_RE: OnceLock<Regex> = OnceLock::new();

fn original_suffix_regex() -> &'static Regex {
    ORIGINAL_SUFFIX_RE.get_or_init(|| {
        Regex::new(r"(.)([a-zA-Z]+)-meta\.xml$").expect("suffix regex is a valid literal")
    })
}

/// Dispatches conversion to Metadata API format according to `component`'s configured
/// [`TransformerStrategy`].
pub(crate) fn transform_to_metadata(
    context: &mut ConvertContext,
    registry: &RegistryAccess,
    component: &SourceComponent,
) -> Result<Vec<WriteInfo>, ConvertError> {
    let effective_type = component
        .parent()
        .map_or(component.metadata_type(), SourceComponent::metadata_type);
    let strategy = transformer_strategy(effective_type);
    match strategy {
        TransformerStrategy::Standard => {
            default_write_infos(component, TargetFormat::Metadata, None)
        }
        TransformerStrategy::Decomposed | TransformerStrategy::NonDecomposed => {
            context.recomposition.add_component(component.clone())?;
            Ok(Vec::new())
        }
        TransformerStrategy::StaticResource => static_resource_to_metadata(component),
        TransformerStrategy::UiBundle => ui_bundle_to_metadata(component),
        TransformerStrategy::DecomposedLabels => {
            decomposed_labels_to_metadata(context, registry, component)
        }
        TransformerStrategy::DecomposedPermissionSet => {
            decomposed_ps_to_metadata(context, registry, component)
        }
        TransformerStrategy::DecomposeExternalServiceRegistration => {
            decomposed_esr_to_metadata(context, registry, component)
        }
        TransformerStrategy::Profile => {
            context.profile_metadata.add(component.clone())?;
            Ok(Vec::new())
        }
    }
}

/// Dispatches conversion to SFDX source format according to `component`'s configured
/// [`TransformerStrategy`].
/// Controls whether source transforms inspect `default_directory` on disk before emitting an
/// otherwise empty decomposed parent XML.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ParentDiskCheck {
    CheckDisk,
    AlwaysEmit,
}

pub(crate) fn transform_to_source(
    context: &mut ConvertContext,
    registry: &RegistryAccess,
    default_directory: Option<&Path>,
    component: &SourceComponent,
    merge_with: Option<&SourceComponent>,
    merge_set: Option<&ComponentSet>,
    parent_check: ParentDiskCheck,
) -> Result<Vec<WriteInfo>, ConvertError> {
    let effective_type = component
        .parent()
        .map_or(component.metadata_type(), SourceComponent::metadata_type);
    let strategy = transformer_strategy(effective_type);
    match strategy {
        TransformerStrategy::Standard | TransformerStrategy::UiBundle => {
            default_write_infos(component, TargetFormat::Source, merge_with)
        }
        TransformerStrategy::Decomposed => decomposed_to_source(
            context,
            default_directory,
            component,
            merge_with,
            parent_check,
        ),
        TransformerStrategy::NonDecomposed => non_decomposed_to_source(context, component),
        TransformerStrategy::StaticResource => {
            static_resource_to_source(default_directory, component, merge_with)
        }
        TransformerStrategy::DecomposedLabels => {
            decomposed_labels_to_source(registry, component, merge_with, merge_set)
        }
        TransformerStrategy::DecomposedPermissionSet => {
            decomposed_ps_to_source(default_directory, component, merge_with, parent_check)
        }
        TransformerStrategy::DecomposeExternalServiceRegistration => {
            decomposed_esr_to_source(registry, component)
        }
        TransformerStrategy::Profile => match (merge_set, merge_with) {
            (Some(_), Some(mw)) => {
                context.profile_metadata.add_source(component, mw)?;
                Ok(Vec::new())
            }
            _ => default_write_infos(component, TargetFormat::Source, merge_with),
        },
    }
}

/// Returns the effective [`TransformerStrategy`] for `component` (inheriting from parent).
pub(crate) fn effective_transformer_strategy(component: &SourceComponent) -> TransformerStrategy {
    let effective_type = component
        .parent()
        .map_or(component.metadata_type(), SourceComponent::metadata_type);
    transformer_strategy(effective_type)
}

/// Output produced by stateless source-format transformations safe to execute in parallel.
pub(crate) struct SourceTransformOutput {
    pub infos: Vec<WriteInfo>,
    pub deferred: Vec<(String, WriteInfo)>,
}

/// Runs stateless SFDX source transforms when `merge_set` is `None`, returning `Some` for
/// parallel-safe strategies or `None` when [`ConvertContext`] state mutation is required.
pub(crate) fn try_transform_to_source_parallel(
    registry: &RegistryAccess,
    default_directory: Option<&Path>,
    component: &SourceComponent,
    parent_check: ParentDiskCheck,
) -> Option<Result<SourceTransformOutput, ConvertError>> {
    match effective_transformer_strategy(component) {
        TransformerStrategy::Standard | TransformerStrategy::UiBundle => Some(
            default_write_infos(component, TargetFormat::Source, None).map(|infos| {
                SourceTransformOutput {
                    infos,
                    deferred: Vec::new(),
                }
            }),
        ),
        TransformerStrategy::Decomposed => Some(decomposed_to_source_pure(
            default_directory,
            component,
            parent_check,
        )),
        TransformerStrategy::StaticResource => Some(
            static_resource_to_source(default_directory, component, None).map(|infos| {
                SourceTransformOutput {
                    infos,
                    deferred: Vec::new(),
                }
            }),
        ),
        TransformerStrategy::DecomposedLabels => Some(
            decomposed_labels_to_source(registry, component, None, None).map(|infos| {
                SourceTransformOutput {
                    infos,
                    deferred: Vec::new(),
                }
            }),
        ),
        TransformerStrategy::DecomposedPermissionSet => Some(
            decomposed_ps_to_source(default_directory, component, None, parent_check).map(
                |infos| SourceTransformOutput {
                    infos,
                    deferred: Vec::new(),
                },
            ),
        ),
        TransformerStrategy::DecomposeExternalServiceRegistration => Some(
            decomposed_esr_to_source(registry, component).map(|infos| SourceTransformOutput {
                infos,
                deferred: Vec::new(),
            }),
        ),
        TransformerStrategy::Profile => Some(
            default_write_infos(component, TargetFormat::Source, None).map(|infos| {
                SourceTransformOutput {
                    infos,
                    deferred: Vec::new(),
                }
            }),
        ),
        TransformerStrategy::NonDecomposed => None,
    }
}

/// Runs stateless MDAPI transforms for strategies that do not accumulate into [`ConvertContext`].
pub(crate) fn try_transform_to_metadata_parallel(
    component: &SourceComponent,
) -> Option<Result<Vec<WriteInfo>, ConvertError>> {
    match effective_transformer_strategy(component) {
        TransformerStrategy::Standard => {
            Some(default_write_infos(component, TargetFormat::Metadata, None))
        }
        TransformerStrategy::StaticResource => Some(static_resource_to_metadata(component)),
        TransformerStrategy::UiBundle => Some(ui_bundle_to_metadata(component)),
        TransformerStrategy::Decomposed
        | TransformerStrategy::NonDecomposed
        | TransformerStrategy::DecomposedLabels
        | TransformerStrategy::DecomposedPermissionSet
        | TransformerStrategy::DecomposeExternalServiceRegistration
        | TransformerStrategy::Profile => None,
    }
}

fn transformer_strategy(metadata_type: &MetadataType) -> TransformerStrategy {
    metadata_type
        .strategies
        .as_ref()
        .and_then(|s| s.transformer)
        .unwrap_or(TransformerStrategy::Standard)
}

fn default_write_infos(
    component: &SourceComponent,
    target_format: TargetFormat,
    merge_with: Option<&SourceComponent>,
) -> Result<Vec<WriteInfo>, ConvertError> {
    let mut infos = Vec::new();
    for path in component.walk_content()? {
        let output = get_content_source_destination(&path, target_format, component, merge_with)?;
        let replacements = component
            .replacements
            .get(&path)
            .cloned()
            .unwrap_or_default();
        infos.push(WriteInfo {
            output,
            action: WriteAction::Write(WriteSource::TreeFile {
                tree: component.tree().clone(),
                path,
                replacements,
            }),
        });
    }
    if let Some(xml) = component.xml() {
        let output = get_xml_destination(target_format, component, merge_with)?;
        let replacements = component.replacements.get(xml).cloned().unwrap_or_default();
        infos.push(WriteInfo {
            output,
            action: WriteAction::Write(WriteSource::TreeFile {
                tree: component.tree().clone(),
                path: xml.to_path_buf(),
                replacements,
            }),
        });
    }
    Ok(infos)
}

fn get_content_source_destination(
    source: &Path,
    target_format: TargetFormat,
    component: &SourceComponent,
    merge_with: Option<&SourceComponent>,
) -> Result<PathBuf, ConvertError> {
    if target_format == TargetFormat::Source
        && let Some(mw) = merge_with
        && let Some(mw_content) = mw.content()
    {
        if let Some(comp_content) = component.content()
            && component.tree().is_directory(comp_content)?
        {
            let is_digital_experience = component
                .metadata_type()
                .strategies
                .as_ref()
                .is_some_and(|s| s.adapter == AdapterStrategy::DigitalExperience);
            if is_digital_experience {
                let file = source.file_name().unwrap_or_default();
                let file_str = file.to_string_lossy();
                let dir = mw_content.join(parent_name(source));
                let dir_exists =
                    mw.tree().exists(&dir) && mw.tree().is_directory(&dir).unwrap_or(false);
                if file_str == "content.json"
                    || file_str == "_meta.json"
                    || (!dir_exists && file_str != "mobile.json" && file_str != "tablet.json")
                {
                    return Ok(mw_content.join(file));
                }
                return Ok(dir.join(file));
            }
            let rel = trim_until(source, path_base_name(comp_content), TrimOccurrence::First);
            return Ok(mw_content.parent().unwrap_or(mw_content).join(rel));
        }
        return Ok(mw_content.to_path_buf());
    }
    Ok(component.package_relative_path(source, target_format)?)
}

/// Computes the destination XML path for standard (non-decomposed) components in `target_format`.
pub(crate) fn get_xml_destination(
    target_format: TargetFormat,
    component: &SourceComponent,
    merge_with: Option<&SourceComponent>,
) -> Result<PathBuf, ConvertError> {
    if target_format == TargetFormat::Source
        && let Some(mw) = merge_with
        && let Some(mw_xml) = mw.xml()
    {
        return Ok(mw_xml.to_path_buf());
    }

    let Some(xml) = component.xml() else {
        return Err(ConvertError::Resolve(ResolveError::ExpectedSourceFiles {
            path: component
                .content()
                .map_or_else(|| PathBuf::from(component.full_name()), Path::to_path_buf),
            type_name: component.metadata_type().name.clone(),
        }));
    };

    let comp_type = component.metadata_type();
    let rel = component.package_relative_path(xml, target_format)?;
    let mut xml_dest = path_to_posix(&rel);

    if component.content().is_none() && comp_type.id != "digitalexperiencebundle" {
        match target_format {
            TargetFormat::Metadata => {
                if comp_type.folder_content_type.is_some() {
                    if let Some(sfx) = &comp_type.suffix {
                        xml_dest = xml_dest.replacen(&format!(".{sfx}"), "", 1);
                    }
                } else if let Some(pos) = xml_dest.rfind(META_XML_SUFFIX) {
                    xml_dest = format!(
                        "{}{}",
                        &xml_dest[..pos],
                        &xml_dest[pos + META_XML_SUFFIX.len()..]
                    );
                }
            }
            TargetFormat::Source => {
                if comp_type.folder_content_type.is_some() {
                    let sfx = comp_type.suffix.as_deref().unwrap_or("");
                    xml_dest =
                        xml_dest.replacen(META_XML_SUFFIX, &format!(".{sfx}{META_XML_SUFFIX}"), 1);
                } else {
                    xml_dest.push_str(META_XML_SUFFIX);
                }
            }
        }
    } else if let Some(suffix) = &comp_type.suffix {
        if comp_type.name == "Document"
            && target_format == TargetFormat::Metadata
            && let Some(content) = component.content()
        {
            let pat = format!(".{suffix}{META_XML_SUFFIX}");
            if let Some(stripped) = xml_dest.strip_suffix(&pat) {
                xml_dest = format!("{stripped}.{}{META_XML_SUFFIX}", ext_name(content));
            }
        } else {
            xml_dest = original_suffix_regex()
                .replace(&xml_dest, format!(".{suffix}{META_XML_SUFFIX}"))
                .into_owned();
        }
    }

    if let (Some(legacy), Some(suffix)) = (&comp_type.legacy_suffix, &comp_type.suffix)
        && xml_dest.contains(legacy.as_str())
    {
        xml_dest = xml_dest.replacen(legacy.as_str(), suffix.as_str(), 1);
    }

    Ok(PathBuf::from(xml_dest))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AloneChildPolicy {
    DeferEmptyParent,
    Standard,
}

struct DecomposedParentSplit {
    parent_root: XmlElement,
    extracted_children: Vec<ExtractedDecomposedChild>,
    alone_policy: AloneChildPolicy,
}

fn decomposed_to_source_pure(
    default_directory: Option<&Path>,
    component: &SourceComponent,
    parent_check: ParentDiskCheck,
) -> Result<SourceTransformOutput, ConvertError> {
    let force_ignore = component.force_ignore();
    let output_file = get_decomposed_output_file(component, None)?;
    if force_ignore.denies(&output_file) {
        return Ok(SourceTransformOutput {
            infos: Vec::new(),
            deferred: Vec::new(),
        });
    }

    let split = split_decomposed_parent(component)?;
    let parent_doc_out = XmlDocument::new_metadata(split.parent_root.clone());
    let parent_bytes = parent_doc_out.to_xml_string().into_bytes();
    let parent_default_output = if split.alone_policy == AloneChildPolicy::DeferEmptyParent {
        Some(get_decomposed_default_output(component)?)
    } else {
        None
    };

    let mut write_infos_for_children = Vec::with_capacity(split.extracted_children.len());
    let mut deferred = Vec::new();
    let parent_tree = component.tree();

    for item in split.extracted_children {
        let child_comp = SourceComponent::new(
            ComponentProps {
                name: item.entry_name,
                metadata_type: item.child_type.clone(),
                xml: None,
                content: None,
                parent: Some(Box::new(component.clone())),
                parent_type: None,
            },
            parent_tree.clone(),
            force_ignore.clone(),
        );
        let default_child_path = get_decomposed_default_output(&child_comp)?;
        if force_ignore.denies(&default_child_path) {
            continue;
        }
        if item.child_type.unaddressable_without_parent == Some(true)
            && let Some(ref parent_out) = parent_default_output
        {
            deferred.push((
                component_state_key(&child_comp),
                WriteInfo {
                    output: parent_out.clone(),
                    action: WriteAction::Write(WriteSource::Bytes(parent_bytes.clone())),
                },
            ));
        }

        let mut child_root = item.element;
        child_root.name.clone_from(&item.child_type.name);
        let child_bytes = XmlDocument::new_metadata(child_root)
            .to_xml_string()
            .into_bytes();
        write_infos_for_children.push(WriteInfo {
            output: default_child_path,
            action: WriteAction::Write(WriteSource::Bytes(child_bytes)),
        });
    }

    let parent_has_real_values = split.parent_root.has_real_values();
    let exists_on_disk = parent_check == ParentDiskCheck::CheckDisk
        && default_directory.is_some_and(|dir| dir.join(&output_file).exists());
    if parent_has_real_values || !exists_on_disk {
        write_infos_for_children.push(WriteInfo {
            output: output_file,
            action: WriteAction::Write(WriteSource::Bytes(parent_bytes)),
        });
    }

    Ok(SourceTransformOutput {
        infos: write_infos_for_children,
        deferred,
    })
}

fn decomposed_to_source(
    context: &mut ConvertContext,
    default_directory: Option<&Path>,
    component: &SourceComponent,
    merge_with: Option<&SourceComponent>,
    parent_check: ParentDiskCheck,
) -> Result<Vec<WriteInfo>, ConvertError> {
    let force_ignore = component.force_ignore();
    let output_file = get_decomposed_output_file(component, merge_with)?;
    if force_ignore.denies(&output_file) {
        return Ok(Vec::new());
    }

    let merge_children = match merge_with {
        Some(mw) => mw.children()?,
        None => Vec::new(),
    };
    let parent_type = component.metadata_type();
    let split = split_decomposed_parent(component)?;
    let parent_doc_out = XmlDocument::new_metadata(split.parent_root.clone());

    let mut write_infos_for_children = Vec::new();
    let mut child_destinations = HashSet::new();

    for item in split.extracted_children {
        emit_decomposed_child_write_info(
            context,
            component,
            merge_with,
            &merge_children,
            &parent_doc_out,
            split.alone_policy,
            item,
            &mut child_destinations,
            &mut write_infos_for_children,
        )?;
    }

    let parent_infos = build_decomposed_parent_writes(
        context,
        default_directory,
        component,
        merge_with,
        &split.parent_root,
        &parent_doc_out,
        parent_check,
    )?;

    let mut delete_infos = Vec::new();
    if merge_with.is_some() && all_children_are_unaddressable(parent_type) {
        for mc in merge_children {
            if let Some(mc_xml) = mc.xml()
                && !child_destinations.contains(mc_xml)
            {
                delete_infos.push(WriteInfo {
                    output: mc_xml.to_path_buf(),
                    action: WriteAction::Delete,
                });
            }
        }
    }

    let mut result = write_infos_for_children;
    result.extend(parent_infos);
    result.extend(delete_infos);
    Ok(result)
}

fn split_decomposed_parent(
    component: &SourceComponent,
) -> Result<DecomposedParentSplit, ConvertError> {
    let parent_type = component.metadata_type();
    let parent_doc = component.parse_xml()?;
    let mut parent_root = XmlElement {
        name: parent_type.name.clone(),
        attributes: parent_doc.root.attributes.clone(),
        children: Vec::new(),
    };

    let mut child_tag_names = HashSet::new();
    let mut extracted_children = Vec::new();

    for node in &parent_doc.root.children {
        if let XmlNode::Element(el) = node
            && let Some(child_type) = resolve_child_type(parent_type, &el.name)?
        {
            child_tag_names.insert(el.name.clone());
            let entry_name = el
                .extract_unique_element(child_type.unique_id_element.as_deref())
                .ok_or_else(|| {
                    let xml_path = component.xml().map_or_else(PathBuf::new, Path::to_path_buf);
                    ResolveError::MissingUniqueElement {
                        element: child_type
                            .unique_id_element
                            .clone()
                            .unwrap_or_else(|| "fullName".to_owned()),
                        child_type: child_type.name.clone(),
                        xml_path,
                    }
                })?
                .to_owned();

            extracted_children.push(ExtractedDecomposedChild {
                child_type: child_type.clone(),
                entry_name,
                element: el.without_xmlns(),
            });
            continue;
        }
        parent_root.children.push(node.clone());
    }

    // Matches JS `composedMetadata.length <= 2` (`@_xmlns` + at most one child array key, and no
    // other attributes or parent child elements/comments).
    let alone_policy = if child_tag_names.len() <= 1 && !parent_root.has_real_values() {
        AloneChildPolicy::DeferEmptyParent
    } else {
        AloneChildPolicy::Standard
    };
    Ok(DecomposedParentSplit {
        parent_root,
        extracted_children,
        alone_policy,
    })
}

#[expect(
    clippy::too_many_arguments,
    reason = "Internal decomposed child writer helper"
)]
fn emit_decomposed_child_write_info(
    context: &mut ConvertContext,
    parent_component: &SourceComponent,
    merge_with: Option<&SourceComponent>,
    merge_children: &[SourceComponent],
    parent_doc_out: &XmlDocument,
    alone_policy: AloneChildPolicy,
    item: ExtractedDecomposedChild,
    child_destinations: &mut HashSet<PathBuf>,
    write_infos_for_children: &mut Vec<WriteInfo>,
) -> Result<(), ConvertError> {
    let force_ignore = parent_component.force_ignore();
    let child_comp = SourceComponent::new(
        ComponentProps {
            name: item.entry_name.clone(),
            metadata_type: item.child_type.clone(),
            xml: None,
            content: None,
            parent: Some(Box::new(parent_component.clone())),
            parent_type: None,
        },
        parent_component.tree().clone(),
        force_ignore.clone(),
    );
    let default_child_path = get_decomposed_default_output(&child_comp)?;
    if force_ignore.denies(&default_child_path) {
        return Ok(());
    }

    if item.child_type.unaddressable_without_parent == Some(true)
        && alone_policy == AloneChildPolicy::DeferEmptyParent
    {
        let parent_bytes = parent_doc_out.to_xml_string().into_bytes();
        let parent_default_output = get_decomposed_default_output(parent_component)?;
        context.decomposition.defer_write(
            component_state_key(&child_comp),
            WriteInfo {
                output: parent_default_output,
                action: WriteAction::Write(WriteSource::Bytes(parent_bytes)),
            },
        );
    }

    let mut child_root = item.element;
    child_root.name.clone_from(&item.child_type.name);
    let child_bytes = XmlDocument::new_metadata(child_root)
        .to_xml_string()
        .into_bytes();

    if let Some(mw) = merge_with {
        let matching_child = merge_children.iter().find(|c| {
            c.metadata_type().id == child_comp.metadata_type().id
                && c.full_name() == child_comp.full_name()
        });
        if let Some(mc) = matching_child {
            let mc_xml = mc.xml().ok_or_else(|| ResolveError::ExpectedSourceFiles {
                path: PathBuf::from(mc.full_name()),
                type_name: mc.metadata_type().name.clone(),
            })?;
            context
                .decomposition
                .mark_merged(component_state_key(&child_comp));
            child_destinations.insert(mc_xml.to_path_buf());
            write_infos_for_children.push(WriteInfo {
                output: mc_xml.to_path_buf(),
                action: WriteAction::Write(WriteSource::Bytes(child_bytes)),
            });
        } else if item.child_type.unaddressable_without_parent == Some(true)
            && let Some(mw_xml) = mw.xml()
        {
            let sfx = item.child_type.suffix.as_deref().unwrap_or("");
            let dest = mw_xml
                .parent()
                .unwrap_or_else(|| Path::new(""))
                .join(format!("{}.{sfx}{META_XML_SUFFIX}", item.entry_name));
            child_destinations.insert(dest.clone());
            write_infos_for_children.push(WriteInfo {
                output: dest,
                action: WriteAction::Write(WriteSource::Bytes(child_bytes)),
            });
        } else {
            child_destinations.insert(default_child_path.clone());
            context.decomposition.defer_write(
                component_state_key(&child_comp),
                WriteInfo {
                    output: default_child_path,
                    action: WriteAction::Write(WriteSource::Bytes(child_bytes)),
                },
            );
        }
    } else {
        child_destinations.insert(default_child_path.clone());
        write_infos_for_children.push(WriteInfo {
            output: default_child_path,
            action: WriteAction::Write(WriteSource::Bytes(child_bytes)),
        });
    }
    Ok(())
}

fn build_decomposed_parent_writes(
    context: &mut ConvertContext,
    default_directory: Option<&Path>,
    component: &SourceComponent,
    merge_with: Option<&SourceComponent>,
    parent_root: &XmlElement,
    parent_doc_out: &XmlDocument,
    parent_check: ParentDiskCheck,
) -> Result<Vec<WriteInfo>, ConvertError> {
    let parent_bytes = parent_doc_out.to_xml_string().into_bytes();
    let parent_has_real_values = parent_root.has_real_values();
    if let Some(mw) = merge_with {
        let output = get_decomposed_output_file(component, Some(mw))?;
        let info = WriteInfo {
            output,
            action: WriteAction::Write(WriteSource::Bytes(parent_bytes)),
        };
        if let Some(mw_xml) = mw.xml() {
            context
                .decomposition
                .mark_merged(component_state_key(component));
            let existing_has_values = mw
                .parse_xml_at(mw_xml)
                .is_ok_and(|doc| doc.root.has_real_values());
            if existing_has_values && !parent_has_real_values {
                return Ok(Vec::new());
            }
            return Ok(vec![info]);
        }
        if parent_has_real_values {
            context
                .decomposition
                .defer_write(component_state_key(component), info);
        }
        return Ok(Vec::new());
    }

    let output = get_decomposed_output_file(component, None)?;
    let exists_on_disk = parent_check == ParentDiskCheck::CheckDisk
        && default_directory.is_some_and(|dir| dir.join(&output).exists());
    if !parent_has_real_values && exists_on_disk {
        Ok(Vec::new())
    } else {
        Ok(vec![WriteInfo {
            output,
            action: WriteAction::Write(WriteSource::Bytes(parent_bytes)),
        }])
    }
}

struct ExtractedDecomposedChild {
    child_type: MetadataType,
    entry_name: String,
    element: XmlElement,
}

fn resolve_child_type<'a>(
    parent_type: &'a MetadataType,
    tag_key: &str,
) -> Result<Option<&'a MetadataType>, ConvertError> {
    let Some(children) = &parent_type.children else {
        return Ok(None);
    };
    let found_id = children
        .types
        .values()
        .find(|c| c.effective_xml_element() == tag_key)
        .map(|c| c.id.as_str())
        .or_else(|| {
            children
                .directories
                .as_ref()
                .and_then(|dirs| dirs.get(tag_key).map(String::as_str))
        });

    let Some(child_id) = found_id else {
        return Ok(None);
    };
    let Some(child_type) = children.types.get(child_id) else {
        return Err(ConvertError::Registry(
            crate::registry::RegistryError::MissingChildTypeDefinition {
                parent_id: parent_type.id.clone(),
                child_id: child_id.to_owned(),
            },
        ));
    };
    Ok(Some(child_type))
}

fn get_decomposed_output_file(
    component: &SourceComponent,
    merge_with: Option<&SourceComponent>,
) -> Result<PathBuf, ConvertError> {
    if let Some(mw) = merge_with
        && let Some(mw_xml) = mw.xml()
    {
        return Ok(mw_xml.to_path_buf());
    }
    get_decomposed_default_output(component)
}

fn get_decomposed_default_output(component: &SourceComponent) -> Result<PathBuf, ConvertError> {
    let full_name = component.full_name();
    let (base_name, child_name) = match full_name.split_once('.') {
        Some((base, tail)) => (base, Some(tail)),
        None => (full_name.as_str(), None),
    };
    let parent_comp = component.parent();
    let rel_type = parent_comp.map_or(component.metadata_type(), SourceComponent::metadata_type);
    let decomp_strategy = parent_comp
        .and_then(|p| p.metadata_type().strategies.as_ref())
        .and_then(|s| s.decomposition);
    let suffix = component.metadata_type().suffix.as_deref().unwrap_or("");
    let filename = format!(
        "{}.{suffix}{META_XML_SUFFIX}",
        child_name.unwrap_or(base_name)
    );
    let sub_output = if decomp_strategy == Some(DecompositionStrategy::FolderPerType) {
        PathBuf::from(&component.metadata_type().directory_name).join(filename)
    } else {
        PathBuf::from(filename)
    };
    let base_dir = calculate_relative_path(
        TargetFormat::Source,
        rel_type,
        component.parent_type(),
        &full_name,
        Path::new(base_name),
    )?;
    Ok(base_dir.join(sub_output))
}

fn all_children_are_unaddressable(metadata_type: &MetadataType) -> bool {
    metadata_type.children.as_ref().is_some_and(|cfg| {
        cfg.types.values().all(|c| {
            c.is_addressable == Some(false) && c.unaddressable_without_parent != Some(true)
        })
    })
}

fn component_state_key(component: &SourceComponent) -> String {
    format!("{}#{}", component.metadata_type().id, component.full_name())
}

fn non_decomposed_to_source(
    context: &mut ConvertContext,
    component: &SourceComponent,
) -> Result<Vec<WriteInfo>, ConvertError> {
    let parent_type = component.metadata_type();
    let Some(children_cfg) = &parent_type.children else {
        return Err(ConvertError::Resolve(ResolveError::ExpectedSourceFiles {
            path: component
                .xml()
                .map_or_else(|| PathBuf::from(component.full_name()), Path::to_path_buf),
            type_name: parent_type.name.clone(),
        }));
    };
    let Some(child_type) = children_cfg.types.values().next() else {
        return Err(ConvertError::Resolve(ResolveError::ExpectedSourceFiles {
            path: component
                .xml()
                .map_or_else(|| PathBuf::from(component.full_name()), Path::to_path_buf),
            type_name: parent_type.name.clone(),
        }));
    };
    let Some(unique_id_elem) = child_type.unique_id_element.as_deref() else {
        return Err(ConvertError::Resolve(ResolveError::MissingUniqueElement {
            element: "uniqueIdElement".to_owned(),
            child_type: child_type.name.clone(),
            xml_path: component.xml().map_or_else(PathBuf::new, Path::to_path_buf),
        }));
    };

    let parent_doc = component.parse_xml()?;
    let xml_element = parent_type.effective_xml_element();

    for el in parent_doc
        .root
        .child_elements()
        .filter(|e| e.name == xml_element)
    {
        let Some(child_name) = el.extract_unique_element(Some(unique_id_elem)) else {
            return Err(ConvertError::Resolve(ResolveError::MissingUniqueElement {
                element: unique_id_elem.to_owned(),
                child_type: child_type.name.clone(),
                xml_path: component.xml().map_or_else(PathBuf::new, Path::to_path_buf),
            }));
        };
        context
            .non_decomposition
            .add_child(child_name.to_owned(), el.without_xmlns(), component);
    }

    Ok(Vec::new())
}

fn is_archive_mime(content_type: &str) -> bool {
    matches!(
        content_type,
        "application/zip" | "application/x-zip-compressed" | "application/jar"
    )
}

fn static_resource_to_metadata(
    component: &SourceComponent,
) -> Result<Vec<WriteInfo>, ConvertError> {
    let Some(content) = component.content() else {
        return Err(ConvertError::Resolve(ResolveError::ExpectedSourceFiles {
            path: PathBuf::from(component.full_name()),
            type_name: component.metadata_type().name.clone(),
        }));
    };
    let Some(xml) = component.xml() else {
        return Err(ConvertError::Resolve(ResolveError::ExpectedSourceFiles {
            path: PathBuf::from(component.full_name()),
            type_name: component.metadata_type().name.clone(),
        }));
    };

    let comp_type = component.metadata_type();
    let suffix = comp_type.suffix.as_deref().unwrap_or("");
    let content_source = if component.tree().is_directory(content)? {
        zip_expanded_static_resource(component, content)?
    } else {
        WriteSource::TreeFile {
            tree: component.tree().clone(),
            path: content.to_path_buf(),
            replacements: component
                .replacements
                .get(content)
                .cloned()
                .unwrap_or_default(),
        }
    };

    let content_out = PathBuf::from(&comp_type.directory_name)
        .join(format!("{}.{suffix}", path_base_name(content)));
    let xml_name = xml.file_name().unwrap_or_default();
    let xml_out = PathBuf::from(&comp_type.directory_name).join(xml_name);

    Ok(vec![
        WriteInfo {
            output: content_out,
            action: WriteAction::Write(content_source),
        },
        WriteInfo {
            output: xml_out,
            action: WriteAction::Write(WriteSource::TreeFile {
                tree: component.tree().clone(),
                path: xml.to_path_buf(),
                replacements: component.replacements.get(xml).cloned().unwrap_or_default(),
            }),
        },
    ])
}

struct ZipStaticEntry {
    rel_posix: String,
    path: PathBuf,
    replacements: Vec<crate::convert::types::MarkedReplacement>,
}

fn zip_expanded_static_resource(
    component: &SourceComponent,
    content: &Path,
) -> Result<WriteSource, ConvertError> {
    let comp_type = component.metadata_type();
    let content_type = read_static_resource_content_type(component)?;
    if !is_archive_mime(&content_type) {
        return Err(ConvertError::StaticResourceExpectedArchive {
            name: component.name().to_owned(),
            content_type,
        });
    }
    let entries = component.walk_content()?;
    if entries.is_empty() {
        return Err(ConvertError::Resolve(ResolveError::ExpectedSourceFiles {
            path: content.to_path_buf(),
            type_name: comp_type.name.clone(),
        }));
    }

    let mut written_dirs: BTreeSet<String> = BTreeSet::new();
    let mut file_items: Vec<ZipStaticEntry> = Vec::with_capacity(entries.len());
    for path in entries {
        let rel = path.strip_prefix(content).unwrap_or(&path);
        let rel_posix = path_to_posix(rel);
        if rel_posix.is_empty() {
            continue;
        }
        let parts: Vec<&str> = rel_posix.split('/').collect();
        for i in 1..parts.len() {
            written_dirs.insert(format!("{}/", parts[..i].join("/")));
        }
        let replacements = component
            .replacements
            .get(&path)
            .cloned()
            .unwrap_or_default();
        file_items.push(ZipStaticEntry {
            rel_posix,
            path,
            replacements,
        });
    }

    if file_items.is_empty() {
        return Err(ConvertError::Resolve(ResolveError::ExpectedSourceFiles {
            path: content.to_path_buf(),
            type_name: comp_type.name.clone(),
        }));
    }

    let mut buf = Cursor::new(Vec::new());
    {
        let mut zip = ZipWriter::new(&mut buf);
        let opts = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .compression_level(Some(9));
        for dir_name in written_dirs {
            zip.add_directory(dir_name, SimpleFileOptions::default())?;
        }

        if file_items.len() >= 16 {
            let tree = component.tree();
            let loaded: Vec<(String, Vec<u8>)> = io_install(|| {
                file_items
                    .into_par_iter()
                    .map(|item| {
                        let data = resolve_write_source(&WriteSource::TreeFile {
                            tree: tree.clone(),
                            path: item.path,
                            replacements: item.replacements,
                        })?;
                        Ok::<_, ConvertError>((item.rel_posix, data))
                    })
                    .collect::<Result<Vec<_>, _>>()
            })?;
            let prepared: Vec<PreparedZipFile> = loaded
                .into_par_iter()
                .map(|(rel_posix, data)| PreparedZipFile::from_data(rel_posix, opts, &data))
                .collect::<Result<Vec<_>, _>>()?;
            zip.add_prepared_files(prepared)?;
        } else {
            for item in file_items {
                let data = resolve_write_source(&WriteSource::TreeFile {
                    tree: component.tree().clone(),
                    path: item.path.clone(),
                    replacements: item.replacements,
                })?;
                zip.start_file(item.rel_posix, opts)?;
                zip.write_all(&data).map_err(|source| ConvertError::Io {
                    path: item.path,
                    source,
                })?;
            }
        }
        zip.finish()?;
    }
    Ok(WriteSource::Bytes(buf.into_inner()))
}

fn static_resource_to_source(
    default_directory: Option<&Path>,
    component: &SourceComponent,
    merge_with: Option<&SourceComponent>,
) -> Result<Vec<WriteInfo>, ConvertError> {
    let Some(content) = component.content() else {
        return Ok(Vec::new());
    };
    let Some(xml) = component.xml() else {
        return Err(ConvertError::Resolve(ResolveError::ExpectedSourceFiles {
            path: PathBuf::from(component.full_name()),
            type_name: component.metadata_type().name.clone(),
        }));
    };

    let content_type = read_static_resource_content_type(component)?;
    let base_content_path = if let Some(mw) = merge_with
        && let Some(mw_content) = mw.content()
    {
        mw_content
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .join(path_base_name(mw_content))
    } else {
        let rel = component.package_relative_path(content, TargetFormat::Source)?;
        rel.parent()
            .unwrap_or_else(|| Path::new(""))
            .join(path_base_name(&rel))
    };

    let should_unzip = is_archive_mime(&content_type)
        && match merge_with {
            None => true,
            Some(mw) => mw
                .content()
                .is_some_and(|mc| mw.tree().is_directory(mc).unwrap_or(false)),
        };

    let xml_dest = if let Some(mw) = merge_with
        && let Some(mw_xml) = mw.xml()
    {
        mw_xml.to_path_buf()
    } else {
        let xml_file = PathBuf::from(xml.file_name().unwrap_or_default());
        component.package_relative_path(&xml_file, TargetFormat::Source)?
    };

    let mut infos = vec![WriteInfo {
        output: xml_dest,
        action: WriteAction::Write(WriteSource::TreeFile {
            tree: component.tree().clone(),
            path: xml.to_path_buf(),
            replacements: Vec::new(),
        }),
    }];

    if should_unzip {
        unzip_static_resource_entries(
            default_directory,
            component,
            content,
            &base_content_path,
            &mut infos,
        )?;
    } else {
        let ext = extension_for_mime(&content_type);
        let mut out_str = base_content_path.to_string_lossy().into_owned();
        out_str.push('.');
        out_str.push_str(ext);
        infos.push(WriteInfo {
            output: PathBuf::from(out_str),
            action: WriteAction::Write(WriteSource::TreeFile {
                tree: component.tree().clone(),
                path: content.to_path_buf(),
                replacements: Vec::new(),
            }),
        });
    }

    Ok(infos)
}

fn unzip_static_resource_entries(
    default_directory: Option<&Path>,
    component: &SourceComponent,
    content: &Path,
    base_content_path: &Path,
    infos: &mut Vec<WriteInfo>,
) -> Result<(), ConvertError> {
    let zip_bytes = component.tree().read_file(content)?;
    let mut archive =
        ZipArchive::new(Cursor::new(zip_bytes)).map_err(|err| ConvertError::BadZipFile {
            path: content.to_path_buf(),
            name: component.name().to_owned(),
            reason: err.to_string(),
        })?;

    let base_dest = if base_content_path.is_absolute() {
        base_content_path.to_path_buf()
    } else if let Some(dir) = default_directory {
        dir.join(base_content_path)
    } else {
        base_content_path.to_path_buf()
    };

    for idx in 0..archive.len() {
        let mut entry = archive
            .by_index(idx)
            .map_err(|err| ConvertError::BadZipFile {
                path: content.to_path_buf(),
                name: component.name().to_owned(),
                reason: err.to_string(),
            })?;
        if entry.is_dir() {
            continue;
        }
        let entry_rel = entry
            .name()
            .map_err(|err| ConvertError::BadZipFile {
                path: content.to_path_buf(),
                name: component.name().to_owned(),
                reason: err.to_string(),
            })?
            .into_owned();
        validate_relative_zip_entry(&entry_rel, &base_dest)?;
        let checked = ensure_within_root(&base_dest, Path::new(&entry_rel))?;
        if default_directory.is_some() || base_content_path.is_absolute() {
            ensure_no_symlink_on_path(&base_dest, &checked)?;
        }
        let dest_for_writer = if base_content_path.is_absolute() {
            checked
        } else {
            base_content_path.join(&entry_rel)
        };
        let mut bytes = Vec::new();
        entry
            .read_to_end(&mut bytes)
            .map_err(|source| ConvertError::Io {
                path: PathBuf::from(&entry_rel),
                source,
            })?;
        infos.push(WriteInfo {
            output: dest_for_writer,
            action: WriteAction::Write(WriteSource::Bytes(bytes)),
        });
    }
    Ok(())
}

fn validate_relative_zip_entry(entry_name: &str, base_dest: &Path) -> Result<(), ConvertError> {
    let rel = Path::new(entry_name);
    if rel.is_absolute()
        || rel.components().any(|c| {
            matches!(
                c,
                Component::ParentDir | Component::Prefix(_) | Component::RootDir
            )
        })
    {
        return Err(ConvertError::Tree(
            crate::tree::TreeError::PathOutsideRoot {
                path: rel.to_path_buf(),
                root: base_dest.to_path_buf(),
            },
        ));
    }
    Ok(())
}

fn read_static_resource_content_type(component: &SourceComponent) -> Result<String, ConvertError> {
    let doc = component.parse_xml()?;
    let Some(el) = doc.root.child_element("contentType") else {
        return Err(ConvertError::StaticResourceMissingContentType {
            name: component.name().to_owned(),
        });
    };
    Ok(el
        .text_content()
        .map_or_else(|| DEFAULT_CONTENT_TYPE.to_owned(), str::to_owned))
}

fn extension_for_mime(content_type: &str) -> &'static str {
    let canonical = match content_type {
        "text/html" => Some("html"),
        "image/jpeg" => Some("jpeg"),
        "application/javascript" | "text/javascript" | "application/x-javascript" => Some("js"),
        "application/x-zip-compressed" => Some("zip"),
        "text/x-haml" => Some("haml"),
        "image/x-png" | "image/png" => Some("png"),
        "text/xml" | "application/xml" => Some("xml"),
        "application/json" => Some("json"),
        "text/css" => Some("css"),
        "text/plain" => Some("txt"),
        "image/svg+xml" => Some("svg"),
        "application/pdf" => Some("pdf"),
        "application/octet-stream" => Some("bin"),
        _ => None,
    };
    if let Some(ext) = canonical {
        return ext;
    }
    mime_guess::get_mime_extensions_str(content_type)
        .and_then(|exts| exts.first().copied())
        .unwrap_or("bin")
}

fn path_to_posix(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

fn ui_bundle_to_metadata(component: &SourceComponent) -> Result<Vec<WriteInfo>, ConvertError> {
    if let Some(content) = component.content() {
        validate_ui_bundle_for_deploy(content, component.tree(), component.force_ignore())?;
    }
    default_write_infos(component, TargetFormat::Metadata, None)
}

fn decomposed_labels_to_metadata(
    context: &mut ConvertContext,
    registry: &RegistryAccess,
    component: &SourceComponent,
) -> Result<Vec<WriteInfo>, ConvertError> {
    if component.metadata_type().name == "CustomLabels" {
        return default_write_infos(component, TargetFormat::Metadata, None);
    }
    let labels_type = registry.type_by_name("CustomLabels")?;
    let doc = component.parse_xml()?;
    context.decomposed_labels.add_label(
        &labels_type.directory_name,
        component.full_name(),
        doc.root.without_xmlns(),
    );
    Ok(Vec::new())
}

fn decomposed_labels_to_source(
    registry: &RegistryAccess,
    component: &SourceComponent,
    merge_with: Option<&SourceComponent>,
    merge_set: Option<&ComponentSet>,
) -> Result<Vec<WriteInfo>, ConvertError> {
    if component.metadata_type().name != "CustomLabels" {
        return default_write_infos(component, TargetFormat::Source, merge_with);
    }
    let label_type = registry.type_by_name("CustomLabel")?;
    let doc = component.parse_xml()?;
    let mut infos = Vec::new();
    for el in doc.root.child_elements().filter(|e| e.name == "labels") {
        let Some(full_name) = el
            .child_element("fullName")
            .and_then(XmlElement::text_content)
            .map(str::trim)
            .filter(|s| !s.is_empty())
        else {
            continue;
        };

        let existing = merge_set.and_then(|ms| {
            ms.filenames_by_name_and_type(&MetadataMember {
                full_name: full_name.to_owned(),
                type_name: label_type.name.clone(),
            })
            .into_iter()
            .next()
        });
        let output = match existing {
            Some(path) => path,
            None => calculate_relative_path(
                TargetFormat::Source,
                label_type,
                None,
                full_name,
                Path::new(&format!("{full_name}.label-meta.xml")),
            )?,
        };

        let mut root = el.without_xmlns();
        "CustomLabel".clone_into(&mut root.name);
        let bytes = XmlDocument {
            declaration: true,
            root,
        }
        .to_xml_string()
        .into_bytes();
        infos.push(WriteInfo {
            output,
            action: WriteAction::Write(WriteSource::Bytes(bytes)),
        });
    }
    Ok(infos)
}

fn decomposed_ps_to_metadata(
    context: &mut ConvertContext,
    registry: &RegistryAccess,
    component: &SourceComponent,
) -> Result<Vec<WriteInfo>, ConvertError> {
    let ps_type = registry.type_by_name("PermissionSet")?;
    let children = component.children()?;
    let parent = if component
        .xml()
        .is_some_and(|p| p.to_string_lossy().ends_with(".permissionset-meta.xml"))
    {
        component.clone()
    } else if let Some(first_child) = children.first() {
        let replacement_name = format!("{}.permissionset-meta.xml", first_child.name());
        let synthesized_xml = first_child.xml().map(|p| {
            p.parent().map_or_else(
                || PathBuf::from(&replacement_name),
                |d| d.join(&replacement_name),
            )
        });
        SourceComponent::new(
            ComponentProps {
                name: first_child.name().to_owned(),
                metadata_type: ps_type.clone(),
                xml: synthesized_xml,
                content: None,
                parent: None,
                parent_type: None,
            },
            component.tree().clone(),
            component.force_ignore().clone(),
        )
    } else {
        component.clone()
    };

    let parent_name = parent.full_name();
    for item in children.into_iter().chain(std::iter::once(parent)) {
        let Some(xml_path) = item.xml() else {
            continue;
        };
        if !item.tree().exists(xml_path) {
            continue;
        }
        let doc = parse_raw_component_xml(&item, xml_path)?;
        context.decomposed_permission_set.add_fragment(
            &ps_type.directory_name,
            parent_name.clone(),
            doc.root.without_xmlns(),
        );
    }

    Ok(Vec::new())
}

fn parse_raw_component_xml(
    component: &SourceComponent,
    xml_path: &Path,
) -> Result<XmlDocument, ConvertError> {
    let mut raw = component.tree().read_to_string(xml_path)?;
    let rules = component.replacements.get(xml_path).or_else(|| {
        component
            .parent()
            .and_then(|p| p.replacements.get(xml_path))
    });
    if let Some(active) = rules {
        raw = apply_replacements(&raw, active);
    }
    Ok(parse_xml(&raw)?)
}

struct PsChildBucket<'a> {
    child_type: &'a MetadataType,
    entry_name: String,
    tags: IndexMap<String, Vec<XmlElement>>,
}

fn decomposed_ps_to_source(
    default_directory: Option<&Path>,
    component: &SourceComponent,
    merge_with: Option<&SourceComponent>,
    parent_check: ParentDiskCheck,
) -> Result<Vec<WriteInfo>, ConvertError> {
    let force_ignore = component.force_ignore();
    let parent_output = get_ps_parent_output(component, merge_with)?;
    if force_ignore.denies(&parent_output) {
        return Ok(Vec::new());
    }

    let parent_type = component.metadata_type();
    let parent_doc = component.parse_xml()?;
    let mut parent_root = XmlElement {
        name: parent_type.name.clone(),
        attributes: parent_doc.root.attributes.clone(),
        children: Vec::new(),
    };

    let mut top_level_buckets: IndexMap<String, PsChildBucket<'_>> = IndexMap::new();
    let mut dir_buckets: IndexMap<String, PsChildBucket<'_>> = IndexMap::new();

    for node in &parent_doc.root.children {
        let XmlNode::Element(el) = node else {
            parent_root.children.push(node.clone());
            continue;
        };
        let Some(child_type) = resolve_child_type(parent_type, &el.name)? else {
            parent_root.children.push(node.clone());
            continue;
        };

        let clean_el = el.without_xmlns();
        if child_type.directory_name.is_empty() {
            let bucket = top_level_buckets
                .entry(child_type.id.clone())
                .or_insert_with(|| PsChildBucket {
                    child_type,
                    entry_name: component.name().to_owned(),
                    tags: IndexMap::new(),
                });
            bucket
                .tags
                .entry(clean_el.name.clone())
                .or_default()
                .push(clean_el);
        } else {
            let entry_name = extract_ps_object_entry_name(component, el, child_type)?;
            let bucket = dir_buckets
                .entry(entry_name.clone())
                .or_insert_with(|| PsChildBucket {
                    child_type,
                    entry_name,
                    tags: IndexMap::new(),
                });
            bucket
                .tags
                .entry(clean_el.name.clone())
                .or_default()
                .push(clean_el);
        }
    }

    let mut infos = Vec::new();
    for bucket in top_level_buckets
        .into_values()
        .chain(dir_buckets.into_values())
    {
        if let Some(info) = build_ps_child_write_info(component, merge_with, bucket)? {
            infos.push(info);
        }
    }

    let parent_has_real_values = parent_root.has_real_values();
    let parent_bytes = XmlDocument::new_metadata(parent_root)
        .to_xml_string()
        .into_bytes();
    let emit_parent = merge_with.is_some()
        || parent_has_real_values
        || parent_check == ParentDiskCheck::AlwaysEmit
        || !default_directory.is_some_and(|dir| dir.join(&parent_output).exists());
    if emit_parent {
        infos.push(WriteInfo {
            output: parent_output,
            action: WriteAction::Write(WriteSource::Bytes(parent_bytes)),
        });
    }

    Ok(infos)
}

fn extract_ps_object_entry_name(
    parent: &SourceComponent,
    el: &XmlElement,
    child_type: &MetadataType,
) -> Result<String, ConvertError> {
    let raw_id = el
        .extract_unique_element(child_type.unique_id_element.as_deref())
        .ok_or_else(|| ResolveError::MissingUniqueElement {
            element: child_type
                .unique_id_element
                .clone()
                .unwrap_or_else(|| "fullName".to_owned()),
            child_type: child_type.name.clone(),
            xml_path: parent.xml().map_or_else(PathBuf::new, Path::to_path_buf),
        })?;
    let base = raw_id.split('.').next().unwrap_or(raw_id);
    Ok(base.strip_prefix("standard-").unwrap_or(base).to_owned())
}

fn build_ps_child_write_info(
    parent: &SourceComponent,
    merge_with: Option<&SourceComponent>,
    bucket: PsChildBucket<'_>,
) -> Result<Option<WriteInfo>, ConvertError> {
    let default_output =
        get_ps_child_default_output(parent, bucket.child_type, &bucket.entry_name)?;
    if parent.force_ignore().denies(&default_output) {
        return Ok(None);
    }

    let suffix = bucket.child_type.suffix.as_deref().unwrap_or("");
    let filename = format!("{}.{suffix}{META_XML_SUFFIX}", bucket.entry_name);
    let output = if let Some(mw) = merge_with
        && let Some(mw_content) = mw.content()
    {
        if bucket.child_type.directory_name.is_empty() {
            mw_content.join(filename)
        } else {
            mw_content
                .join(&bucket.child_type.directory_name)
                .join(filename)
        }
    } else {
        default_output
    };

    let mut root = XmlElement::new(&parent.metadata_type().name);
    for (_, elems) in bucket.tags {
        for elem in elems {
            root.children.push(XmlNode::Element(elem));
        }
    }
    let bytes = XmlDocument {
        declaration: true,
        root,
    }
    .to_xml_string()
    .into_bytes();

    Ok(Some(WriteInfo {
        output,
        action: WriteAction::Write(WriteSource::Bytes(bytes)),
    }))
}

fn get_ps_child_default_output(
    parent: &SourceComponent,
    child_type: &MetadataType,
    entry_name: &str,
) -> Result<PathBuf, ConvertError> {
    let base_dir = calculate_relative_path(
        TargetFormat::Source,
        parent.metadata_type(),
        parent.parent_type(),
        &parent.full_name(),
        Path::new(parent.name()),
    )?;
    let suffix = child_type.suffix.as_deref().unwrap_or("");
    let file_name = format!("{entry_name}.{suffix}{META_XML_SUFFIX}");
    if child_type.directory_name.is_empty() {
        Ok(base_dir.join(file_name))
    } else {
        Ok(base_dir.join(&child_type.directory_name).join(file_name))
    }
}

fn get_ps_parent_output(
    component: &SourceComponent,
    merge_with: Option<&SourceComponent>,
) -> Result<PathBuf, ConvertError> {
    if let Some(mw) = merge_with
        && let Some(mw_content) = mw.content()
    {
        let sfx = component.metadata_type().suffix.as_deref().unwrap_or("");
        return Ok(mw_content.join(format!("{}.{sfx}{META_XML_SUFFIX}", component.name())));
    }
    get_decomposed_output_file(component, merge_with)
}

fn decomposed_esr_to_source(
    registry: &RegistryAccess,
    component: &SourceComponent,
) -> Result<Vec<WriteInfo>, ConvertError> {
    let esr_type = registry.type_by_name("ExternalServiceRegistration")?;
    let output_dir = PathBuf::from("main")
        .join("default")
        .join(&esr_type.directory_name);
    let mut doc = component.parse_xml()?;

    let schema_content = doc
        .root
        .child_element("schema")
        .and_then(XmlElement::text_content)
        .unwrap_or_default()
        .to_owned();
    doc.root
        .children
        .retain(|node| !matches!(node, XmlNode::Element(el) if el.name == "schema"));

    let existing_ext = doc
        .root
        .child_element("schemaUploadFileExtension")
        .and_then(XmlElement::text_content)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let schema_type = existing_ext.unwrap_or_else(|| {
        if schema_content.trim_start().starts_with('{') {
            "json".to_owned()
        } else {
            "yaml".to_owned()
        }
    });

    if let Some(ext_el) = doc.root.children.iter_mut().find_map(|n| match n {
        XmlNode::Element(el) if el.name == "schemaUploadFileExtension" => Some(el),
        _ => None,
    }) {
        ext_el.children = vec![XmlNode::Text(schema_type.clone())];
    } else {
        let mut ext_el = XmlElement::new("schemaUploadFileExtension");
        ext_el.children.push(XmlNode::Text(schema_type.clone()));
        doc.root.children.push(XmlNode::Element(ext_el));
    }

    let xml_path = component.xml().map_or_else(PathBuf::new, Path::to_path_buf);
    let yaml_content = if schema_type == "yaml" {
        schema_content
    } else {
        let json_val: serde_json::Value =
            serde_json::from_str(&schema_content).map_err(|err| ConvertError::InvalidSchema {
                name: component.full_name(),
                path: xml_path.clone(),
                reason: err.to_string(),
            })?;
        serde_yaml_ng::to_string(&json_val).map_err(|err| ConvertError::InvalidSchema {
            name: component.full_name(),
            path: xml_path,
            reason: err.to_string(),
        })?
    };

    let full_name = component.full_name();
    let schema_out = output_dir.join(format!("{full_name}.yaml"));
    let meta_out = output_dir.join(format!(
        "{full_name}.externalServiceRegistration{META_XML_SUFFIX}"
    ));
    let meta_bytes = XmlDocument::new_metadata(doc.root)
        .to_xml_string()
        .into_bytes();

    Ok(vec![
        WriteInfo {
            output: schema_out,
            action: WriteAction::Write(WriteSource::Bytes(yaml_content.into_bytes())),
        },
        WriteInfo {
            output: meta_out,
            action: WriteAction::Write(WriteSource::Bytes(meta_bytes)),
        },
    ])
}

fn decomposed_esr_to_metadata(
    context: &mut ConvertContext,
    registry: &RegistryAccess,
    component: &SourceComponent,
) -> Result<Vec<WriteInfo>, ConvertError> {
    let esr_type = registry.type_by_name("ExternalServiceRegistration")?;
    let mut doc = component.parse_xml()?;

    let full_name = component.full_name();
    let schema_file = component
        .xml()
        .and_then(Path::parent)
        .unwrap_or_else(|| Path::new(""))
        .join(format!("{full_name}.yaml"));
    let schema_raw = component.tree().read_to_string(&schema_file)?;

    let is_json = doc
        .root
        .child_element("schemaUploadFileExtension")
        .and_then(XmlElement::text_content)
        .map(str::trim)
        == Some("json");

    let schema_value = if is_json {
        let yaml_val: serde_json::Value =
            serde_yaml_ng::from_str(&schema_raw).map_err(|err| ConvertError::InvalidSchema {
                name: full_name.clone(),
                path: schema_file.clone(),
                reason: err.to_string(),
            })?;
        serde_json::to_string_pretty(&yaml_val).map_err(|err| ConvertError::InvalidSchema {
            name: full_name.clone(),
            path: schema_file,
            reason: err.to_string(),
        })?
    } else {
        schema_raw
    };

    let mut schema_el = XmlElement::new("schema");
    schema_el.children.push(XmlNode::Text(schema_value));
    doc.root.children.push(XmlNode::Element(schema_el));

    context
        .decomposed_esr
        .add_record(&esr_type.directory_name, full_name, doc.root);
    Ok(Vec::new())
}
