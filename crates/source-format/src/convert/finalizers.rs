//! Conversion transaction context and finalizers matching SDR's `src/convert/convertContext/*`.

use std::cell::RefCell;
use std::cmp::Ordering;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use crate::registry::{
    RecompositionStrategy, RegistryAccess, SfdxProjectConfig, TransformerStrategy,
};
use crate::tree::{ForceIgnore, TreeContainer, normalize_lexical};
use crate::xml::{XmlDocument, XmlElement, XmlNode};
use feruca::{Collator, Tailoring};
use indexmap::IndexMap;
use rayon::prelude::*;

use crate::convert::error::ConvertError;
use crate::convert::transformers::get_xml_destination;
use crate::convert::types::{
    ResolveScope, TargetFormat, WriteAction, WriteInfo, WriteSource, WriterFormat,
};
use crate::io_pool::io_install;
use crate::path_util::calculate_relative_path;
use crate::resolve::{ComponentProps, ComponentSet, MetadataMember, SourceComponent, TreeHandle};

const SINGLETON_PROFILE_TAGS: &[&str] = &["custom", "description", "loginHours", "userLicense"];

/// Identity field within each repeatable `<Profile>` section tag.
const PROFILE_IDENTITY_FIELDS: &[(&str, &str)] = &[
    ("applicationVisibilities", "application"),
    ("categoryGroupVisibilities", "dataCategoryGroup"),
    ("classAccesses", "apexClass"),
    ("customMetadataTypeAccesses", "name"),
    ("customPermissions", "name"),
    ("customSettingAccesses", "name"),
    ("externalDataSourceAccesses", "externalDataSource"),
    ("fieldPermissions", "field"),
    ("flowAccesses", "flow"),
    ("objectPermissions", "object"),
    ("pageAccesses", "apexPage"),
    ("recordTypeVisibilities", "recordType"),
    ("tabVisibilities", "tab"),
    ("userPermissions", "name"),
];

/// Controls whether profile fragment merging rejects conflicting singleton values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ValidateSingletons {
    Yes,
    #[expect(dead_code, reason = "Completes the boolean replacement enum")]
    No,
}

/// Shared conversion state accumulated across all components in a single conversion run.
pub(crate) struct ConvertContext {
    pub decomposition: DecompositionFinalizer,
    pub recomposition: RecompositionFinalizer,
    pub non_decomposition: NonDecompositionFinalizer,
    pub profile_metadata: ProfileMetadataFinalizer,
    pub decomposed_labels: DecomposedLabelsFinalizer,
    pub decomposed_permission_set: DecomposedPermissionSetFinalizer,
    pub decomposed_esr: DecomposedExternalServiceRegistrationFinalizer,
}

impl ConvertContext {
    pub(crate) fn new() -> Self {
        Self {
            decomposition: DecompositionFinalizer::default(),
            recomposition: RecompositionFinalizer::default(),
            non_decomposition: NonDecompositionFinalizer::default(),
            profile_metadata: ProfileMetadataFinalizer::default(),
            decomposed_labels: DecomposedLabelsFinalizer::default(),
            decomposed_permission_set: DecomposedPermissionSetFinalizer::default(),
            decomposed_esr: DecomposedExternalServiceRegistrationFinalizer::default(),
        }
    }

    pub(crate) fn execute_finalizers(
        &mut self,
        default_directory: Option<&Path>,
        registry: &RegistryAccess,
        tree: &TreeHandle,
    ) -> Result<Vec<WriterFormat>, ConvertError> {
        let mut out = Vec::new();
        out.extend(self.decomposition.finalize());
        out.extend(self.recomposition.finalize()?);
        if let Some(dir) = default_directory {
            out.extend(self.non_decomposition.finalize(dir, registry, tree)?);
        }
        out.extend(self.profile_metadata.finalize(default_directory)?);
        out.extend(self.decomposed_labels.finalize());
        out.extend(self.decomposed_permission_set.finalize());
        out.extend(self.decomposed_esr.finalize());
        Ok(out)
    }
}

/// Entry tracking an unclaimed decomposed child or parent pending merge resolution.
#[derive(Debug, Default)]
pub(crate) struct DecompositionStateValue {
    pub found_merge: bool,
    pub write_info: Option<WriteInfo>,
}

/// Flushes deferred decomposed children whose parents/siblings were not found during merge.
#[derive(Debug, Default)]
pub(crate) struct DecompositionFinalizer {
    pub state: IndexMap<String, DecompositionStateValue>,
}

impl DecompositionFinalizer {
    pub(crate) fn mark_merged(&mut self, comp_key: String) {
        let entry = self.state.entry(comp_key).or_default();
        entry.found_merge = true;
    }

    pub(crate) fn defer_write(&mut self, comp_key: String, write_info: WriteInfo) {
        let entry = self.state.entry(comp_key).or_default();
        entry.write_info = Some(write_info);
    }

    pub(crate) fn finalize(&mut self) -> Vec<WriterFormat> {
        let mut out = Vec::new();
        for value in self.state.values() {
            if !value.found_merge
                && let Some(write_info) = &value.write_info
            {
                out.push(WriterFormat {
                    write_infos: vec![write_info.clone()],
                    resolve_scope: ResolveScope::AllFiles,
                });
            }
        }
        out
    }
}

#[derive(Debug, Default)]
pub(crate) struct RecompositionStateValue {
    pub component: Option<SourceComponent>,
    pub children: Vec<SourceComponent>,
}

/// Rolls up decomposed and non-decomposed children into a single MDAPI parent XML file.
#[derive(Debug, Default)]
pub(crate) struct RecompositionFinalizer {
    pub state: IndexMap<String, RecompositionStateValue>,
}

impl RecompositionFinalizer {
    pub(crate) fn add_component(&mut self, component: SourceComponent) -> Result<(), ConvertError> {
        if component.parent().is_some() {
            self.add_resolved_component(component, Vec::new());
        } else {
            let children = component.children()?;
            self.add_resolved_component(component, children);
        }
        Ok(())
    }

    pub(crate) fn add_resolved_component(
        &mut self,
        component: SourceComponent,
        children: Vec<SourceComponent>,
    ) {
        if let Some(parent) = component.parent().cloned() {
            let key = simple_key(&parent.metadata_type().id, &parent.full_name());
            let entry = self.state.entry(key).or_default();
            entry.component.get_or_insert(parent);
            entry.children.push(component);
        } else {
            let key = simple_key(&component.metadata_type().id, &component.full_name());
            let entry = self.state.entry(key).or_default();
            let should_update_parent = (component.xml().is_some()
                && entry.component.as_ref().is_some_and(|e| e.xml().is_none()))
                || entry.component.is_none();
            if should_update_parent {
                entry.component = Some(component);
            }
            entry.children.extend(children);
        }
    }

    pub(crate) fn finalize(&mut self) -> Result<Vec<WriterFormat>, ConvertError> {
        let state_entries: Vec<RecompositionStateValue> =
            self.state.drain(..).map(|(_, v)| v).collect();
        if state_entries.len() >= 4 {
            io_install(|| {
                state_entries
                    .into_par_iter()
                    .map(finalize_recomposition_entry)
                    .collect()
            })
        } else {
            state_entries
                .into_iter()
                .map(finalize_recomposition_entry)
                .collect()
        }
    }
}

fn finalize_recomposition_entry(
    entry: RecompositionStateValue,
) -> Result<WriterFormat, ConvertError> {
    let Some(parent_comp) = entry.component else {
        let child_names: Vec<String> = entry
            .children
            .iter()
            .map(SourceComponent::full_name)
            .collect();
        return Err(ConvertError::MissingRecompositionParent {
            children: if child_names.is_empty() {
                "<none>".to_owned()
            } else {
                child_names.join(", ")
            },
        });
    };

    let doc = recompose_parent(&parent_comp, &entry.children)?;
    let xml_bytes = doc.to_xml_string().into_bytes();
    let suffix = parent_comp.metadata_type().suffix.as_deref().unwrap_or("");
    let output = PathBuf::from(&parent_comp.metadata_type().directory_name)
        .join(format!("{}.{suffix}", parent_comp.full_name()));

    Ok(WriterFormat {
        write_infos: vec![WriteInfo {
            output,
            action: WriteAction::Write(WriteSource::Bytes(xml_bytes)),
        }],
        resolve_scope: ResolveScope::AllFiles,
    })
}

fn recompose_parent(
    parent_comp: &SourceComponent,
    raw_children: &[SourceComponent],
) -> Result<XmlDocument, ConvertError> {
    let deduped = deduplicate_children(raw_children);
    let parent_type = parent_comp.metadata_type();

    let root_policy = if parent_type
        .strategies
        .as_ref()
        .and_then(|s| s.recomposition)
        == Some(RecompositionStrategy::StartEmpty)
    {
        ParentRootPolicy::StartEmpty
    } else {
        ParentRootPolicy::PreserveExisting
    };
    let is_non_decomposed = parent_type.strategies.as_ref().and_then(|s| s.transformer)
        == Some(TransformerStrategy::NonDecomposed);

    let (mut root, child_items) = if is_non_decomposed {
        read_non_decomposed_group(parent_comp, &deduped, root_policy)?
    } else {
        let root_el = match root_policy {
            ParentRootPolicy::StartEmpty => XmlElement::new(&parent_type.name),
            ParentRootPolicy::PreserveExisting => match parent_comp.xml() {
                Some(pxml) => parent_comp.parse_xml_at(pxml)?.root.without_xmlns(),
                None => XmlElement::new(&parent_type.name),
            },
        };
        let items = read_decomposed_children(&deduped)?;
        (root_el, items)
    };

    // Group children alphabetically by XML element tag name (matching JS Object.keys().sort()).
    let mut grouped: BTreeMap<String, Vec<RecomposedChild>> = BTreeMap::new();
    for item in child_items {
        grouped.entry(item.group_name).or_default().push(item.child);
    }

    for (group_name, mut items) in grouped {
        items.sort_by(|a, b| collate_str(&a.sort_key, &b.sort_key));

        for item in items {
            let mut child_elem = item.elem.without_xmlns();
            child_elem.name.clone_from(&group_name);
            root.children.push(XmlNode::Element(child_elem));
        }
    }

    Ok(XmlDocument::new_metadata(root))
}

fn read_decomposed_children(deduped: &[SourceComponent]) -> Result<Vec<TaggedChild>, ConvertError> {
    if deduped.len() >= 16 {
        io_install(|| deduped.par_iter().map(read_decomposed_child).collect())
    } else {
        deduped.iter().map(read_decomposed_child).collect()
    }
}

fn read_decomposed_child(child: &SourceComponent) -> Result<TaggedChild, ConvertError> {
    let child_type = child.metadata_type();
    let group_name = child_type.effective_xml_element().to_owned();
    let elem = match child.xml() {
        Some(xml_path) => child.parse_xml_at(xml_path)?.root,
        None => XmlElement::new(&child_type.name),
    };
    let sort_key = elem
        .extract_unique_element(child_type.unique_id_element.as_deref())
        .unwrap_or("")
        .to_owned();
    Ok(TaggedChild {
        group_name,
        child: RecomposedChild { elem, sort_key },
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParentRootPolicy {
    StartEmpty,
    PreserveExisting,
}

fn read_non_decomposed_group(
    parent_comp: &SourceComponent,
    deduped: &[SourceComponent],
    root_policy: ParentRootPolicy,
) -> Result<(XmlElement, Vec<TaggedChild>), ConvertError> {
    let (root_el, index) = build_non_decomposed_index(parent_comp, deduped, root_policy)?;
    let mut items = Vec::with_capacity(deduped.len());
    for child in deduped {
        let child_type = child.metadata_type();
        let group_name = child_type.effective_xml_element().to_owned();
        let elem = match child.xml() {
            Some(xml_path) if child.parent().is_some() => {
                let key = (
                    xml_path.to_path_buf(),
                    group_name.clone(),
                    child.name().to_owned(),
                );
                index.get(&key).cloned().ok_or_else(|| {
                    ConvertError::Resolve(crate::resolve::ResolveError::MissingUniqueElement {
                        element: child_type
                            .unique_id_element
                            .clone()
                            .unwrap_or_else(|| "fullName".to_owned()),
                        child_type: child_type.name.clone(),
                        xml_path: xml_path.to_path_buf(),
                    })
                })?
            }
            Some(xml_path) => child.parse_xml_at(xml_path)?.root,
            None => XmlElement::new(&child_type.name),
        };
        let sort_key = elem
            .extract_unique_element(child_type.unique_id_element.as_deref())
            .unwrap_or("")
            .to_owned();
        items.push(TaggedChild {
            group_name,
            child: RecomposedChild { elem, sort_key },
        });
    }

    Ok((root_el, items))
}

type NonDecomposedIndex = HashMap<(PathBuf, String, String), XmlElement>;

fn build_non_decomposed_index(
    parent_comp: &SourceComponent,
    deduped: &[SourceComponent],
    root_policy: ParentRootPolicy,
) -> Result<(XmlElement, NonDecomposedIndex), ConvertError> {
    let parent_type = parent_comp.metadata_type();
    let mut unique_parents: HashMap<PathBuf, SourceComponent> = HashMap::new();
    for child in deduped {
        if let Some(p) = child.parent()
            && let Some(pxml) = p.xml()
        {
            unique_parents
                .entry(pxml.to_path_buf())
                .or_insert_with(|| p.clone());
        }
    }
    if root_policy == ParentRootPolicy::PreserveExisting
        && let Some(pxml) = parent_comp.xml()
    {
        unique_parents
            .entry(pxml.to_path_buf())
            .or_insert_with(|| parent_comp.clone());
    }

    // Build expected `(tag_name, unique_id_element)` specs so each parsed parent document is
    // indexed in a single `O(N)` pass instead of cloned and scanned per child (`O(N^2)`).
    let mut tag_specs: HashMap<&str, Option<&str>> = HashMap::new();
    if let Some(cfg) = &parent_type.children {
        for ct in cfg.types.values() {
            tag_specs
                .entry(ct.effective_xml_element())
                .or_insert(ct.unique_id_element.as_deref());
        }
    }
    for child in deduped {
        let ct = child.metadata_type();
        tag_specs
            .entry(ct.effective_xml_element())
            .or_insert(ct.unique_id_element.as_deref());
    }

    let parent_list: Vec<(PathBuf, SourceComponent)> = unique_parents.into_iter().collect();
    let parsed_parents: Vec<(PathBuf, XmlDocument)> = if parent_list.len() >= 2 {
        io_install(|| {
            parent_list
                .into_par_iter()
                .map(|(pxml, pcomp)| {
                    Ok::<_, ConvertError>((pxml.clone(), pcomp.parse_xml_at(&pxml)?))
                })
                .collect::<Result<Vec<_>, _>>()
        })?
    } else {
        parent_list
            .into_iter()
            .map(|(pxml, pcomp)| Ok::<_, ConvertError>((pxml.clone(), pcomp.parse_xml_at(&pxml)?)))
            .collect::<Result<Vec<_>, _>>()?
    };

    let mut root_el = XmlElement::new(&parent_type.name);
    let hint: usize = parsed_parents
        .iter()
        .map(|(_, d)| d.root.children.len())
        .sum();
    let mut index: HashMap<(PathBuf, String, String), XmlElement> = HashMap::with_capacity(hint);

    for (pxml, doc) in parsed_parents {
        if root_policy == ParentRootPolicy::PreserveExisting
            && parent_comp.xml() == Some(pxml.as_path())
        {
            root_el = doc.root.without_xmlns();
        }
        for node in doc.root.children {
            if let XmlNode::Element(el) = node
                && let Some(unique_id_opt) = tag_specs.get(el.name.as_str())
                && let Some(id_val) = el.extract_unique_element(*unique_id_opt)
            {
                index
                    .entry((pxml.clone(), el.name.clone(), id_val.to_owned()))
                    .or_insert(el);
            }
        }
    }

    Ok((root_el, index))
}

struct TaggedChild {
    group_name: String,
    child: RecomposedChild,
}

struct RecomposedChild {
    elem: XmlElement,
    sort_key: String,
}

thread_local! {
    static COLLATOR: RefCell<Collator> =
        RefCell::new(Collator::new(Tailoring::default(), false, true));
}

fn collate_str(a: &str, b: &str) -> Ordering {
    COLLATOR.with(|cell| cell.borrow_mut().collate(a, b))
}

fn deduplicate_children(children: &[SourceComponent]) -> Vec<SourceComponent> {
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for child in children {
        let key = format!("{}#{}", child.metadata_type().name, child.full_name());
        if seen.insert(key) {
            out.push(child.clone());
        }
    }
    out
}

/// Merges non-decomposed children (`CustomLabels`) across package directories into existing or
/// default source files.
#[derive(Debug, Default)]
pub(crate) struct NonDecompositionFinalizer {
    pub children_by_unique_element: IndexMap<String, XmlElement>,
    pub example_component: Option<SourceComponent>,
}

impl NonDecompositionFinalizer {
    pub(crate) fn add_child(
        &mut self,
        unique_name: String,
        element: XmlElement,
        example: &SourceComponent,
    ) {
        self.example_component
            .get_or_insert_with(|| example.clone());
        self.children_by_unique_element.insert(unique_name, element);
    }

    pub(crate) fn finalize(
        &mut self,
        default_dir: &Path,
        registry: &RegistryAccess,
        tree: &TreeHandle,
    ) -> Result<Vec<WriterFormat>, ConvertError> {
        if self.children_by_unique_element.is_empty() {
            return Ok(Vec::new());
        }
        let example = self
            .example_component
            .as_ref()
            .ok_or(ConvertError::MissingRecompositionParent {
                children: self
                    .children_by_unique_element
                    .keys()
                    .cloned()
                    .collect::<Vec<_>>()
                    .join(", "),
            })?
            .clone();

        let pkg_paths = discover_package_directories(default_dir)?;
        let default_norm = normalize_path_for_compare(default_dir);
        let default_is_package = pkg_paths
            .iter()
            .any(|p| normalize_path_for_compare(p) == default_norm);

        let all_non_decomposed = if default_is_package {
            find_all_components_of_type(&pkg_paths, &example.metadata_type().name, registry, tree)?
        } else {
            Vec::new()
        };

        let mut merge_map: IndexMap<PathBuf, IndexMap<String, XmlElement>> = IndexMap::new();
        let mut parent_component_map: IndexMap<PathBuf, SourceComponent> = IndexMap::new();
        let mut child_to_parent: HashMap<String, PathBuf> = HashMap::new();

        for comp in all_non_decomposed {
            let Some(xml_path) = comp.xml().map(Path::to_path_buf) else {
                continue;
            };
            let parent_doc = comp.parse_xml_at(&xml_path)?;
            let xml_element = comp.metadata_type().effective_xml_element();
            let unique_id = comp
                .metadata_type()
                .children
                .as_ref()
                .and_then(|c| c.types.values().next())
                .and_then(|ct| ct.unique_id_element.as_deref());

            let mut child_map = IndexMap::new();
            for el in parent_doc
                .root
                .child_elements()
                .filter(|e| e.name == xml_element)
            {
                if let Some(child_name) = el.extract_unique_element(unique_id) {
                    child_to_parent.insert(child_name.to_owned(), xml_path.clone());
                    child_map.insert(child_name.to_owned(), el.clone());
                }
            }
            merge_map.insert(xml_path.clone(), child_map);
            parent_component_map.insert(xml_path, comp);
        }

        let default_key = default_dir.join(get_default_non_decomposed_output(&example)?);
        merge_map.entry(default_key.clone()).or_default();
        parent_component_map
            .entry(default_key.clone())
            .or_insert_with(|| example.clone());

        let mut files_to_write = HashSet::new();
        for (unique_name, child_elem) in &self.children_by_unique_element {
            let parent_key = child_to_parent
                .get(unique_name)
                .unwrap_or(&default_key)
                .clone();
            if let Some(bucket) = merge_map.get_mut(&parent_key) {
                bucket.insert(unique_name.clone(), child_elem.clone());
            }
            files_to_write.insert(parent_key);
        }

        let mut writer_data = Vec::new();
        for (parent_key, children) in merge_map {
            if !files_to_write.contains(&parent_key) {
                continue;
            }
            let parent_comp = &parent_component_map[&parent_key];
            let parent_type = parent_comp.metadata_type();
            let group_tag = parent_type.effective_xml_element();

            let mut root = XmlElement::new(&parent_type.name);
            for child in children.into_values() {
                let mut elem = child.without_xmlns();
                group_tag.clone_into(&mut elem.name);
                root.children.push(XmlNode::Element(elem));
            }
            let xml_bytes = XmlDocument::new_metadata(root).to_xml_string().into_bytes();
            writer_data.push(WriterFormat {
                write_infos: vec![WriteInfo {
                    output: parent_key,
                    action: WriteAction::Write(WriteSource::Bytes(xml_bytes)),
                }],
                resolve_scope: ResolveScope::AllFiles,
            });
        }

        Ok(writer_data)
    }
}

fn discover_package_directories(default_dir: &Path) -> Result<Vec<PathBuf>, ConvertError> {
    let Some((project_file, cfg)) = SfdxProjectConfig::find_and_load(default_dir)? else {
        return Ok(Vec::new());
    };
    let project_root = project_file.parent().unwrap_or(default_dir);
    Ok(cfg
        .package_directories
        .iter()
        .map(|d| normalize_lexical(&project_root.join(&d.path)))
        .collect())
}

fn normalize_path_for_compare(path: &Path) -> PathBuf {
    path.canonicalize()
        .unwrap_or_else(|_| std::path::absolute(path).unwrap_or_else(|_| normalize_lexical(path)))
}

fn find_all_components_of_type(
    pkg_dirs: &[PathBuf],
    type_name: &str,
    registry: &RegistryAccess,
    tree: &TreeHandle,
) -> Result<Vec<SourceComponent>, ConvertError> {
    let mut include = ComponentSet::new(registry.clone());
    include.add_member(&MetadataMember {
        type_name: type_name.to_owned(),
        full_name: "*".to_owned(),
    })?;
    let existing_paths: Vec<PathBuf> = pkg_dirs
        .iter()
        .filter(|p| tree.exists(p))
        .cloned()
        .collect();
    if existing_paths.is_empty() {
        return Ok(Vec::new());
    }
    let cs = ComponentSet::from_source_with_tree(
        &existing_paths,
        tree,
        registry.clone(),
        Some(&include),
    )?;
    Ok(cs.source_components())
}

fn get_default_non_decomposed_output(component: &SourceComponent) -> Result<PathBuf, ConvertError> {
    let full_name = component.full_name();
    let base_name = full_name.split('.').next().unwrap_or(&full_name);
    let suffix = component.metadata_type().suffix.as_deref().unwrap_or("");
    let filename = format!("{base_name}.{suffix}-meta.xml");
    let rel_dir = calculate_relative_path(
        TargetFormat::Source,
        component.metadata_type(),
        component.parent_type(),
        &full_name,
        Path::new(base_name),
    )?;
    let dir = rel_dir.parent().unwrap_or(&rel_dir);
    Ok(dir.join(filename))
}

#[derive(Debug)]
struct ProfileFragment {
    component: SourceComponent,
    root: XmlElement,
}

#[derive(Debug)]
struct MetadataProfileBucket {
    fragments: Vec<ProfileFragment>,
}

#[derive(Debug)]
struct LocalProfileEntry {
    xml_path: PathBuf,
    root: XmlElement,
}

#[derive(Debug)]
struct SourceProfileBucket {
    component: SourceComponent,
    incoming: XmlElement,
    locals: IndexMap<PathBuf, LocalProfileEntry>,
}

#[derive(Debug)]
struct WorkingProfile {
    xml_path: PathBuf,
    root: XmlElement,
    touched: bool,
}

/// Merges `<Profile>` fragments across package directories in MDAPI output or across matching
/// local `<Profile>` files in source merge mode.
#[derive(Debug, Default)]
pub(crate) struct ProfileMetadataFinalizer {
    metadata_buckets: IndexMap<PathBuf, MetadataProfileBucket>,
    source_buckets: IndexMap<String, SourceProfileBucket>,
}

impl ProfileMetadataFinalizer {
    pub(crate) fn add(&mut self, component: SourceComponent) -> Result<(), ConvertError> {
        let output = get_xml_destination(TargetFormat::Metadata, &component, None)?;
        let root = component.parse_xml()?.root;
        let bucket = self
            .metadata_buckets
            .entry(output)
            .or_insert_with(|| MetadataProfileBucket {
                fragments: Vec::new(),
            });
        bucket.fragments.push(ProfileFragment { component, root });
        Ok(())
    }

    pub(crate) fn add_source(
        &mut self,
        component: &SourceComponent,
        merge_with: &SourceComponent,
    ) -> Result<(), ConvertError> {
        let key = component.full_name();
        if !self.source_buckets.contains_key(&key) {
            let incoming = component.parse_xml()?.root;
            self.source_buckets.insert(
                key.clone(),
                SourceProfileBucket {
                    component: component.clone(),
                    incoming,
                    locals: IndexMap::new(),
                },
            );
        }
        let bucket = &mut self.source_buckets[&key];
        if let Some(local_xml) = merge_with.xml() {
            let resolved_xml = absolutize_path(local_xml);
            if !bucket.locals.contains_key(&resolved_xml) {
                let root = merge_with.parse_xml()?.root;
                bucket.locals.insert(
                    resolved_xml.clone(),
                    LocalProfileEntry {
                        xml_path: resolved_xml,
                        root,
                    },
                );
            }
        }
        Ok(())
    }

    pub(crate) fn finalize(
        &mut self,
        default_directory: Option<&Path>,
    ) -> Result<Vec<WriterFormat>, ConvertError> {
        let mut results = Vec::new();
        let metadata_buckets: Vec<(PathBuf, MetadataProfileBucket)> =
            self.metadata_buckets.drain(..).collect();

        for (output, bucket) in metadata_buckets {
            let merged_root = merge_profiles(&bucket.fragments, ValidateSingletons::Yes)?;
            let bytes = XmlDocument::new_metadata(merged_root)
                .to_xml_string()
                .into_bytes();
            results.push(WriterFormat {
                write_infos: vec![WriteInfo {
                    output,
                    action: WriteAction::Write(WriteSource::Bytes(bytes)),
                }],
                resolve_scope: ResolveScope::AllFiles,
            });
        }

        let source_buckets: Vec<(String, SourceProfileBucket)> =
            self.source_buckets.drain(..).collect();
        for (_, bucket) in source_buckets {
            results.extend(finalize_source_profile(default_directory, &bucket)?);
        }

        Ok(results)
    }
}

fn finalize_source_profile(
    default_directory: Option<&Path>,
    bucket: &SourceProfileBucket,
) -> Result<Vec<WriterFormat>, ConvertError> {
    let default_rel = get_xml_destination(TargetFormat::Source, &bucket.component, None)?;
    let output = match default_directory {
        Some(dir) => absolutize_path(&dir.join(default_rel)),
        None => absolutize_path(&default_rel),
    };

    let (mut working, mut default_idx) = init_working_profiles(bucket, &output)?;
    for node in &bucket.incoming.children {
        if let XmlNode::Element(incoming_el) = node {
            route_incoming_profile_element(
                &mut working,
                &mut default_idx,
                &bucket.component,
                &output,
                incoming_el,
            );
        }
    }

    let mut out = Vec::new();
    for mut profile in working {
        if !profile.touched {
            continue;
        }
        copy_missing_root_attributes(&mut profile.root, &bucket.incoming);
        let bytes = XmlDocument::new_metadata(profile.root)
            .to_xml_string()
            .into_bytes();
        out.push(WriterFormat {
            write_infos: vec![WriteInfo {
                output: profile.xml_path,
                action: WriteAction::Write(WriteSource::Bytes(bytes)),
            }],
            resolve_scope: ResolveScope::AllFiles,
        });
    }
    Ok(out)
}

fn init_working_profiles(
    bucket: &SourceProfileBucket,
    output: &Path,
) -> Result<(Vec<WorkingProfile>, Option<usize>), ConvertError> {
    let mut working: Vec<WorkingProfile> = bucket
        .locals
        .values()
        .map(|entry| WorkingProfile {
            xml_path: entry.xml_path.clone(),
            root: entry.root.clone(),
            touched: false,
        })
        .collect();

    let mut default_idx: Option<usize> = None;
    if output.exists() {
        default_idx = working.iter().position(|w| w.xml_path == output);
        if default_idx.is_none() {
            let default_comp = SourceComponent::new(
                ComponentProps {
                    name: bucket.component.full_name(),
                    metadata_type: bucket.component.metadata_type().clone(),
                    xml: Some(output.to_path_buf()),
                    content: None,
                    parent: None,
                    parent_type: None,
                },
                TreeHandle::fs(),
                ForceIgnore::empty(),
            );
            let default_root = default_comp.parse_xml()?.root;
            default_idx = Some(working.len());
            working.push(WorkingProfile {
                xml_path: output.to_path_buf(),
                root: default_root,
                touched: false,
            });
        }
    }
    Ok((working, default_idx))
}

fn route_incoming_profile_element(
    working: &mut Vec<WorkingProfile>,
    default_idx: &mut Option<usize>,
    _incoming_comp: &SourceComponent,
    output: &Path,
    incoming_el: &XmlElement,
) {
    let tag = incoming_el.name.as_str();
    let owner_indices: Vec<usize> = working
        .iter()
        .enumerate()
        .filter(|(_, w)| {
            w.root
                .child_elements()
                .filter(|e| e.name == tag)
                .any(|local_el| entries_match(tag, local_el, incoming_el))
        })
        .map(|(i, _)| i)
        .collect();

    if owner_indices.is_empty() {
        let idx = if let Some(idx) = *default_idx {
            idx
        } else {
            working.push(WorkingProfile {
                xml_path: output.to_path_buf(),
                root: XmlElement::new("Profile"),
                touched: false,
            });
            let idx = working.len() - 1;
            *default_idx = Some(idx);
            idx
        };
        append_profile_entry(&mut working[idx], tag, incoming_el);
    } else {
        for idx in owner_indices {
            replace_profile_entry(&mut working[idx], tag, incoming_el);
        }
    }
}

fn replace_profile_entry(target: &mut WorkingProfile, tag: &str, incoming: &XmlElement) {
    let incoming_clean = incoming.without_xmlns();
    let mut changed = false;
    for node in &mut target.root.children {
        if let XmlNode::Element(local_el) = node
            && local_el.name == tag
            && entries_match(tag, local_el, incoming)
            && *local_el != incoming_clean
        {
            *local_el = incoming_clean.clone();
            changed = true;
        }
    }
    if changed {
        target.touched = true;
    }
}

fn append_profile_entry(target: &mut WorkingProfile, tag: &str, incoming: &XmlElement) {
    let clean = incoming.without_xmlns();
    if SINGLETON_PROFILE_TAGS.contains(&tag) {
        if let Some(existing_node) = target
            .root
            .children
            .iter_mut()
            .find(|n| matches!(n, XmlNode::Element(e) if e.name == tag))
        {
            *existing_node = XmlNode::Element(clean);
        } else {
            target.root.children.push(XmlNode::Element(clean));
        }
        target.touched = true;
        return;
    }

    let last_same_tag = target
        .root
        .children
        .iter()
        .rposition(|n| matches!(n, XmlNode::Element(e) if e.name == tag));
    match last_same_tag {
        Some(idx) => target
            .root
            .children
            .insert(idx + 1, XmlNode::Element(clean)),
        None => target.root.children.push(XmlNode::Element(clean)),
    }
    target.touched = true;
}

fn merge_profiles(
    fragments: &[ProfileFragment],
    validate_singletons: ValidateSingletons,
) -> Result<XmlElement, ConvertError> {
    let mut merged = XmlElement::new("Profile");
    let mut grouped: IndexMap<String, Vec<XmlElement>> = IndexMap::new();
    let mut owners: HashMap<String, &SourceComponent> = HashMap::new();

    for frag in fragments {
        copy_missing_root_attributes(&mut merged, &frag.root);
        for child_el in frag.root.child_elements() {
            let tag = &child_el.name;
            let clean = child_el.without_xmlns();
            let bucket = grouped.entry(tag.clone()).or_default();
            if bucket.contains(&clean) {
                continue;
            }
            if validate_singletons == ValidateSingletons::Yes
                && SINGLETON_PROFILE_TAGS.contains(&tag.as_str())
                && !bucket.is_empty()
            {
                let first_path = owners
                    .get(tag)
                    .and_then(|c| c.xml())
                    .map_or_else(PathBuf::new, Path::to_path_buf);
                let second_path = frag
                    .component
                    .xml()
                    .map_or_else(PathBuf::new, Path::to_path_buf);
                return Err(ConvertError::ProfileSingletonConflict {
                    profile_name: frag.component.full_name(),
                    tag: tag.clone(),
                    first_path,
                    second_path,
                });
            }
            bucket.push(clean);
            owners.entry(tag.clone()).or_insert(&frag.component);
        }
    }

    for (_, elems) in grouped {
        for elem in elems {
            merged.children.push(XmlNode::Element(elem));
        }
    }
    Ok(merged)
}

fn copy_missing_root_attributes(target: &mut XmlElement, source: &XmlElement) {
    for (k, v) in &source.attributes {
        if !target.attributes.iter().any(|(existing, _)| existing == k) {
            target.attributes.push((k.clone(), v.clone()));
        }
    }
}

fn entry_identity(tag: &str, el: &XmlElement) -> Option<String> {
    if SINGLETON_PROFILE_TAGS.contains(&tag) {
        return Some(tag.to_owned());
    }
    if tag == "layoutAssignments" {
        if let Some(rt) = plain_text_field(el, "recordType") {
            return Some(format!("recordType:{rt}"));
        }
        if let Some(layout) = plain_text_field(el, "layout") {
            let prefix = layout.split('-').next().unwrap_or(layout);
            return Some(format!("default:{prefix}"));
        }
        return None;
    }
    if tag == "loginIpRanges" {
        let start = plain_text_field(el, "startAddress")?;
        let end = plain_text_field(el, "endAddress")?;
        return Some(format!("{start}:{end}"));
    }
    if tag == "profileActionOverrides" {
        let fields = [
            "actionName",
            "formFactor",
            "pageOrSobjectType",
            "recordType",
        ];
        let mut parts = Vec::with_capacity(fields.len());
        for field in fields {
            match el.child_element(field) {
                None => parts.push(""),
                Some(child) => parts.push(plain_element_text(child)?),
            }
        }
        return Some(parts.join(":"));
    }
    for (section_tag, id_field) in PROFILE_IDENTITY_FIELDS {
        if *section_tag == tag {
            return plain_text_field(el, id_field).map(str::to_owned);
        }
    }
    None
}

fn plain_text_field<'a>(parent: &'a XmlElement, child_name: &str) -> Option<&'a str> {
    let child = parent.child_element(child_name)?;
    plain_element_text(child)
}

fn plain_element_text(el: &XmlElement) -> Option<&str> {
    if !el.attributes.is_empty() {
        return None;
    }
    match el.children.as_slice() {
        [] => Some(""),
        [XmlNode::Text(s) | XmlNode::CData(s)] => Some(s.as_str()),
        _ => None,
    }
}

fn entries_match(tag: &str, local: &XmlElement, incoming: &XmlElement) -> bool {
    if let (Some(a), Some(b)) = (entry_identity(tag, local), entry_identity(tag, incoming)) {
        a == b
    } else {
        local.without_xmlns() == incoming.without_xmlns()
    }
}

fn simple_key(type_id: &str, full_name: &str) -> String {
    format!("{type_id}#{full_name}")
}

fn absolutize_path(path: &Path) -> PathBuf {
    normalize_lexical(&std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf()))
}

/// Rolls up decomposed `CustomLabel` XML files (`decomposeCustomLabelsBeta2`) into
/// `labels/CustomLabels.labels`.
#[derive(Debug, Default)]
pub(crate) struct DecomposedLabelsFinalizer {
    custom_labels_dir: Option<String>,
    labels: IndexMap<String, XmlElement>,
}

impl DecomposedLabelsFinalizer {
    pub(crate) fn add_label(&mut self, labels_dir: &str, full_name: String, element: XmlElement) {
        if self.custom_labels_dir.is_none() {
            self.custom_labels_dir = Some(labels_dir.to_owned());
        }
        self.labels.insert(full_name, element);
    }

    pub(crate) fn finalize(&mut self) -> Vec<WriterFormat> {
        if self.labels.is_empty() {
            return Vec::new();
        }
        let Some(dir) = self.custom_labels_dir.take() else {
            return Vec::new();
        };
        let mut entries: Vec<(String, XmlElement)> = self.labels.drain(..).collect();
        entries.sort_by(|(a, _), (b, _)| collate_str(a, b));

        let mut root = XmlElement::new("CustomLabels");
        for (_, elem) in entries {
            let mut label_el = elem.without_xmlns();
            "labels".clone_into(&mut label_el.name);
            root.children.push(XmlNode::Element(label_el));
        }

        let bytes = XmlDocument::new_metadata(root).to_xml_string().into_bytes();
        let output = PathBuf::from(dir).join("CustomLabels.labels");
        vec![WriterFormat {
            write_infos: vec![WriteInfo {
                output,
                action: WriteAction::Write(WriteSource::Bytes(bytes)),
            }],
            resolve_scope: ResolveScope::AllFiles,
        }]
    }
}

/// Rolls up `decomposePermissionSetBeta2` fragments (`permissionset`, `classAccess`,
/// `objectSettings`, etc.) into `<name>.permissionset`.
#[derive(Debug, Default)]
pub(crate) struct DecomposedPermissionSetFinalizer {
    permission_sets_dir: Option<String>,
    fragments: IndexMap<String, Vec<XmlElement>>,
}

impl DecomposedPermissionSetFinalizer {
    pub(crate) fn add_fragment(
        &mut self,
        ps_dir: &str,
        parent_name: String,
        fragment_root: XmlElement,
    ) {
        if self.permission_sets_dir.is_none() {
            self.permission_sets_dir = Some(ps_dir.to_owned());
        }
        self.fragments
            .entry(parent_name)
            .or_default()
            .push(fragment_root);
    }

    pub(crate) fn finalize(&mut self) -> Vec<WriterFormat> {
        if self.fragments.is_empty() {
            return Vec::new();
        }
        let Some(dir) = self.permission_sets_dir.take() else {
            return Vec::new();
        };

        let mut out = Vec::new();
        for (parent_name, frags) in self.fragments.drain(..) {
            let mut grouped: IndexMap<String, Vec<XmlElement>> = IndexMap::new();
            for frag in frags {
                for el in frag.child_elements() {
                    grouped
                        .entry(el.name.clone())
                        .or_default()
                        .push(el.without_xmlns());
                }
            }
            let mut root = XmlElement::new("PermissionSet");
            for (_, elems) in grouped {
                for elem in elems {
                    root.children.push(XmlNode::Element(elem));
                }
            }
            let bytes = XmlDocument::new_metadata(root).to_xml_string().into_bytes();
            let output = PathBuf::from(&dir).join(format!("{parent_name}.permissionset"));
            out.push(WriterFormat {
                write_infos: vec![WriteInfo {
                    output,
                    action: WriteAction::Write(WriteSource::Bytes(bytes)),
                }],
                resolve_scope: ResolveScope::AllFiles,
            });
        }
        out
    }
}

/// Emits recomposed `ExternalServiceRegistration` MDAPI XML files from `.externalServiceRegistration-meta.xml`
/// + sibling `.yaml` files.
#[derive(Debug, Default)]
pub(crate) struct DecomposedExternalServiceRegistrationFinalizer {
    esr_dir: Option<String>,
    records: IndexMap<String, XmlElement>,
}

impl DecomposedExternalServiceRegistrationFinalizer {
    pub(crate) fn add_record(&mut self, esr_dir: &str, full_name: String, root: XmlElement) {
        if self.esr_dir.is_none() {
            self.esr_dir = Some(esr_dir.to_owned());
        }
        self.records.insert(full_name, root);
    }

    pub(crate) fn finalize(&mut self) -> Vec<WriterFormat> {
        if self.records.is_empty() {
            return Vec::new();
        }
        let Some(dir) = self.esr_dir.take() else {
            return Vec::new();
        };

        self.records
            .drain(..)
            .map(|(full_name, root)| {
                let bytes = XmlDocument::new_metadata(root.without_xmlns())
                    .to_xml_string()
                    .into_bytes();
                let output =
                    PathBuf::from(&dir).join(format!("{full_name}.externalServiceRegistration"));
                WriterFormat {
                    write_infos: vec![WriteInfo {
                        output,
                        action: WriteAction::Write(WriteSource::Bytes(bytes)),
                    }],
                    resolve_scope: ResolveScope::AllFiles,
                }
            })
            .collect()
    }
}
