//! Integration tests for persistent `blake3` incremental conversion (`MetadataConverter::convert_incremental`).

use std::collections::BTreeMap;
use std::fs::{self, OpenOptions};
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, SystemTime};

use busbar_sf_source_format::convert::{
    CacheVerifyMode, ConvertCache, ConvertOutput, ConvertResult, IncrementalResolvePolicy,
    MetadataConverter, TargetFormat, UniqueDirPolicy,
};
use busbar_sf_source_format::registry::RegistryAccess;
use busbar_sf_source_format::resolve::{ComponentSet, ComponentSetBuilder, ComponentSetOptions};
use zip::ZipArchive;

fn write_file(path: &Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent directories");
    }
    fs::write(path, contents).expect("write test file");
}

fn setup_sample_sfdx_project(root: &Path) {
    write_file(
        &root.join("sfdx-project.json"),
        "{\n  \"packageDirectories\": [{\"path\": \"force-app\", \"default\": true}],\n  \"sourceApiVersion\": \"62.0\"\n}\n",
    );
    let base = root.join("force-app/main/default");
    write_file(
        &base.join("classes/Alpha.cls"),
        "public class Alpha { /* v1 */ }\n",
    );
    write_file(
        &base.join("classes/Alpha.cls-meta.xml"),
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<ApexClass xmlns=\"http://soap.sforce.com/2006/04/metadata\">\n    <apiVersion>62.0</apiVersion>\n    <status>Active</status>\n</ApexClass>\n",
    );
    write_file(
        &base.join("classes/Beta.cls"),
        "public class Beta { /* v1 */ }\n",
    );
    write_file(
        &base.join("classes/Beta.cls-meta.xml"),
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<ApexClass xmlns=\"http://soap.sforce.com/2006/04/metadata\">\n    <apiVersion>62.0</apiVersion>\n    <status>Active</status>\n</ApexClass>\n",
    );
    write_file(
        &base.join("objects/Widget__c/Widget__c.object-meta.xml"),
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<CustomObject xmlns=\"http://soap.sforce.com/2006/04/metadata\">\n    <deploymentStatus>Deployed</deploymentStatus>\n    <label>Widget</label>\n    <pluralLabel>Widgets</pluralLabel>\n    <nameField>\n        <label>Widget Name</label>\n        <type>Text</type>\n    </nameField>\n    <sharingModel>ReadWrite</sharingModel>\n</CustomObject>\n",
    );
    write_file(
        &base.join("objects/Widget__c/fields/Code__c.field-meta.xml"),
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<CustomField xmlns=\"http://soap.sforce.com/2006/04/metadata\">\n    <fullName>Code__c</fullName>\n    <label>Code</label>\n    <length>40</length>\n    <type>Text</type>\n</CustomField>\n",
    );
    write_file(
        &base.join("objects/Widget__c/fields/Notes__c.field-meta.xml"),
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<CustomField xmlns=\"http://soap.sforce.com/2006/04/metadata\">\n    <fullName>Notes__c</fullName>\n    <label>Notes</label>\n    <type>LongTextArea</type>\n    <length>2000</length>\n    <visibleLines>3</visibleLines>\n</CustomField>\n",
    );
}

fn build_component_set(root: &Path, sourcepath: &Path, api_version: Option<&str>) -> ComponentSet {
    ComponentSetBuilder::build(&ComponentSetOptions {
        sourcepath: vec![sourcepath.to_path_buf()],
        project_dir: Some(root.to_path_buf()),
        api_version: api_version.map(str::to_owned),
        ..Default::default()
    })
    .expect("build component set")
}

fn snapshot_dir(root: &Path) -> BTreeMap<String, Vec<u8>> {
    fn walk(base: &Path, cur: &Path, out: &mut BTreeMap<String, Vec<u8>>) {
        let mut entries: Vec<PathBuf> = fs::read_dir(cur)
            .expect("read_dir")
            .filter_map(|e| e.ok().map(|d| d.path()))
            .collect();
        entries.sort();
        for p in entries {
            if p.is_dir() {
                walk(base, &p, out);
            } else {
                let rel = p
                    .strip_prefix(base)
                    .expect("strip prefix")
                    .to_string_lossy()
                    .replace('\\', "/");
                out.insert(rel, fs::read(&p).expect("read file"));
            }
        }
    }
    let mut map = BTreeMap::new();
    if root.exists() {
        walk(root, root, &mut map);
    }
    map
}

fn snapshot_zip_bytes(zip_bytes: &[u8]) -> BTreeMap<String, Vec<u8>> {
    let mut archive = ZipArchive::new(Cursor::new(zip_bytes)).expect("open zip archive");
    let mut map = BTreeMap::new();
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).expect("zip entry");
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().expect("zip entry name").into_owned();
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf).expect("read zip member");
        map.insert(name, buf);
    }
    map
}

fn run_cold_dir(cs: &ComponentSet, target_format: TargetFormat, out_dir: &Path) -> ConvertResult {
    let converter = MetadataConverter::new(RegistryAccess::new());
    converter
        .convert(
            cs,
            target_format,
            ConvertOutput::Directory {
                output_dir: out_dir.to_path_buf(),
                package_name: None,
                unique_dir: UniqueDirPolicy::Exact,
            },
        )
        .expect("cold conversion")
}

fn run_inc_dir(
    cs: &ComponentSet,
    target_format: TargetFormat,
    out_dir: &Path,
    cache: &ConvertCache,
) -> ConvertResult {
    let converter = MetadataConverter::new(RegistryAccess::new());
    converter
        .convert_incremental(
            cs,
            target_format,
            ConvertOutput::Directory {
                output_dir: out_dir.to_path_buf(),
                package_name: None,
                unique_dir: UniqueDirPolicy::Exact,
            },
            cache,
        )
        .expect("incremental conversion")
}

#[test]
fn incremental_dir_cold_then_warm_noop() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let src = tmp.path().join("proj");
    setup_sample_sfdx_project(&src);
    let cache = ConvertCache::new(tmp.path().join("cache"));
    let inc_out = tmp.path().join("inc_mdapi");
    let cold_out = tmp.path().join("cold_mdapi");

    let cs = build_component_set(&src, &src.join("force-app"), None);
    let cold_res = run_inc_dir(&cs, TargetFormat::Metadata, &inc_out, &cache);
    let cold_stats = cold_res.incremental.expect("stats");
    assert_eq!(cold_stats.total_units, 3);
    assert_eq!(cold_stats.cached_units, 0);
    assert_eq!(cold_stats.rebuilt_units, 3);
    assert!(cold_stats.written_files >= 5);

    run_cold_dir(&cs, TargetFormat::Metadata, &cold_out);
    assert_eq!(snapshot_dir(&inc_out), snapshot_dir(&cold_out));

    let warm_res = run_inc_dir(&cs, TargetFormat::Metadata, &inc_out, &cache);
    let warm_stats = warm_res.incremental.expect("stats");
    assert_eq!(warm_stats.total_units, 3);
    assert_eq!(warm_stats.cached_units, 3);
    assert_eq!(warm_stats.rebuilt_units, 0);
    assert_eq!(warm_stats.written_files, 0);
    assert!(warm_stats.skipped_writes >= 6);
    assert_eq!(warm_stats.removed_stale_files, 0);
    assert!(warm_res.converted.is_empty());
    assert_eq!(snapshot_dir(&inc_out), snapshot_dir(&cold_out));
}

#[test]
fn incremental_dir_single_field_edit_rebuilds_only_parent_object() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let src = tmp.path().join("proj");
    setup_sample_sfdx_project(&src);
    let cache = ConvertCache::new(tmp.path().join("cache"));
    let inc_out = tmp.path().join("inc_mdapi");

    let cs1 = build_component_set(&src, &src.join("force-app"), None);
    let first = run_inc_dir(&cs1, TargetFormat::Metadata, &inc_out, &cache);
    assert_eq!(first.incremental.expect("stats").rebuilt_units, 3);

    write_file(
        &src.join("force-app/main/default/objects/Widget__c/fields/Code__c.field-meta.xml"),
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<CustomField xmlns=\"http://soap.sforce.com/2006/04/metadata\">\n    <fullName>Code__c</fullName>\n    <label>Updated Code</label>\n    <length>80</length>\n    <type>Text</type>\n</CustomField>\n",
    );

    let cs2 = build_component_set(&src, &src.join("force-app"), None);
    let second = run_inc_dir(&cs2, TargetFormat::Metadata, &inc_out, &cache);
    let stats = second.incremental.expect("stats");
    assert_eq!(stats.total_units, 3);
    assert_eq!(stats.cached_units, 2);
    assert_eq!(stats.rebuilt_units, 1);

    let cold_out = tmp.path().join("cold_mdapi");
    run_cold_dir(&cs2, TargetFormat::Metadata, &cold_out);
    assert_eq!(snapshot_dir(&inc_out), snapshot_dir(&cold_out));
}

#[test]
fn incremental_mdapi_to_source_single_object_edit() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let proj = tmp.path().join("proj");
    setup_sample_sfdx_project(&proj);
    let mdapi_in = tmp.path().join("mdapi_in");
    let cs_init = build_component_set(&proj, &proj.join("force-app"), None);
    run_cold_dir(&cs_init, TargetFormat::Metadata, &mdapi_in);

    let cache = ConvertCache::new(tmp.path().join("cache"));
    let inc_src = tmp.path().join("inc_src");
    let cs_md1 = ComponentSetBuilder::build(&ComponentSetOptions {
        sourcepath: vec![mdapi_in.clone()],
        ..Default::default()
    })
    .expect("build mdapi cs");
    let first = run_inc_dir(&cs_md1, TargetFormat::Source, &inc_src, &cache);
    assert_eq!(first.incremental.expect("stats").rebuilt_units, 3);

    let warm = run_inc_dir(&cs_md1, TargetFormat::Source, &inc_src, &cache);
    let warm_stats = warm.incremental.expect("stats");
    assert_eq!(warm_stats.cached_units, 3);
    assert_eq!(warm_stats.rebuilt_units, 0);
    assert_eq!(warm_stats.written_files, 0);

    // Edit Widget__c.object: remove Notes__c and rename Code__c label.
    write_file(
        &mdapi_in.join("objects/Widget__c.object"),
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<CustomObject xmlns=\"http://soap.sforce.com/2006/04/metadata\">\n    <deploymentStatus>Deployed</deploymentStatus>\n    <fields>\n        <fullName>Code__c</fullName>\n        <label>Code Renamed</label>\n        <length>40</length>\n        <type>Text</type>\n    </fields>\n    <label>Widget</label>\n    <nameField>\n        <label>Widget Name</label>\n        <type>Text</type>\n    </nameField>\n    <pluralLabel>Widgets</pluralLabel>\n    <sharingModel>ReadWrite</sharingModel>\n</CustomObject>\n",
    );

    let cs_md2 = ComponentSetBuilder::build(&ComponentSetOptions {
        sourcepath: vec![mdapi_in],
        ..Default::default()
    })
    .expect("rebuild mdapi cs");
    let second = run_inc_dir(&cs_md2, TargetFormat::Source, &inc_src, &cache);
    let stats2 = second.incremental.expect("stats");
    assert_eq!(stats2.cached_units, 2);
    assert_eq!(stats2.rebuilt_units, 1);
    assert_eq!(stats2.removed_stale_files, 1);
    assert!(
        !inc_src
            .join("objects/Widget__c/fields/Notes__c.field-meta.xml")
            .exists()
    );

    let cold_src = tmp.path().join("cold_src");
    run_cold_dir(&cs_md2, TargetFormat::Source, &cold_src);
    assert_eq!(snapshot_dir(&inc_src), snapshot_dir(&cold_src));
}

#[test]
fn incremental_dir_add_and_delete_component_prunes_stale_and_updates_manifest() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let src = tmp.path().join("proj");
    setup_sample_sfdx_project(&src);
    let cache = ConvertCache::new(tmp.path().join("cache"));
    let inc_out = tmp.path().join("inc_mdapi");

    let cs1 = build_component_set(&src, &src.join("force-app"), None);
    run_inc_dir(&cs1, TargetFormat::Metadata, &inc_out, &cache);

    // Add Gamma Apex class + CustomTab (so deleting CustomTab prunes empty tabs/ dir too).
    let base = src.join("force-app/main/default");
    write_file(&base.join("classes/Gamma.cls"), "public class Gamma {}\n");
    write_file(
        &base.join("classes/Gamma.cls-meta.xml"),
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<ApexClass xmlns=\"http://soap.sforce.com/2006/04/metadata\">\n    <apiVersion>62.0</apiVersion>\n    <status>Active</status>\n</ApexClass>\n",
    );
    write_file(
        &base.join("tabs/Widget__c.tab-meta.xml"),
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<CustomTab xmlns=\"http://soap.sforce.com/2006/04/metadata\">\n    <customObject>true</customObject>\n    <motif>Custom1: Heart</motif>\n</CustomTab>\n",
    );

    let cs2 = build_component_set(&src, &src.join("force-app"), None);
    let add_res = run_inc_dir(&cs2, TargetFormat::Metadata, &inc_out, &cache);
    let add_stats = add_res.incremental.expect("stats");
    assert_eq!(add_stats.cached_units, 3);
    assert_eq!(add_stats.rebuilt_units, 2);
    assert!(inc_out.join("tabs/Widget__c.tab").exists());

    // Delete Gamma and Widget__c.tab.
    fs::remove_file(base.join("classes/Gamma.cls")).expect("rm cls");
    fs::remove_file(base.join("classes/Gamma.cls-meta.xml")).expect("rm cls meta");
    fs::remove_dir_all(base.join("tabs")).expect("rm tabs");

    let cs3 = build_component_set(&src, &src.join("force-app"), None);
    let del_res = run_inc_dir(&cs3, TargetFormat::Metadata, &inc_out, &cache);
    let del_stats = del_res.incremental.expect("stats");
    assert_eq!(del_stats.cached_units, 3);
    assert_eq!(del_stats.rebuilt_units, 0);
    assert_eq!(del_stats.removed_stale_files, 3);
    assert!(!inc_out.join("tabs").exists());

    let cold_out = tmp.path().join("cold_mdapi");
    run_cold_dir(&cs3, TargetFormat::Metadata, &cold_out);
    assert_eq!(snapshot_dir(&inc_out), snapshot_dir(&cold_out));
}

#[test]
fn incremental_dir_deleted_or_tampered_output_self_heals() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let src = tmp.path().join("proj");
    setup_sample_sfdx_project(&src);
    let cache = ConvertCache::new(tmp.path().join("cache"));
    let inc_out = tmp.path().join("inc_mdapi");

    let cs = build_component_set(&src, &src.join("force-app"), None);
    run_inc_dir(&cs, TargetFormat::Metadata, &inc_out, &cache);

    fs::remove_file(inc_out.join("classes/Alpha.cls")).expect("delete Alpha.cls");
    fs::write(inc_out.join("classes/Beta.cls"), "corrupted!").expect("tamper Beta.cls");

    let heal = run_inc_dir(&cs, TargetFormat::Metadata, &inc_out, &cache);
    let stats = heal.incremental.expect("stats");
    assert_eq!(stats.cached_units, 1);
    assert_eq!(stats.rebuilt_units, 2);

    let cold_out = tmp.path().join("cold_mdapi");
    run_cold_dir(&cs, TargetFormat::Metadata, &cold_out);
    assert_eq!(snapshot_dir(&inc_out), snapshot_dir(&cold_out));
}

#[test]
fn incremental_env_invalidation_forceignore_and_api_version() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let src = tmp.path().join("proj");
    setup_sample_sfdx_project(&src);
    let cache = ConvertCache::new(tmp.path().join("cache"));
    let inc_out = tmp.path().join("inc_mdapi");

    let cs1 = build_component_set(&src, &src.join("force-app"), Some("61.0"));
    run_inc_dir(&cs1, TargetFormat::Metadata, &inc_out, &cache);

    let cs2 = build_component_set(&src, &src.join("force-app"), Some("62.0"));
    let api_res = run_inc_dir(&cs2, TargetFormat::Metadata, &inc_out, &cache);
    let api_stats = api_res.incremental.expect("stats");
    assert_eq!(api_stats.cached_units, 0);
    assert_eq!(api_stats.rebuilt_units, 3);
    let pkg_xml = fs::read_to_string(inc_out.join("package.xml")).expect("read package.xml");
    assert!(pkg_xml.contains("<version>62.0</version>"));

    // Now add .forceignore ignoring Beta; force_ignored_paths + .forceignore file hash change.
    write_file(&src.join(".forceignore"), "**/Beta.*\n");
    let cs3 = build_component_set(&src, &src.join("force-app"), Some("62.0"));
    let ign_res = run_inc_dir(&cs3, TargetFormat::Metadata, &inc_out, &cache);
    let ign_stats = ign_res.incremental.expect("stats");
    assert_eq!(ign_stats.cached_units, 0);
    assert_eq!(ign_stats.total_units, 2);

    let cold_out = tmp.path().join("cold_mdapi");
    run_cold_dir(&cs3, TargetFormat::Metadata, &cold_out);
    assert_eq!(snapshot_dir(&inc_out), snapshot_dir(&cold_out));
}

#[test]
fn incremental_always_hash_detects_same_size_edit() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let src = tmp.path().join("proj");
    setup_sample_sfdx_project(&src);
    let cache =
        ConvertCache::new(tmp.path().join("cache")).with_verify_mode(CacheVerifyMode::AlwaysHash);
    let inc_out = tmp.path().join("inc_mdapi");

    let cs1 = build_component_set(&src, &src.join("force-app"), None);
    run_inc_dir(&cs1, TargetFormat::Metadata, &inc_out, &cache);

    let cls_path = src.join("force-app/main/default/classes/Alpha.cls");
    let orig_mtime = fs::metadata(&cls_path)
        .expect("stat")
        .modified()
        .expect("mtime");
    let updated = "public class Alpha { /* v2 */ }\n";
    assert_eq!(updated.len(), fs::read(&cls_path).expect("read").len());
    fs::write(&cls_path, updated).expect("overwrite same size");
    OpenOptions::new()
        .write(true)
        .open(&cls_path)
        .expect("reopen")
        .set_modified(orig_mtime)
        .expect("restore mtime");

    let cs2 = build_component_set(&src, &src.join("force-app"), None);
    let res = run_inc_dir(&cs2, TargetFormat::Metadata, &inc_out, &cache);
    let stats = res.incremental.expect("stats");
    assert_eq!(stats.cached_units, 2);
    assert_eq!(stats.rebuilt_units, 1);

    let cold_out = tmp.path().join("cold_mdapi");
    run_cold_dir(&cs2, TargetFormat::Metadata, &cold_out);
    assert_eq!(snapshot_dir(&inc_out), snapshot_dir(&cold_out));
}

#[test]
fn incremental_corrupt_cache_file_falls_back_cleanly() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let src = tmp.path().join("proj");
    setup_sample_sfdx_project(&src);
    let cache_dir = tmp.path().join("cache");
    let cache = ConvertCache::new(&cache_dir);
    let inc_out = tmp.path().join("inc_mdapi");

    let cs = build_component_set(&src, &src.join("force-app"), None);
    run_inc_dir(&cs, TargetFormat::Metadata, &inc_out, &cache);

    let mut sfsfc_files: Vec<PathBuf> = fs::read_dir(&cache_dir)
        .expect("read cache dir")
        .filter_map(|e| e.ok().map(|d| d.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "sfsfc"))
        .collect();
    assert_eq!(sfsfc_files.len(), 1);
    let index_file = sfsfc_files.pop().expect("index file");
    fs::write(&index_file, b"CORRUPT_HEADER_DATA").expect("corrupt index");

    let rebuilt = run_inc_dir(&cs, TargetFormat::Metadata, &inc_out, &cache);
    let stats = rebuilt.incremental.expect("stats");
    assert_eq!(stats.cached_units, 0);
    assert_eq!(stats.rebuilt_units, 3);

    let warm = run_inc_dir(&cs, TargetFormat::Metadata, &inc_out, &cache);
    assert_eq!(warm.incremental.expect("stats").cached_units, 3);
}

#[test]
fn incremental_zip_exact_dir_matches_cold_bytes() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let src = tmp.path().join("proj");
    setup_sample_sfdx_project(&src);
    let cache = ConvertCache::new(tmp.path().join("cache"));
    let converter = MetadataConverter::new(RegistryAccess::new());
    let zip_dir = tmp.path().join("zips");

    let cs1 = build_component_set(&src, &src.join("force-app"), None);
    let z1 = converter
        .convert_incremental(
            &cs1,
            TargetFormat::Metadata,
            ConvertOutput::Zip {
                output_dir: Some(zip_dir.clone()),
                package_name: Some("pkg".to_owned()),
                unique_dir: UniqueDirPolicy::Exact,
            },
            &cache,
        )
        .expect("cold zip");
    assert_eq!(z1.incremental.expect("stats").rebuilt_units, 3);
    let zip_path = z1.package_path.expect("zip path");

    let z_warm = converter
        .convert_incremental(
            &cs1,
            TargetFormat::Metadata,
            ConvertOutput::Zip {
                output_dir: Some(zip_dir.clone()),
                package_name: Some("pkg".to_owned()),
                unique_dir: UniqueDirPolicy::Exact,
            },
            &cache,
        )
        .expect("warm zip");
    let zw_stats = z_warm.incremental.expect("stats");
    assert_eq!(zw_stats.cached_units, 3);
    assert_eq!(zw_stats.rebuilt_units, 0);
    assert_eq!(zw_stats.written_files, 0);

    // Edit 1 file, keep 2 units cached via raw_copy_file.
    write_file(
        &src.join("force-app/main/default/classes/Alpha.cls"),
        "public class Alpha { /* zip v2 */ }\n",
    );
    let cs2 = build_component_set(&src, &src.join("force-app"), None);
    let z_edit = converter
        .convert_incremental(
            &cs2,
            TargetFormat::Metadata,
            ConvertOutput::Zip {
                output_dir: Some(zip_dir),
                package_name: Some("pkg".to_owned()),
                unique_dir: UniqueDirPolicy::Exact,
            },
            &cache,
        )
        .expect("edit zip");
    let edit_stats = z_edit.incremental.expect("stats");
    assert_eq!(edit_stats.cached_units, 2);
    assert_eq!(edit_stats.rebuilt_units, 1);

    let cold_zip = converter
        .convert(
            &cs2,
            TargetFormat::Metadata,
            ConvertOutput::Zip {
                output_dir: None,
                package_name: Some("pkg".to_owned()),
                unique_dir: UniqueDirPolicy::Exact,
            },
        )
        .expect("cold zip");
    assert_eq!(
        snapshot_zip_bytes(&fs::read(&zip_path).expect("read zip")),
        snapshot_zip_bytes(cold_zip.zip_bytes.as_deref().expect("cold zip bytes"))
    );
}

#[test]
fn incremental_zip_in_memory_matches_cold_bytes() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let src = tmp.path().join("proj");
    setup_sample_sfdx_project(&src);
    let cache = ConvertCache::new(tmp.path().join("cache"));
    let converter = MetadataConverter::new(RegistryAccess::new());

    let cs = build_component_set(&src, &src.join("force-app"), None);
    let mem1 = converter
        .convert_incremental(
            &cs,
            TargetFormat::Metadata,
            ConvertOutput::Zip {
                output_dir: None,
                package_name: Some("mem_pkg".to_owned()),
                unique_dir: UniqueDirPolicy::Exact,
            },
            &cache,
        )
        .expect("mem zip cold");
    assert_eq!(mem1.incremental.expect("stats").rebuilt_units, 3);

    let mem2 = converter
        .convert_incremental(
            &cs,
            TargetFormat::Metadata,
            ConvertOutput::Zip {
                output_dir: None,
                package_name: Some("mem_pkg".to_owned()),
                unique_dir: UniqueDirPolicy::Exact,
            },
            &cache,
        )
        .expect("mem zip warm");
    let mem2_stats = mem2.incremental.expect("stats");
    assert_eq!(mem2_stats.cached_units, 3);
    assert_eq!(mem2_stats.written_files, 0);

    let cold_zip = converter
        .convert(
            &cs,
            TargetFormat::Metadata,
            ConvertOutput::Zip {
                output_dir: None,
                package_name: Some("mem_pkg".to_owned()),
                unique_dir: UniqueDirPolicy::Exact,
            },
        )
        .expect("cold mem zip");
    assert_eq!(
        snapshot_zip_bytes(mem2.zip_bytes.as_deref().expect("mem bytes")),
        snapshot_zip_bytes(cold_zip.zip_bytes.as_deref().expect("cold zip bytes"))
    );
}

#[test]
fn incremental_resolve_policy_all_vs_rebuilt_only() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let src = tmp.path().join("proj");
    setup_sample_sfdx_project(&src);
    let cache_rebuilt = ConvertCache::new(tmp.path().join("cache"))
        .with_resolve_policy(IncrementalResolvePolicy::RebuiltOnly);
    let cache_all = ConvertCache::new(tmp.path().join("cache"))
        .with_resolve_policy(IncrementalResolvePolicy::All);
    let inc_out = tmp.path().join("inc_mdapi");

    let cs = build_component_set(&src, &src.join("force-app"), None);
    let first = run_inc_dir(&cs, TargetFormat::Metadata, &inc_out, &cache_rebuilt);
    assert_eq!(first.converted.len(), 3);

    let warm_rebuilt_only = run_inc_dir(&cs, TargetFormat::Metadata, &inc_out, &cache_rebuilt);
    assert_eq!(warm_rebuilt_only.converted.len(), 0);

    let warm_all = run_inc_dir(&cs, TargetFormat::Metadata, &inc_out, &cache_all);
    assert_eq!(warm_all.converted.len(), 3);
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum IncludeNotes {
    Yes,
    No,
}

fn build_widget_object_xml(
    obj_label: &str,
    code_label: &str,
    include_notes: IncludeNotes,
) -> String {
    let notes = if include_notes == IncludeNotes::Yes {
        "\n    <fields>\n        <fullName>Notes__c</fullName>\n        <label>Notes</label>\n        <length>2000</length>\n        <type>LongTextArea</type>\n        <visibleLines>3</visibleLines>\n    </fields>"
    } else {
        ""
    };
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<CustomObject xmlns=\"http://soap.sforce.com/2006/04/metadata\">\n    <deploymentStatus>Deployed</deploymentStatus>\n    <fields>\n        <fullName>Code__c</fullName>\n        <label>{code_label}</label>\n        <length>40</length>\n        <type>Text</type>\n    </fields>{notes}\n    <label>{obj_label}</label>\n    <nameField>\n        <label>Widget Name</label>\n        <type>Text</type>\n    </nameField>\n    <pluralLabel>Widgets</pluralLabel>\n    <sharingModel>ReadWrite</sharingModel>\n</CustomObject>\n"
    )
}

fn backdate_tree_mtimes(root: &Path, secs_ago: u64) {
    let target = SystemTime::now()
        .checked_sub(Duration::from_secs(secs_ago))
        .expect("past time");
    let mut stack = vec![root.to_path_buf()];
    while let Some(cur) = stack.pop() {
        for entry in fs::read_dir(&cur).expect("read_dir").flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.is_file() {
                OpenOptions::new()
                    .write(true)
                    .open(&path)
                    .expect("open for mtime")
                    .set_modified(target)
                    .expect("set_modified");
            }
        }
    }
}

fn file_mtime(path: &Path) -> SystemTime {
    fs::metadata(path)
        .expect("stat file")
        .modified()
        .expect("modified time")
}

#[test]
fn incremental_mdapi_to_source_only_writes_changed_child_files() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mdapi_in = tmp.path().join("mdapi_in");
    write_file(
        &mdapi_in.join("objects/Widget__c.object"),
        &build_widget_object_xml("Widget", "Code", IncludeNotes::Yes),
    );
    write_file(
        &mdapi_in.join("classes/Alpha.cls"),
        "public class Alpha {}\n",
    );
    write_file(
        &mdapi_in.join("classes/Alpha.cls-meta.xml"),
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<ApexClass xmlns=\"http://soap.sforce.com/2006/04/metadata\">\n    <apiVersion>62.0</apiVersion>\n    <status>Active</status>\n</ApexClass>\n",
    );
    let cache = ConvertCache::new(tmp.path().join("cache"));
    let inc_src = tmp.path().join("inc_src");
    let obj_meta = inc_src.join("main/default/objects/Widget__c/Widget__c.object-meta.xml");
    let code_xml = inc_src.join("main/default/objects/Widget__c/fields/Code__c.field-meta.xml");
    let notes_xml = inc_src.join("main/default/objects/Widget__c/fields/Notes__c.field-meta.xml");

    let run_mdapi = |out_dir: &Path, use_inc: CacheVerifyMode| -> ConvertResult {
        let cs = ComponentSetBuilder::build(&ComponentSetOptions {
            sourcepath: vec![mdapi_in.clone()],
            ..Default::default()
        })
        .expect("build mdapi cs");
        let c = cache.clone().with_verify_mode(use_inc);
        run_inc_dir(&cs, TargetFormat::Source, out_dir, &c)
    };

    let cold_stats = run_mdapi(&inc_src, CacheVerifyMode::StatThenHash)
        .incremental
        .expect("stats");
    assert_eq!(cold_stats.written_files, 5);

    backdate_tree_mtimes(&inc_src, 20);
    let obj_t0 = file_mtime(&obj_meta);
    let code_t0 = file_mtime(&code_xml);
    let notes_t0 = file_mtime(&notes_xml);

    // 1. Edit only parent <label>: only Widget__c.object-meta.xml is written.
    write_file(
        &mdapi_in.join("objects/Widget__c.object"),
        &build_widget_object_xml("Widget Renamed", "Code", IncludeNotes::Yes),
    );
    let parent_edit = run_mdapi(&inc_src, CacheVerifyMode::StatThenHash)
        .incremental
        .expect("stats");
    assert_eq!(parent_edit.rebuilt_units, 1);
    assert_eq!(parent_edit.written_files, 1);
    assert_eq!(parent_edit.skipped_writes, 4);
    assert_ne!(file_mtime(&obj_meta), obj_t0);
    assert_eq!(file_mtime(&code_xml), code_t0);
    assert_eq!(file_mtime(&notes_xml), notes_t0);

    // 2. Edit only Code__c child field: only Code__c.field-meta.xml is written.
    backdate_tree_mtimes(&inc_src, 20);
    let obj_t1 = file_mtime(&obj_meta);
    let notes_t1 = file_mtime(&notes_xml);
    write_file(
        &mdapi_in.join("objects/Widget__c.object"),
        &build_widget_object_xml("Widget Renamed", "Code V2", IncludeNotes::Yes),
    );
    let field_edit = run_mdapi(&inc_src, CacheVerifyMode::StatThenHash)
        .incremental
        .expect("stats");
    assert_eq!(field_edit.rebuilt_units, 1);
    assert_eq!(field_edit.written_files, 1);
    assert_eq!(field_edit.skipped_writes, 4);
    assert_eq!(file_mtime(&obj_meta), obj_t1);
    assert_ne!(file_mtime(&code_xml), code_t0);
    assert_eq!(file_mtime(&notes_xml), notes_t1);

    // 3. Delete Notes__c field: 0 writes, 1 stale file pruned, remaining children untouched.
    backdate_tree_mtimes(&inc_src, 20);
    let code_t2 = file_mtime(&code_xml);
    write_file(
        &mdapi_in.join("objects/Widget__c.object"),
        &build_widget_object_xml("Widget Renamed", "Code V2", IncludeNotes::No),
    );
    let del_edit = run_mdapi(&inc_src, CacheVerifyMode::StatThenHash)
        .incremental
        .expect("stats");
    assert_eq!(del_edit.rebuilt_units, 1);
    assert_eq!(del_edit.written_files, 0);
    assert_eq!(del_edit.removed_stale_files, 1);
    assert!(!notes_xml.exists());
    assert_eq!(file_mtime(&code_xml), code_t2);

    let cold_cs = ComponentSetBuilder::build(&ComponentSetOptions {
        sourcepath: vec![mdapi_in],
        ..Default::default()
    })
    .expect("cold cs");
    let cold_out = tmp.path().join("cold_src");
    run_cold_dir(&cold_cs, TargetFormat::Source, &cold_out);
    assert_eq!(snapshot_dir(&inc_src), snapshot_dir(&cold_out));
}

fn run_git(repo: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("execute git");
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn single_cache_file_bytes(cache_dir: &Path) -> Vec<u8> {
    let files: Vec<PathBuf> = fs::read_dir(cache_dir)
        .expect("read cache dir")
        .filter_map(|e| e.ok().map(|d| d.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "sfsfc"))
        .collect();
    assert_eq!(files.len(), 1, "expected exactly one .sfsfc file");
    fs::read(&files[0]).expect("read sfsfc")
}

fn decode_hex_sha1_needle(hex: &str) -> [u8; 33] {
    assert_eq!(hex.len(), 40, "expected 40-char SHA-1 hex OID");
    let mut needle = [0u8; 33];
    needle[0] = 1; // DigestKind::GitSha1
    for i in 0..20 {
        needle[1 + i] = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).expect("hex byte");
    }
    needle
}

fn bytes_contain_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

fn setup_git_sample_repo(src: &Path) -> [u8; 33] {
    setup_sample_sfdx_project(src);
    run_git(src, &["init"]);
    run_git(src, &["config", "user.name", "Test"]);
    run_git(src, &["config", "user.email", "test@example.com"]);
    run_git(src, &["config", "core.autocrlf", "false"]);
    run_git(src, &["config", "core.eol", "lf"]);
    // Set file mtime safely older than `.git/index` so entries are non-racy clean.
    backdate_tree_mtimes(&src.join("force-app"), 10);
    run_git(src, &["add", "."]);
    run_git(src, &["commit", "-m", "init"]);
    let alpha_oid = run_git(
        src,
        &["rev-parse", "HEAD:force-app/main/default/classes/Alpha.cls"],
    );
    decode_hex_sha1_needle(&alpha_oid)
}

/// Hermetic git config: the index-reuse path intentionally bails out when user-level git
/// config defines a `filter` (e.g. git-lfs on CI runners), so point HOME at an empty dir.
fn isolate_git_user_config(base: &Path) {
    let fake_home = base.join("home");
    fs::create_dir_all(&fake_home).expect("create fake home");
    // SAFETY: this is the only test in this binary that reads HOME/XDG_CONFIG_HOME.
    unsafe { std::env::set_var("HOME", &fake_home) };
    // SAFETY: as above.
    unsafe { std::env::set_var("XDG_CONFIG_HOME", fake_home.join(".config")) };
}

#[test]
fn incremental_git_index_oid_reuse_dirty_untracked_and_gitattributes() {
    let tmp = tempfile::tempdir().expect("tempdir");
    isolate_git_user_config(tmp.path());
    let src = tmp.path().join("repo");
    let oid_needle = setup_git_sample_repo(&src);

    // 1. Git v2 index cold build reuses Git SHA-1 OID + warm no-op.
    let cache_v2_dir = tmp.path().join("cache_v2");
    let cache_v2 = ConvertCache::new(&cache_v2_dir);
    let out_v2 = tmp.path().join("out_v2");
    let cs1 = build_component_set(&src, &src.join("force-app"), None);
    let cold_v2 = run_inc_dir(&cs1, TargetFormat::Metadata, &out_v2, &cache_v2);
    assert_eq!(cold_v2.incremental.expect("stats").rebuilt_units, 3);
    assert!(bytes_contain_subslice(
        &single_cache_file_bytes(&cache_v2_dir),
        &oid_needle
    ));
    let warm = run_inc_dir(&cs1, TargetFormat::Metadata, &out_v2, &cache_v2)
        .incremental
        .expect("stats");
    assert_eq!(
        (warm.cached_units, warm.rebuilt_units, warm.written_files),
        (3, 0, 0)
    );

    // 2. Unstaged dirty edit + untracked component are detected accurately.
    write_file(
        &src.join("force-app/main/default/classes/Alpha.cls"),
        "public class Alpha { /* dirty */ }\n",
    );
    write_file(
        &src.join("force-app/main/default/classes/Gamma.cls"),
        "public class Gamma {}\n",
    );
    write_file(
        &src.join("force-app/main/default/classes/Gamma.cls-meta.xml"),
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<ApexClass xmlns=\"http://soap.sforce.com/2006/04/metadata\">\n    <apiVersion>62.0</apiVersion>\n    <status>Active</status>\n</ApexClass>\n",
    );
    let cs_dirty = build_component_set(&src, &src.join("force-app"), None);
    let dirty_res = run_inc_dir(&cs_dirty, TargetFormat::Metadata, &out_v2, &cache_v2);
    let dirty_stats = dirty_res.incremental.expect("stats");
    assert_eq!(
        (dirty_stats.cached_units, dirty_stats.rebuilt_units),
        (2, 2)
    );
    let cold_dirty = tmp.path().join("cold_dirty");
    run_cold_dir(&cs_dirty, TargetFormat::Metadata, &cold_dirty);
    assert_eq!(snapshot_dir(&out_v2), snapshot_dir(&cold_dirty));

    // Clean working tree before checking .gitattributes, AlwaysHash, and v4 index.
    fs::remove_file(src.join("force-app/main/default/classes/Gamma.cls")).expect("rm untracked");
    fs::remove_file(src.join("force-app/main/default/classes/Gamma.cls-meta.xml"))
        .expect("rm untracked");
    run_git(&src, &["checkout", "--", "."]);
    backdate_tree_mtimes(&src.join("force-app"), 10);
    run_git(&src, &["update-index", "--refresh"]);

    // 3. `.gitattributes` and `AlwaysHash` disable Git OID reuse cleanly.
    write_file(&src.join(".gitattributes"), "*.cls text\n");
    let cache_attr_dir = tmp.path().join("cache_attr");
    let cs_clean = build_component_set(&src, &src.join("force-app"), None);
    run_inc_dir(
        &cs_clean,
        TargetFormat::Metadata,
        &tmp.path().join("out_attr"),
        &ConvertCache::new(&cache_attr_dir),
    );
    assert!(!bytes_contain_subslice(
        &single_cache_file_bytes(&cache_attr_dir),
        &oid_needle
    ));
    fs::remove_file(src.join(".gitattributes")).expect("rm gitattributes");

    let cache_always_dir = tmp.path().join("cache_always");
    let cache_always =
        ConvertCache::new(&cache_always_dir).with_verify_mode(CacheVerifyMode::AlwaysHash);
    run_inc_dir(
        &cs_clean,
        TargetFormat::Metadata,
        &tmp.path().join("out_always"),
        &cache_always,
    );
    assert!(!bytes_contain_subslice(
        &single_cache_file_bytes(&cache_always_dir),
        &oid_needle
    ));

    // 4. Git index v4 (prefix-compressed) reuses Git OID, hits warm, and detects dirty edits.
    run_git(&src, &["update-index", "--index-version", "4"]);
    let cache_v4_dir = tmp.path().join("cache_v4");
    let cache_v4 = ConvertCache::new(&cache_v4_dir);
    let out_v4 = tmp.path().join("out_v4");
    run_inc_dir(&cs_clean, TargetFormat::Metadata, &out_v4, &cache_v4);
    assert!(bytes_contain_subslice(
        &single_cache_file_bytes(&cache_v4_dir),
        &oid_needle
    ));
    let v4_warm = run_inc_dir(&cs_clean, TargetFormat::Metadata, &out_v4, &cache_v4)
        .incremental
        .expect("stats");
    assert_eq!(v4_warm.cached_units, 3);
    write_file(
        &src.join("force-app/main/default/classes/Alpha.cls"),
        "public class Alpha { /* v4 dirty */ }\n",
    );
    let cs_v4_dirty = build_component_set(&src, &src.join("force-app"), None);
    let v4_dirty = run_inc_dir(&cs_v4_dirty, TargetFormat::Metadata, &out_v4, &cache_v4);
    assert_eq!(v4_dirty.incremental.expect("stats").rebuilt_units, 1);
    let cold_v4_dirty = tmp.path().join("cold_v4_dirty");
    run_cold_dir(&cs_v4_dirty, TargetFormat::Metadata, &cold_v4_dirty);
    assert_eq!(snapshot_dir(&out_v4), snapshot_dir(&cold_v4_dirty));
}
