//! Top-level conversion orchestrator (`MetadataConverter`) matching SDR's `metadataConverter.ts`.

use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::registry::{AdapterStrategy, RegistryAccess, TransformerStrategy};
use crate::tree::{ForceIgnore, normalize_lexical};
use rayon::prelude::*;

use crate::convert::error::ConvertError;
use crate::convert::finalizers::ConvertContext;
use crate::convert::replacements::mark_replacements;
use crate::convert::transformers::{
    ParentDiskCheck, effective_transformer_strategy, transform_to_metadata, transform_to_source,
    try_transform_to_metadata_parallel, try_transform_to_source_parallel,
};
use crate::convert::types::{
    ConvertCache, ConvertOutput, ConvertResult, ResolveScope, TargetFormat, UniqueDirPolicy,
    WriterFormat,
};
use crate::convert::writer::{ArchiveWriter, StandardWriter};
use crate::io_pool::io_install;
use crate::resolve::{
    ComponentSet, ForceIgnorePolicy, MetadataResolver, SourceComponent, TreeHandle,
};

const DEFAULT_PACKAGE_PREFIX: &str = "metadataPackage";
const PACKAGE_XML_FILE: &str = "package.xml";

/// Converts [`ComponentSet`]s between Salesforce Metadata API format and SFDX source format.
#[derive(Debug, Clone)]
pub struct MetadataConverter {
    registry: RegistryAccess,
}

impl Default for MetadataConverter {
    fn default() -> Self {
        Self::new(RegistryAccess::new())
    }
}

pub(crate) struct PreparedOutput {
    pub package_path: Option<PathBuf>,
    pub default_directory: Option<PathBuf>,
    pub merge_set: Option<ComponentSet>,
    pub project_dir: Option<PathBuf>,
    pub write_manifests: bool,
    pub is_zip: bool,
    pub parent_disk_check: ParentDiskCheck,
}

impl MetadataConverter {
    /// Creates a new [`MetadataConverter`] configured with `registry`.
    pub fn new(registry: RegistryAccess) -> Self {
        Self { registry }
    }

    /// Returns the [`RegistryAccess`] backing this converter.
    pub fn registry(&self) -> &RegistryAccess {
        &self.registry
    }

    /// Converts all addressable components in `components` to `target_format` and writes them
    /// according to `output`.
    ///
    /// # Errors
    ///
    /// Returns [`ConvertError`] if `Merge` is combined with `TargetFormat::Metadata`, any XML or
    /// archive file cannot be read or written, a replacement rule fails, a profile singleton
    /// conflicts across package directories, or resolution of written components fails.
    pub fn convert(
        &self,
        components: &ComponentSet,
        target_format: TargetFormat,
        output: ConvertOutput,
    ) -> Result<ConvertResult, ConvertError> {
        let cs = prepare_component_set(components, &output);
        let prepared = self.prepare_output(target_format, output)?;

        let mut source_comps: Vec<SourceComponent> = cs
            .source_components()
            .into_iter()
            .filter(SourceComponent::addressable)
            .collect();

        let apply_replacements = target_format == TargetFormat::Metadata
            && (prepared.is_zip
                || env::var("SF_APPLY_REPLACEMENTS_ON_CONVERT").as_deref() == Ok("true"));
        if apply_replacements {
            mark_replacements(&mut source_comps, cs.project_directory())?;
        }

        let writer_items = self.run_pipeline(&source_comps, target_format, &prepared)?;

        if prepared.is_zip {
            write_zip_result(&cs, target_format, prepared.package_path, &writer_items)
        } else {
            self.write_directory_result(&cs, target_format, prepared, &writer_items)
        }
    }

    /// Incrementally converts addressable components in `components` to `target_format`, reusing
    /// unchanged cached outputs verified with `blake3` hashes stored under `cache`.
    ///
    /// Falls back cleanly to a full rebuild whenever cached index metadata is missing or
    /// corrupted, the environment/registry/API configuration changed, or outputs are missing or
    /// modified on disk. [`ConvertOutput::Merge`] delegates directly to [`Self::convert`].
    ///
    /// # Errors
    ///
    /// Returns [`ConvertError`] if output preparation fails, any input/cache I/O or XML/archive
    /// processing fails, or re-resolution of converted files fails.
    pub fn convert_incremental(
        &self,
        components: &ComponentSet,
        target_format: TargetFormat,
        output: ConvertOutput,
        cache: &ConvertCache,
    ) -> Result<ConvertResult, ConvertError> {
        if matches!(output, ConvertOutput::Merge { .. }) {
            return self.convert(components, target_format, output);
        }
        crate::convert::cache::execute_incremental(self, components, target_format, &output, cache)
    }

    fn run_pipeline(
        &self,
        source_comps: &[SourceComponent],
        target_format: TargetFormat,
        prepared: &PreparedOutput,
    ) -> Result<Vec<WriterFormat>, ConvertError> {
        let mut context = ConvertContext::new();
        let active: Vec<&SourceComponent> = source_comps
            .iter()
            .filter(|c| !c.is_marked_for_delete())
            .collect();

        let mut writer_items = match target_format {
            TargetFormat::Source if prepared.merge_set.is_none() => {
                self.run_source_pipeline_parallel(&active, prepared, &mut context)?
            }
            TargetFormat::Source => {
                self.run_source_pipeline_seq(&active, prepared, &mut context)?
            }
            TargetFormat::Metadata => self.run_metadata_pipeline(&active, &mut context)?,
        };

        writer_items.extend(context.execute_finalizers(
            prepared.default_directory.as_deref(),
            &self.registry,
            &TreeHandle::fs(),
        )?);
        Ok(writer_items)
    }

    pub(crate) fn run_source_pipeline_parallel(
        &self,
        active: &[&SourceComponent],
        prepared: &PreparedOutput,
        context: &mut ConvertContext,
    ) -> Result<Vec<WriterFormat>, ConvertError> {
        self.run_source_pipeline_with_check(active, prepared, context, prepared.parent_disk_check)
    }

    pub(crate) fn run_source_pipeline_with_check(
        &self,
        active: &[&SourceComponent],
        prepared: &PreparedOutput,
        context: &mut ConvertContext,
        parent_check: ParentDiskCheck,
    ) -> Result<Vec<WriterFormat>, ConvertError> {
        let default_dir = prepared.default_directory.as_deref();
        let outputs = if active.len() >= 8 {
            io_install(|| {
                active
                    .par_iter()
                    .map(|comp| {
                        try_transform_to_source_parallel(
                            &self.registry,
                            default_dir,
                            comp,
                            parent_check,
                        )
                        .transpose()
                    })
                    .collect::<Result<Vec<_>, _>>()
            })?
        } else {
            active
                .iter()
                .map(|comp| {
                    try_transform_to_source_parallel(
                        &self.registry,
                        default_dir,
                        comp,
                        parent_check,
                    )
                    .transpose()
                })
                .collect::<Result<Vec<_>, _>>()?
        };

        let mut writer_items = Vec::new();
        for (comp, parallel_out) in active.iter().zip(outputs) {
            if let Some(out) = parallel_out {
                for (key, write_info) in out.deferred {
                    context.decomposition.defer_write(key, write_info);
                }
                if !out.infos.is_empty() {
                    writer_items.push(WriterFormat {
                        write_infos: out.infos,
                        resolve_scope: resolve_scope_for_component(comp),
                    });
                }
            } else {
                let infos = transform_component_to_source_with_check(
                    &self.registry,
                    context,
                    comp,
                    prepared,
                    parent_check,
                )?;
                if !infos.is_empty() {
                    writer_items.push(WriterFormat {
                        write_infos: infos,
                        resolve_scope: resolve_scope_for_component(comp),
                    });
                }
            }
        }
        Ok(writer_items)
    }

    fn run_source_pipeline_seq(
        &self,
        active: &[&SourceComponent],
        prepared: &PreparedOutput,
        context: &mut ConvertContext,
    ) -> Result<Vec<WriterFormat>, ConvertError> {
        let mut writer_items = Vec::new();
        for comp in active {
            let infos = transform_component_to_source(&self.registry, context, comp, prepared)?;
            if !infos.is_empty() {
                writer_items.push(WriterFormat {
                    write_infos: infos,
                    resolve_scope: resolve_scope_for_component(comp),
                });
            }
        }
        Ok(writer_items)
    }

    pub(crate) fn run_metadata_pipeline(
        &self,
        active: &[&SourceComponent],
        context: &mut ConvertContext,
    ) -> Result<Vec<WriterFormat>, ConvertError> {
        // Pre-resolve child files of decomposed/non-decomposed parents in parallel and compute
        // stateless MDAPI transforms concurrently before feeding `ConvertContext`.
        enum MetadataItem {
            Writes(Vec<crate::convert::types::WriteInfo>),
            Recompose(Vec<SourceComponent>),
            Sequential,
        }

        let process_one = |comp: &&SourceComponent| -> Result<MetadataItem, ConvertError> {
            if let Some(res) = try_transform_to_metadata_parallel(comp) {
                return Ok(MetadataItem::Writes(res?));
            }
            if matches!(
                effective_transformer_strategy(comp),
                TransformerStrategy::Decomposed | TransformerStrategy::NonDecomposed
            ) {
                let children = if comp.parent().is_some() {
                    Vec::new()
                } else {
                    comp.children()?
                };
                return Ok(MetadataItem::Recompose(children));
            }
            Ok(MetadataItem::Sequential)
        };

        let items = if active.len() >= 8 {
            io_install(|| {
                active
                    .par_iter()
                    .map(process_one)
                    .collect::<Result<Vec<_>, _>>()
            })?
        } else {
            active
                .iter()
                .map(process_one)
                .collect::<Result<Vec<_>, _>>()?
        };

        let mut writer_items = Vec::new();
        for (comp, item) in active.iter().zip(items) {
            match item {
                MetadataItem::Writes(infos) => {
                    if !infos.is_empty() {
                        writer_items.push(WriterFormat {
                            write_infos: infos,
                            resolve_scope: resolve_scope_for_component(comp),
                        });
                    }
                }
                MetadataItem::Recompose(children) => {
                    context
                        .recomposition
                        .add_resolved_component((*comp).clone(), children);
                }
                MetadataItem::Sequential => {
                    let infos = transform_to_metadata(context, &self.registry, comp)?;
                    if !infos.is_empty() {
                        writer_items.push(WriterFormat {
                            write_infos: infos,
                            resolve_scope: resolve_scope_for_component(comp),
                        });
                    }
                }
            }
        }
        Ok(writer_items)
    }

    pub(crate) fn prepare_output(
        &self,
        target_format: TargetFormat,
        output: ConvertOutput,
    ) -> Result<PreparedOutput, ConvertError> {
        match output {
            ConvertOutput::Directory {
                output_dir,
                package_name,
                unique_dir,
            } => {
                let pkg = resolve_package_path(&output_dir, package_name.as_deref(), unique_dir);
                fs::create_dir_all(&pkg).map_err(|source| ConvertError::Io {
                    path: pkg.clone(),
                    source,
                })?;
                Ok(PreparedOutput {
                    package_path: Some(pkg.clone()),
                    default_directory: Some(pkg),
                    merge_set: None,
                    project_dir: None,
                    write_manifests: true,
                    is_zip: false,
                    parent_disk_check: ParentDiskCheck::CheckDisk,
                })
            }
            ConvertOutput::Zip {
                output_dir,
                package_name,
                unique_dir,
            } => {
                let package_path = match output_dir {
                    Some(dir) => {
                        let base = resolve_package_path(&dir, package_name.as_deref(), unique_dir);
                        let mut zip_os = base.into_os_string();
                        zip_os.push(".zip");
                        let zip_path = PathBuf::from(zip_os);
                        if let Some(parent) = zip_path.parent() {
                            fs::create_dir_all(parent).map_err(|source| ConvertError::Io {
                                path: parent.to_path_buf(),
                                source,
                            })?;
                        }
                        Some(zip_path)
                    }
                    None => None,
                };
                Ok(PreparedOutput {
                    default_directory: package_path.clone(),
                    package_path,
                    merge_set: None,
                    project_dir: None,
                    write_manifests: true,
                    is_zip: true,
                    parent_disk_check: ParentDiskCheck::CheckDisk,
                })
            }
            ConvertOutput::Merge {
                merge_with,
                default_dir,
                project_dir,
            } => {
                if target_format != TargetFormat::Source {
                    return Err(ConvertError::MergeMetadataTargetUnsupported);
                }
                let merge_set = build_merge_set(&self.registry, merge_with);
                Ok(PreparedOutput {
                    package_path: None,
                    default_directory: Some(default_dir),
                    merge_set: Some(merge_set),
                    project_dir,
                    write_manifests: false,
                    is_zip: false,
                    parent_disk_check: ParentDiskCheck::CheckDisk,
                })
            }
        }
    }

    fn write_directory_result(
        &self,
        cs: &ComponentSet,
        target_format: TargetFormat,
        prepared: PreparedOutput,
        writer_items: &[WriterFormat],
    ) -> Result<ConvertResult, ConvertError> {
        let root_dest = prepared.default_directory.ok_or_else(|| ConvertError::Io {
            path: PathBuf::new(),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "missing output directory",
            ),
        })?;

        if prepared.write_manifests && target_format == TargetFormat::Metadata {
            write_manifests_to_dir(cs, &root_dest)?;
        }

        let ignore_anchor = prepared.project_dir.as_deref().unwrap_or(&root_dest);
        let anchor_ignore_path = ForceIgnore::find_path(ignore_anchor);
        let force_ignore = match &anchor_ignore_path {
            Some(path) => ForceIgnore::from_file(path)?,
            None => ForceIgnore::empty(),
        };
        let mut writer = StandardWriter::new(&root_dest, prepared.project_dir.as_deref());
        writer.write_all(writer_items, &force_ignore)?;

        let converted = if writer.converted_paths.is_empty() && writer.deleted_paths.is_empty() {
            Vec::new()
        } else {
            resolve_converted_paths_parallel(
                &self.registry,
                &writer.converted_paths,
                (anchor_ignore_path, force_ignore),
            )?
        };

        Ok(ConvertResult {
            package_path: prepared.package_path,
            zip_bytes: None,
            zip_file_count: 0,
            converted,
            deleted: writer.deleted_paths,
            incremental: None,
        })
    }
}

pub(crate) fn resolve_scope_for_component(comp: &SourceComponent) -> ResolveScope {
    let ty = comp.metadata_type();
    if ty.children.is_some()
        || ty.strategies.as_ref().and_then(|s| s.transformer)
            == Some(TransformerStrategy::DecomposedLabels)
    {
        ResolveScope::AllFiles
    } else {
        ResolveScope::FirstOnly
    }
}

pub(crate) fn resolve_converted_paths_parallel(
    registry: &RegistryAccess,
    paths: &[PathBuf],
    seed_ignore: (Option<PathBuf>, ForceIgnore),
) -> Result<Vec<SourceComponent>, ConvertError> {
    let mut ignore_by_file: HashMap<Option<PathBuf>, ForceIgnore> = HashMap::new();
    ignore_by_file.insert(seed_ignore.0, seed_ignore.1);

    let mut dir_ignores: HashMap<PathBuf, ForceIgnore> = HashMap::new();
    for path in paths {
        let dir = path.parent().unwrap_or(path);
        find_force_ignore_cached(dir, &mut dir_ignores, &mut ignore_by_file)?;
    }

    let resolver = MetadataResolver::new(
        registry.clone(),
        TreeHandle::cached_fs(),
        ForceIgnorePolicy::FromProject,
    );
    let resolved_batches = if paths.len() >= 16 {
        io_install(|| {
            paths
                .par_iter()
                .map(|path| {
                    let dir = path.parent().unwrap_or(path);
                    let fi = &dir_ignores[dir];
                    let (comps, _) = resolver.resolve_single_path_with_ignore(path, fi)?;
                    Ok::<_, ConvertError>(comps)
                })
                .collect::<Result<Vec<_>, _>>()
        })?
    } else {
        paths
            .iter()
            .map(|path| {
                let dir = path.parent().unwrap_or(path);
                let fi = &dir_ignores[dir];
                let (comps, _) = resolver.resolve_single_path_with_ignore(path, fi)?;
                Ok::<_, ConvertError>(comps)
            })
            .collect::<Result<Vec<_>, _>>()?
    };

    let mut out = Vec::new();
    for batch in resolved_batches {
        out.extend(batch);
    }
    Ok(out)
}

fn find_force_ignore_cached(
    dir: &Path,
    dir_ignores: &mut HashMap<PathBuf, ForceIgnore>,
    ignore_by_file: &mut HashMap<Option<PathBuf>, ForceIgnore>,
) -> Result<(), ConvertError> {
    if dir_ignores.contains_key(dir) {
        return Ok(());
    }
    let found = ForceIgnore::find_path(dir);
    let fi = if let Some(cached) = ignore_by_file.get(&found) {
        cached.clone()
    } else {
        let loaded = match &found {
            Some(path) => ForceIgnore::from_file(path)?,
            None => ForceIgnore::empty(),
        };
        ignore_by_file.insert(found, loaded.clone());
        loaded
    };
    dir_ignores.insert(dir.to_path_buf(), fi);
    Ok(())
}

pub(crate) fn prepare_component_set(
    components: &ComponentSet,
    output: &ConvertOutput,
) -> ComponentSet {
    let mut cs = components.clone();
    if let ConvertOutput::Directory {
        package_name: Some(name),
        ..
    }
    | ConvertOutput::Zip {
        package_name: Some(name),
        ..
    } = output
    {
        cs.set_full_name(Some(name.clone()));
    }
    cs
}

fn build_merge_set(registry: &RegistryAccess, merge_with: Vec<SourceComponent>) -> ComponentSet {
    let mut merge_set = ComponentSet::new(registry.clone());
    for comp in merge_with {
        let is_de_standalone = comp.metadata_type().strategies.as_ref().map(|s| s.adapter)
            == Some(AdapterStrategy::DigitalExperience)
            && comp.parent().and_then(SourceComponent::content).is_none();
        if is_de_standalone {
            merge_set.add_source_component(comp, None);
        } else {
            let target = comp.parent().cloned().unwrap_or(comp);
            merge_set.add_source_component(target, None);
        }
    }
    merge_set
}

pub(crate) fn transform_component_to_source(
    registry: &RegistryAccess,
    context: &mut ConvertContext,
    comp: &SourceComponent,
    prepared: &PreparedOutput,
) -> Result<Vec<crate::convert::types::WriteInfo>, ConvertError> {
    transform_component_to_source_with_check(
        registry,
        context,
        comp,
        prepared,
        prepared.parent_disk_check,
    )
}

pub(crate) fn transform_component_to_source_with_check(
    registry: &RegistryAccess,
    context: &mut ConvertContext,
    comp: &SourceComponent,
    prepared: &PreparedOutput,
    parent_check: ParentDiskCheck,
) -> Result<Vec<crate::convert::types::WriteInfo>, ConvertError> {
    let default_dir = prepared.default_directory.as_deref();
    let Some(ms) = &prepared.merge_set else {
        return transform_to_source(
            context,
            registry,
            default_dir,
            comp,
            None,
            None,
            parent_check,
        );
    };
    let matches = ms.source_components_for(&comp.metadata_type().id, &comp.full_name());
    if matches.is_empty() {
        return transform_to_source(
            context,
            registry,
            default_dir,
            comp,
            None,
            Some(ms),
            parent_check,
        );
    }
    let mut infos = Vec::new();
    for mw in &matches {
        infos.extend(transform_to_source(
            context,
            registry,
            default_dir,
            comp,
            Some(mw),
            Some(ms),
            parent_check,
        )?);
    }
    Ok(infos)
}

fn write_zip_result(
    cs: &ComponentSet,
    target_format: TargetFormat,
    package_path: Option<PathBuf>,
    writer_items: &[WriterFormat],
) -> Result<ConvertResult, ConvertError> {
    let mut archive = ArchiveWriter::new();
    if target_format == TargetFormat::Metadata {
        let pkg_xml = cs.render_package_xml(None)?;
        archive.add_bytes(Path::new(PACKAGE_XML_FILE), pkg_xml.into_bytes());
        for kind in cs.destructive_changes_kinds() {
            let destructive_xml = cs.render_package_xml(Some(kind))?;
            archive.add_bytes(Path::new(kind.filename()), destructive_xml.into_bytes());
        }
    }
    archive.add_all(writer_items)?;
    let (zip_bytes, zip_file_count) = archive.finish()?;

    if let Some(zip_path) = package_path {
        fs::write(&zip_path, &zip_bytes).map_err(|source| ConvertError::Io {
            path: zip_path.clone(),
            source,
        })?;
        Ok(ConvertResult {
            package_path: Some(zip_path),
            zip_bytes: None,
            zip_file_count: 0,
            converted: Vec::new(),
            deleted: Vec::new(),
            incremental: None,
        })
    } else {
        Ok(ConvertResult {
            package_path: None,
            zip_bytes: Some(zip_bytes),
            zip_file_count,
            converted: Vec::new(),
            deleted: Vec::new(),
            incremental: None,
        })
    }
}

fn write_manifests_to_dir(cs: &ComponentSet, root_dest: &Path) -> Result<(), ConvertError> {
    let pkg_xml_path = root_dest.join(PACKAGE_XML_FILE);
    fs::write(&pkg_xml_path, cs.render_package_xml(None)?).map_err(|source| ConvertError::Io {
        path: pkg_xml_path,
        source,
    })?;
    for kind in cs.destructive_changes_kinds() {
        let dpath = root_dest.join(kind.filename());
        fs::write(&dpath, cs.render_package_xml(Some(kind))?).map_err(|source| {
            ConvertError::Io {
                path: dpath,
                source,
            }
        })?;
    }
    Ok(())
}

fn resolve_package_path(
    output_dir: &Path,
    package_name: Option<&str>,
    unique_dir: UniqueDirPolicy,
) -> PathBuf {
    if let Some(name) = package_name {
        return output_dir.join(name);
    }
    match unique_dir {
        UniqueDirPolicy::Generate => {
            let ts = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |d| d.as_millis());
            output_dir.join(format!("{DEFAULT_PACKAGE_PREFIX}_{ts}"))
        }
        UniqueDirPolicy::Exact => normalize_lexical(output_dir),
    }
}
