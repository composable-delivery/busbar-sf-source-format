//! End-to-end parity test suite ported from `@salesforce/source-deploy-retrieve`
//! `test/snapshot/sampleProjects/`.
//!
//! Each test copies its fixture directory into an isolated `TempDir` (excluding `__snapshots__`)
//! so fixtures are never mutated and tests run safely in parallel.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{Cursor, Read as _};
use std::path::{Path, PathBuf};

use busbar_sf_source_format::convert::{
    ConvertOutput, MetadataConverter, TargetFormat, UniqueDirPolicy,
};
use busbar_sf_source_format::registry::RegistryAccess;
use busbar_sf_source_format::resolve::{
    ComponentSetBuilder, ComponentSetOptions, ManifestPurpose, MetadataOptions, TreeHandle,
};
use busbar_sf_source_format::tree::ZipTree;
use busbar_sf_source_format::xml::{parse_xml, semantic_eq};
use pretty_assertions::assert_eq;
use zip::ZipArchive;

fn fixture_root(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/sdr-snapshots")
        .join(name)
}

fn copy_fixture_without_snapshots(name: &str) -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let work = tmp.path().join(name);
    let src = fixture_root(name);
    copy_dir_filtered(&src, &work);
    (tmp, work)
}

fn copy_dir_filtered(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).expect("mkdir dst");
    for entry in fs::read_dir(src).expect("read_dir src") {
        let entry = entry.expect("dir entry");
        let file_name = entry.file_name();
        if file_name == "__snapshots__" {
            continue;
        }
        let src_path = entry.path();
        let dst_path = dst.join(&file_name);
        if src_path.is_dir() {
            copy_dir_filtered(&src_path, &dst_path);
        } else {
            fs::copy(&src_path, &dst_path).expect("copy fixture file");
        }
    }
}

fn collect_relative_files(root: &Path) -> BTreeSet<PathBuf> {
    let mut out = BTreeSet::new();
    if !root.exists() {
        return out;
    }
    collect_files_rec(root, root, &mut out);
    out
}

fn collect_files_rec(base: &Path, current: &Path, out: &mut BTreeSet<PathBuf>) {
    for entry in fs::read_dir(current).expect("read_dir") {
        let entry = entry.expect("entry");
        let path = entry.path();
        if path.is_dir() {
            collect_files_rec(base, &path, out);
        } else {
            let rel = path
                .strip_prefix(base)
                .expect("strip base prefix")
                .to_path_buf();
            out.insert(rel);
        }
    }
}

fn assert_dirs_match(actual_dir: &Path, expected_dir: &Path, label: &str) {
    assert_dirs_match_with_exclusions(actual_dir, expected_dir, label, &[], &[]);
}

fn assert_dirs_match_with_exclusions(
    actual_dir: &Path,
    expected_dir: &Path,
    label: &str,
    ignored_actual: &[&str],
    ignored_expected: &[&str],
) {
    let actual_ignore: BTreeSet<PathBuf> = ignored_actual.iter().map(PathBuf::from).collect();
    let expected_ignore: BTreeSet<PathBuf> = ignored_expected.iter().map(PathBuf::from).collect();
    let actual_files: BTreeSet<PathBuf> = collect_relative_files(actual_dir)
        .into_iter()
        .filter(|p| !actual_ignore.contains(p))
        .collect();
    let expected_files: BTreeSet<PathBuf> = collect_relative_files(expected_dir)
        .into_iter()
        .filter(|p| !expected_ignore.contains(p))
        .collect();
    assert_eq!(
        actual_files,
        expected_files,
        "[{label}] file sets differ between actual ({}) and expected ({})",
        actual_dir.display(),
        expected_dir.display()
    );

    for rel in &expected_files {
        let actual_path = actual_dir.join(rel);
        let expected_path = expected_dir.join(rel);
        if rel
            .file_name()
            .and_then(|s| s.to_str())
            .is_some_and(|n| n.ends_with(".resource") && n == "leafletjs.resource")
        {
            assert_zip_entries_equal(&actual_path, &expected_path, label);
            continue;
        }
        let actual_bytes = fs::read(&actual_path).expect("read actual");
        let expected_bytes = fs::read(&expected_path).expect("read expected");
        if let (Ok(actual_str), Ok(expected_str)) = (
            String::from_utf8(actual_bytes.clone()),
            String::from_utf8(expected_bytes.clone()),
        ) {
            // mocha-snap normalizes CRLF to LF when writing snapshots; normalize only `.csv` fixtures.
            let is_csv = rel.extension().and_then(|s| s.to_str()) == Some("csv");
            let actual_norm = if is_csv {
                actual_str.replace("\r\n", "\n")
            } else {
                actual_str
            };
            assert_eq!(
                actual_norm,
                expected_str,
                "[{label}] text mismatch for {}",
                rel.display()
            );
        } else {
            assert_eq!(
                actual_bytes,
                expected_bytes,
                "[{label}] binary mismatch for {}",
                rel.display()
            );
        }
    }
}

fn assert_zip_entries_equal(actual_zip_path: &Path, expected_zip_path: &Path, label: &str) {
    let actual_entries = read_zip_contents(actual_zip_path);
    let expected_entries = read_zip_contents(expected_zip_path);
    assert_eq!(
        actual_entries.keys().collect::<Vec<_>>(),
        expected_entries.keys().collect::<Vec<_>>(),
        "[{label}] zip entry list mismatch"
    );
    for (name, expected_data) in &expected_entries {
        assert_eq!(
            &actual_entries[name], expected_data,
            "[{label}] zip entry '{name}' contents differ"
        );
    }
}

fn read_zip_contents(path: &Path) -> BTreeMap<String, Vec<u8>> {
    let bytes = fs::read(path).expect("read zip file");
    let mut archive = ZipArchive::new(Cursor::new(bytes)).expect("open zip");
    let mut out = BTreeMap::new();
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).expect("zip entry");
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().expect("zip entry utf8").into_owned();
        let mut buf = Vec::new();
        entry.read_to_end(&mut buf).expect("read zip entry");
        out.insert(name, buf);
    }
    out
}

fn run_mdapi_to_source_and_back(project_name: &str) {
    run_mdapi_to_source_and_back_with_exclusions(project_name, &[], &[], &[]);
}

fn run_mdapi_to_source_and_back_with_exclusions(
    project_name: &str,
    ignored_actual_source: &[&str],
    ignored_expected_source: &[&str],
    ignored_actual_mdapi: &[&str],
) -> (tempfile::TempDir, PathBuf) {
    let (tmp, work_dir) = copy_fixture_without_snapshots(project_name);
    let work = work_dir.as_path();
    let registry = RegistryAccess::from_project_dir(work).expect("load registry");
    let converter = MetadataConverter::new(registry);

    let force_app = work.join("force-app");
    fs::create_dir_all(&force_app).expect("mkdir force-app");
    let cs_md = ComponentSetBuilder::build(&ComponentSetOptions {
        sourcepath: vec![work.join("originalMdapi")],
        project_dir: Some(work.to_path_buf()),
        ..Default::default()
    })
    .expect("build cs from originalMdapi");

    converter
        .convert(
            &cs_md,
            TargetFormat::Source,
            ConvertOutput::Directory {
                output_dir: force_app.clone(),
                package_name: None,
                unique_dir: UniqueDirPolicy::Exact,
            },
        )
        .expect("convert mdapi -> source");

    let expected_source =
        fixture_root(project_name).join("__snapshots__/verify-source-files.expected/force-app");
    assert_dirs_match_with_exclusions(
        &force_app,
        &expected_source,
        &format!("{project_name}:source"),
        ignored_actual_source,
        ignored_expected_source,
    );

    let mdapi_out = work.join("mdapiOutput");
    let cs_src = ComponentSetBuilder::build(&ComponentSetOptions {
        sourcepath: vec![force_app],
        project_dir: Some(work.to_path_buf()),
        ..Default::default()
    })
    .expect("build cs from force-app");

    converter
        .convert(
            &cs_src,
            TargetFormat::Metadata,
            ConvertOutput::Directory {
                output_dir: mdapi_out.clone(),
                package_name: None,
                unique_dir: UniqueDirPolicy::Exact,
            },
        )
        .expect("convert source -> mdapi");

    let expected_mdapi = fixture_root(project_name).join("__snapshots__/verify-md-files.expected");
    assert_dirs_match_with_exclusions(
        &mdapi_out,
        &expected_mdapi,
        &format!("{project_name}:mdapi"),
        ignored_actual_mdapi,
        &[],
    );
    (tmp, work_dir)
}

fn assert_semantic_xml_equal(actual_path: &Path, expected_path: &Path, label: &str) {
    let a = parse_xml(&fs::read_to_string(actual_path).expect("read actual xml"))
        .expect("parse actual xml");
    let b = parse_xml(&fs::read_to_string(expected_path).expect("read expected xml"))
        .expect("parse expected xml");
    assert!(
        semantic_eq(&a, &b),
        "[{label}] semantic XML mismatch between {} and {}",
        actual_path.display(),
        expected_path.display()
    );
}

#[test]
fn snapshot_ai_agent_definition_version() {
    run_mdapi_to_source_and_back("aiAgentDefinitionVersion");
}

#[test]
fn snapshot_app_templates() {
    run_mdapi_to_source_and_back("appTemplates");
}

#[test]
fn snapshot_bots() {
    run_mdapi_to_source_and_back("bots");
}

#[test]
fn snapshot_custom_labels_merged() {
    let (_tmp, work_dir) = copy_fixture_without_snapshots("customLabels-merged");
    let work = work_dir.as_path();
    let test_output = work.join("testOutput");

    let cs = ComponentSetBuilder::build(&ComponentSetOptions {
        sourcepath: vec![work.join("originalSource")],
        project_dir: Some(work.to_path_buf()),
        ..Default::default()
    })
    .expect("build cs");

    let registry = RegistryAccess::from_project_dir(work).expect("registry");
    MetadataConverter::new(registry)
        .convert(
            &cs,
            TargetFormat::Metadata,
            ConvertOutput::Directory {
                output_dir: test_output.clone(),
                package_name: None,
                unique_dir: UniqueDirPolicy::Exact,
            },
        )
        .expect("convert");

    let expected = fixture_root("customLabels-merged")
        .join("__snapshots__/verify-source-convert.expected/testOutput");
    assert_dirs_match(&test_output, &expected, "customLabels-merged");
    let labels_xml =
        fs::read_to_string(test_output.join("labels/CustomLabels.labels")).expect("read labels");
    assert_eq!(labels_xml.matches("<fullName>").count(), 8);
}

#[test]
fn snapshot_custom_labels_multiple() {
    let (_tmp, work_dir) = copy_fixture_without_snapshots("customLabels-multiple");
    let work = work_dir.as_path();
    let test_output = work.join("testOutput");

    let cs = ComponentSetBuilder::build(&ComponentSetOptions {
        sourcepath: vec![work.join("originalSource")],
        project_dir: Some(work.to_path_buf()),
        ..Default::default()
    })
    .expect("build cs");

    let registry = RegistryAccess::from_project_dir(work).expect("registry");
    MetadataConverter::new(registry)
        .convert(
            &cs,
            TargetFormat::Metadata,
            ConvertOutput::Directory {
                output_dir: test_output.clone(),
                package_name: None,
                unique_dir: UniqueDirPolicy::Exact,
            },
        )
        .expect("convert");

    let expected = fixture_root("customLabels-multiple")
        .join("__snapshots__/verify-source-convert.expected/testOutput");
    assert_dirs_match(&test_output, &expected, "customLabels-multiple");
}

#[test]
fn snapshot_custom_labels_simple() {
    run_mdapi_to_source_and_back("customLabels-simple");
}

#[test]
fn snapshot_custom_objects_and_children_round_trip() {
    run_mdapi_to_source_and_back("customObjects-and-children");
}

#[test]
fn snapshot_custom_objects_and_children_retrieve_and_deploy_empty_parent() {
    let (_tmp, work_dir) = copy_fixture_without_snapshots("customObjects-and-children");
    let work = work_dir.as_path();
    let registry = RegistryAccess::from_project_dir(work).expect("registry");
    let converter = MetadataConverter::new(registry);

    for (purpose, snap_name, out_folder) in [
        (
            ManifestPurpose::Retrieve,
            "verify-md-files-retrieve.expected",
            "retrieveOutput",
        ),
        (
            ManifestPurpose::Deploy,
            "verify-md-files-deploy.expected",
            "deployOutput",
        ),
    ] {
        let cs = ComponentSetBuilder::build(&ComponentSetOptions {
            metadata: Some(MetadataOptions {
                metadata_entries: vec!["CustomObject:Broker__c".to_owned()],
                directory_paths: vec![work.join("originalMdapi2")],
                ..Default::default()
            }),
            project_dir: Some(work.to_path_buf()),
            ..Default::default()
        })
        .expect("build mdapi cs");

        let source_dir = work.join(out_folder).join("source");
        let md_dir = work.join(out_folder).join("mdapi");

        converter
            .convert(
                &cs,
                TargetFormat::Source,
                ConvertOutput::Directory {
                    output_dir: source_dir.clone(),
                    package_name: None,
                    unique_dir: UniqueDirPolicy::Exact,
                },
            )
            .expect("convert to source");

        let mut cs2 = ComponentSetBuilder::build(&ComponentSetOptions {
            metadata: Some(MetadataOptions {
                metadata_entries: vec!["CustomObject:Broker__c".to_owned()],
                directory_paths: vec![source_dir],
                ..Default::default()
            }),
            project_dir: Some(work.to_path_buf()),
            ..Default::default()
        })
        .expect("build source cs");
        cs2.set_manifest_purpose(purpose);

        converter
            .convert(
                &cs2,
                TargetFormat::Metadata,
                ConvertOutput::Directory {
                    output_dir: md_dir.clone(),
                    package_name: None,
                    unique_dir: UniqueDirPolicy::Exact,
                },
            )
            .expect("convert to mdapi");

        let expected = fixture_root("customObjects-and-children")
            .join("__snapshots__")
            .join(snap_name)
            .join(out_folder)
            .join("mdapi");
        assert_dirs_match(&md_dir, &expected, snap_name);
    }
}

#[test]
fn snapshot_custom_translation_project() {
    run_mdapi_to_source_and_back("customTranslationProject");
}

#[test]
fn snapshot_destructive_comp_sets() {
    type DestructiveCase<'a> = (
        &'a str,
        &'a [&'a str],
        &'a [&'a str],
        &'a [&'a str],
        &'a str,
    );
    let (_tmp, work_dir) = copy_fixture_without_snapshots("destructiveCompSets");
    let work = work_dir.as_path();
    let source_dir = work.join("originalSource");
    let registry = RegistryAccess::from_project_dir(work).expect("registry");
    let converter = MetadataConverter::new(registry);

    let cases: &[DestructiveCase<'_>] = &[
        (
            "pre1",
            &["ApexClass:OneClass"],
            &["ApexClass:RedClass"],
            &[],
            "1-pre-destructive-and-1-deployment.expected",
        ),
        (
            "pre2",
            &["ApexClass:OneClass", "ApexClass:TwoClass"],
            &["ApexClass:RedClass", "ApexClass:BlueClass"],
            &[],
            "2-pre-destructive-and-2-deployments.expected",
        ),
        (
            "post1",
            &["ApexClass:OneClass"],
            &[],
            &["ApexClass:RedClass"],
            "1-post-destructive-and-1-deployment.expected",
        ),
        (
            "post2",
            &["ApexClass:OneClass", "ApexClass:TwoClass"],
            &[],
            &["ApexClass:RedClass", "ApexClass:BlueClass"],
            "2-post-destructive-and-2-deployments.expected",
        ),
        (
            "post1-solo",
            &[],
            &[],
            &["ApexClass:RedClass"],
            "1-post-destructive-and-no-deployment.expected",
        ),
    ];

    for (out_sub, entries, pre, post, snap_dir) in cases {
        let cs = ComponentSetBuilder::build(&ComponentSetOptions {
            metadata: Some(MetadataOptions {
                metadata_entries: entries.iter().map(|s| (*s).to_owned()).collect(),
                directory_paths: vec![source_dir.clone()],
                destructive_entries_pre: pre.iter().map(|s| (*s).to_owned()).collect(),
                destructive_entries_post: post.iter().map(|s| (*s).to_owned()).collect(),
                ..Default::default()
            }),
            project_dir: Some(work.to_path_buf()),
            ..Default::default()
        })
        .expect("build destructive cs");

        let out_dir = work.join("testOutput").join(out_sub);
        converter
            .convert(
                &cs,
                TargetFormat::Metadata,
                ConvertOutput::Directory {
                    output_dir: out_dir.clone(),
                    package_name: None,
                    unique_dir: UniqueDirPolicy::Exact,
                },
            )
            .expect("convert destructive cs");

        let expected = fixture_root("destructiveCompSets")
            .join("__snapshots__")
            .join(snap_dir)
            .join("testOutput")
            .join(out_sub);
        assert_dirs_match(&out_dir, &expected, out_sub);
    }

    let wildcard_err = ComponentSetBuilder::build(&ComponentSetOptions {
        metadata: Some(MetadataOptions {
            metadata_entries: vec!["ApexClass:OneClass".to_owned()],
            directory_paths: vec![source_dir],
            destructive_entries_post: vec!["ApexClass:*".to_owned()],
            ..Default::default()
        }),
        project_dir: Some(work.to_path_buf()),
        ..Default::default()
    })
    .expect_err("wildcards in destructive entries must fail");
    assert!(
        wildcard_err.to_string().contains("wildcard")
            || wildcard_err.to_string().contains("Wildcard"),
        "unexpected wildcard error message: {wildcard_err}"
    );
}

#[test]
fn snapshot_digital_experience_bundle() {
    let (_tmp, work_dir) = copy_fixture_without_snapshots("digitalExperienceBundle");
    let work = work_dir.as_path();
    let registry = RegistryAccess::from_project_dir(work).expect("registry");
    let converter = MetadataConverter::new(registry);

    let force_app = work.join("force-app");
    fs::create_dir_all(&force_app).expect("mkdir force-app");
    let cs_md = ComponentSetBuilder::build(&ComponentSetOptions {
        sourcepath: vec![work.join("originalMdapi")],
        project_dir: Some(work.to_path_buf()),
        ..Default::default()
    })
    .expect("build cs");
    converter
        .convert(
            &cs_md,
            TargetFormat::Source,
            ConvertOutput::Directory {
                output_dir: force_app.clone(),
                package_name: None,
                unique_dir: UniqueDirPolicy::Exact,
            },
        )
        .expect("convert mdapi -> source");

    let expected_src = fixture_root("digitalExperienceBundle")
        .join("__snapshots__/verify-source-files.expected/force-app");
    assert_dirs_match(&force_app, &expected_src, "deb:first");

    let merge_with = ComponentSetBuilder::build(&ComponentSetOptions {
        sourcepath: vec![force_app.clone()],
        project_dir: Some(work.to_path_buf()),
        ..Default::default()
    })
    .expect("build mergeWith")
    .source_components();

    converter
        .convert(
            &cs_md,
            TargetFormat::Source,
            ConvertOutput::Merge {
                merge_with,
                default_dir: force_app.clone(),
                project_dir: Some(work.to_path_buf()),
            },
        )
        .expect("merge second time");
    assert_dirs_match(&force_app, &expected_src, "deb:merge");

    let mdapi_out = work.join("mdapiOutput");
    let cs_src = ComponentSetBuilder::build(&ComponentSetOptions {
        sourcepath: vec![force_app],
        project_dir: Some(work.to_path_buf()),
        ..Default::default()
    })
    .expect("build src cs");
    converter
        .convert(
            &cs_src,
            TargetFormat::Metadata,
            ConvertOutput::Directory {
                output_dir: mdapi_out.clone(),
                package_name: None,
                unique_dir: UniqueDirPolicy::Exact,
            },
        )
        .expect("convert source -> mdapi");
    let expected_md =
        fixture_root("digitalExperienceBundle").join("__snapshots__/verify-md-files.expected");
    assert_dirs_match(&mdapi_out, &expected_md, "deb:mdapi");
}

#[test]
fn snapshot_digital_experience_mobile_pages() {
    let (_tmp, work_dir) = copy_fixture_without_snapshots("digitalExperienceMobilePages");
    let work = work_dir.as_path();
    let pkg_dir = work.join("force-app");
    let augmented_dir = work.join("force-app-destination");
    let mdapi_out = work.join("mdapiOutput");

    let cs = ComponentSetBuilder::build(&ComponentSetOptions {
        sourcepath: vec![pkg_dir.clone()],
        api_version: Some("60.0".to_owned()),
        ..Default::default()
    })
    .expect("build cs");
    let converter = MetadataConverter::default();
    converter
        .convert(
            &cs,
            TargetFormat::Metadata,
            ConvertOutput::Directory {
                output_dir: mdapi_out.clone(),
                package_name: None,
                unique_dir: UniqueDirPolicy::Exact,
            },
        )
        .expect("convert to mdapi");

    let expected_pre_merge = fixture_root("digitalExperienceMobilePages")
        .join("__snapshots__/verify-files-pre-merge.expected");
    assert_dirs_match(&mdapi_out, &expected_pre_merge, "deb-mobile:pre-merge");

    let cs_md = ComponentSetBuilder::build(&ComponentSetOptions {
        sourcepath: vec![mdapi_out],
        ..Default::default()
    })
    .expect("build mdapi cs");
    let merge_with = ComponentSetBuilder::build(&ComponentSetOptions {
        sourcepath: vec![augmented_dir.clone()],
        ..Default::default()
    })
    .expect("build dest cs")
    .source_components();

    converter
        .convert(
            &cs_md,
            TargetFormat::Source,
            ConvertOutput::Merge {
                merge_with,
                default_dir: pkg_dir.clone(),
                project_dir: Some(work.to_path_buf()),
            },
        )
        .expect("merge into destination");
    assert_dirs_match(&pkg_dir, &augmented_dir, "deb-mobile:post-merge");
}

#[test]
fn snapshot_forceignore() {
    let (_tmp, work_dir) = copy_fixture_without_snapshots("forceignore");
    let work = work_dir.as_path();
    let cs = ComponentSetBuilder::build(&ComponentSetOptions {
        metadata: Some(MetadataOptions {
            metadata_entries: vec!["ApexClass:OneClass".to_owned(), "Profile:Admin".to_owned()],
            directory_paths: vec![work.join("originalMdapi")],
            ..Default::default()
        }),
        project_dir: Some(work.to_path_buf()),
        ..Default::default()
    })
    .expect("build cs");

    let source_output = work.join("testOutput/source-format");
    let registry = RegistryAccess::from_project_dir(work).expect("registry");
    MetadataConverter::new(registry)
        .convert(
            &cs,
            TargetFormat::Source,
            ConvertOutput::Directory {
                output_dir: source_output.clone(),
                package_name: None,
                unique_dir: UniqueDirPolicy::Exact,
            },
        )
        .expect("convert");

    let expected = fixture_root("forceignore")
        .join("__snapshots__/ignores-source-format-path.expected/testOutput/source-format");
    assert_dirs_match(&source_output, &expected, "forceignore");
}

#[test]
fn snapshot_gen_ai_functions() {
    run_mdapi_to_source_and_back("genAiFunctions");
}

#[test]
fn snapshot_legacy_suffix_support() {
    let (_tmp, work_dir) = copy_fixture_without_snapshots("legacySuffixSupport");
    let work = work_dir.as_path();
    let mdapi_out = work.join("mdapiOutput");

    let cs = ComponentSetBuilder::build(&ComponentSetOptions {
        sourcepath: vec![work.join("force-app")],
        project_dir: Some(work.to_path_buf()),
        ..Default::default()
    })
    .expect("build cs");

    let registry = RegistryAccess::from_project_dir(work).expect("registry");
    MetadataConverter::new(registry)
        .convert(
            &cs,
            TargetFormat::Metadata,
            ConvertOutput::Directory {
                output_dir: mdapi_out.clone(),
                package_name: None,
                unique_dir: UniqueDirPolicy::Exact,
            },
        )
        .expect("convert");

    let expected =
        fixture_root("legacySuffixSupport").join("__snapshots__/verify-md-files.expected");
    assert_dirs_match(&mdapi_out, &expected, "legacySuffixSupport");
}

#[test]
fn snapshot_mpd_with_labels() {
    let (_tmp, work_dir) = copy_fixture_without_snapshots("mpdWithLabels");
    let work = work_dir.as_path();
    let pkg_dirs = [
        work.join("force-app"),
        work.join("my-app"),
        work.join("foo-bar/app"),
    ];
    let cs = ComponentSetBuilder::build(&ComponentSetOptions {
        sourcepath: pkg_dirs.to_vec(),
        api_version: Some("60.0".to_owned()),
        ..Default::default()
    })
    .expect("build cs");

    let converter = MetadataConverter::default();
    let mdapi_out = work.join("mdapiOutput");
    converter
        .convert(
            &cs,
            TargetFormat::Metadata,
            ConvertOutput::Directory {
                output_dir: mdapi_out.clone(),
                package_name: None,
                unique_dir: UniqueDirPolicy::Exact,
            },
        )
        .expect("recompose mpd");

    let expected_md = fixture_root("mpdWithLabels")
        .join("__snapshots__/verify-md-files-after-recomposing.expected");
    assert_dirs_match(&mdapi_out, &expected_md, "mpdWithLabels:mdapi");

    let cs_md = ComponentSetBuilder::build(&ComponentSetOptions {
        sourcepath: vec![mdapi_out],
        ..Default::default()
    })
    .expect("build mdapi cs");
    let merge_with = ComponentSetBuilder::build(&ComponentSetOptions {
        sourcepath: pkg_dirs.to_vec(),
        ..Default::default()
    })
    .expect("build mergeWith")
    .source_components();

    converter
        .convert(
            &cs_md,
            TargetFormat::Source,
            ConvertOutput::Merge {
                merge_with,
                default_dir: pkg_dirs[0].clone(),
                project_dir: Some(work.to_path_buf()),
            },
        )
        .expect("merge back");

    // Upstream `@salesforce/core` (`pkg.fullPath`) includes a trailing separator while
    // `mpdWithLabels/snapshots.test.ts` passed `path.resolve(...)` without one, causing
    // `pkgPaths.includes(defaultDirectory)` to miss and copy `my_app_Label_1` into `force-app`.
    // In Rust, package ownership stays idempotent: verify `force-app/main/default/labels/CustomLabels.labels-meta.xml`
    // equals the original fixture and all other files match their snapshots.
    let label_rel = "main/default/labels/CustomLabels.labels-meta.xml";
    let expected_force =
        fixture_root("mpdWithLabels").join("__snapshots__/verify-force-app.expected/force-app");
    assert_dirs_match_with_exclusions(
        &work.join("force-app"),
        &expected_force,
        "force-app",
        &[label_rel],
        &[label_rel],
    );
    assert_eq!(
        fs::read_to_string(work.join("force-app").join(label_rel))
            .expect("read actual force labels"),
        fs::read_to_string(
            fixture_root("mpdWithLabels")
                .join("force-app")
                .join(label_rel)
        )
        .expect("read original force labels"),
        "mpdWithLabels force-app labels should remain idempotent"
    );

    for (rel, expected_snap) in [
        ("my-app", "verify-my-app.expected"),
        ("foo-bar/app", "verify-foo-bar-app.expected"),
    ] {
        let actual = work.join(rel);
        let expected = fixture_root("mpdWithLabels")
            .join("__snapshots__")
            .join(expected_snap)
            .join(rel);
        assert_dirs_match(&actual, &expected, rel);
    }
}

#[test]
fn snapshot_nested_folders() {
    run_mdapi_to_source_and_back("nestedFolders");
}

#[test]
fn snapshot_preset_perm_set() {
    // Upstream `preset-PermSet/snapshots.test.ts` omitted `await` on `dirsAreIdentical(...)`,
    // leaving an orphaned un-decomposed `dreamhouse.permissionset-meta.xml` artifact in
    // `__snapshots__/verify-source-files.expected`.
    run_mdapi_to_source_and_back_with_exclusions(
        "preset-PermSet",
        &[],
        &["main/default/permissionsets/dreamhouse.permissionset-meta.xml"],
        &[],
    );
    let orig = fixture_root("preset-PermSet")
        .join("originalMdapi/permissionsets/dreamhouse.permissionset");
    let expected_md = fixture_root("preset-PermSet")
        .join("__snapshots__/verify-md-files.expected/permissionsets/dreamhouse.permissionset");
    assert_semantic_xml_equal(&orig, &expected_md, "preset-PermSet");
}

#[test]
fn snapshot_preset_decompose_labels() {
    run_mdapi_to_source_and_back("preset-decomposeLabels");
}

#[test]
fn snapshot_preset_decomposed_esr() {
    run_mdapi_to_source_and_back("preset-decomposedESR");
}

#[test]
fn snapshot_preset_decomposed_ps2() {
    run_mdapi_to_source_and_back("preset-decomposedPS2");
}

#[test]
fn snapshot_preset_sharing_rules() {
    // Upstream `preset-sharingRules/snapshots.test.ts` omitted `await` on `dirsAreIdentical(...)`,
    // leaving an orphaned `account.sharingRules-meta.xml` artifact in
    // `__snapshots__/verify-source-files.expected`.
    run_mdapi_to_source_and_back_with_exclusions(
        "preset-sharingRules",
        &[],
        &["main/default/sharingRules/account.sharingRules-meta.xml"],
        &[],
    );
}

#[test]
fn snapshot_preset_workflow() {
    run_mdapi_to_source_and_back("preset-workflow");
}

#[test]
fn snapshot_preset_workflow_mpd() {
    let (_tmp, work_dir) = copy_fixture_without_snapshots("preset-workflow-mpd");
    let work = work_dir.as_path();
    let cs = ComponentSetBuilder::build(&ComponentSetOptions {
        sourcepath: vec![work.join("force-app"), work.join("another-package")],
        project_dir: Some(work.to_path_buf()),
        ..Default::default()
    })
    .expect("build cs");

    let registry = RegistryAccess::from_project_dir(work).expect("registry");
    let mdapi_out = work.join("mdapiOutput");
    MetadataConverter::new(registry)
        .convert(
            &cs,
            TargetFormat::Metadata,
            ConvertOutput::Directory {
                output_dir: mdapi_out.clone(),
                package_name: None,
                unique_dir: UniqueDirPolicy::Exact,
            },
        )
        .expect("convert");

    let expected =
        fixture_root("preset-workflow-mpd").join("__snapshots__/verify-md-files.expected");
    assert_dirs_match(&mdapi_out, &expected, "preset-workflow-mpd");
}

#[test]
fn snapshot_sharing_rules_entire_project_and_filtered() {
    run_mdapi_to_source_and_back("sharingRules");

    let (_tmp, work_dir) = copy_fixture_without_snapshots("sharingRules");
    let work = work_dir.as_path();
    let registry = RegistryAccess::from_project_dir(work).expect("registry");
    let converter = MetadataConverter::new(registry);
    let snaps = fixture_root("sharingRules").join("__snapshots__");

    for (entry, tag) in [
        ("SharingCriteriaRule:account.AcctCBS_toAllInternal", "SCR"),
        ("SharingRules:account", "SR"),
    ] {
        let src_out = work.join(format!("out-src-{tag}"));
        let cs_md = ComponentSetBuilder::build(&ComponentSetOptions {
            metadata: Some(MetadataOptions {
                metadata_entries: vec![entry.to_owned()],
                directory_paths: vec![snaps.join("verify-md-files.expected")],
                ..Default::default()
            }),
            project_dir: Some(work.to_path_buf()),
            ..Default::default()
        })
        .expect("build cs md");
        converter
            .convert(
                &cs_md,
                TargetFormat::Source,
                ConvertOutput::Directory {
                    output_dir: src_out.clone(),
                    package_name: None,
                    unique_dir: UniqueDirPolicy::Exact,
                },
            )
            .expect("convert to source");
        assert_dirs_match(
            &src_out,
            &snaps.join(format!("verify-source-files-{tag}.expected/force-app")),
            &format!("sharingRules:{tag}:source"),
        );

        let md_out = work.join(format!("out-md-{tag}"));
        let cs_src = ComponentSetBuilder::build(&ComponentSetOptions {
            metadata: Some(MetadataOptions {
                metadata_entries: vec![entry.to_owned()],
                directory_paths: vec![snaps.join("verify-source-files.expected/force-app")],
                ..Default::default()
            }),
            project_dir: Some(work.to_path_buf()),
            ..Default::default()
        })
        .expect("build cs src");
        converter
            .convert(
                &cs_src,
                TargetFormat::Metadata,
                ConvertOutput::Directory {
                    output_dir: md_out.clone(),
                    package_name: None,
                    unique_dir: UniqueDirPolicy::Exact,
                },
            )
            .expect("convert to mdapi");
        assert_dirs_match(
            &md_out,
            &snaps.join(format!("verify-md-files-{tag}.expected")),
            &format!("sharingRules:{tag}:mdapi"),
        );
    }
}

#[test]
fn snapshot_single_custom_field_retrieve_non_overwrite_and_zip_merge() {
    let (_tmp, work_dir) = copy_fixture_without_snapshots("singleCustomFieldRetrieve");
    let work = work_dir.as_path();
    let force_app = work.join("force-app");
    let converter = MetadataConverter::default();

    // Step 1: merge Email__c from mdapiOutput onto force-app
    let cs = ComponentSetBuilder::build(&ComponentSetOptions {
        sourcepath: vec![work.join("mdapiOutput")],
        ..Default::default()
    })
    .expect("build mdapiOutput cs");
    let merge_with = ComponentSetBuilder::build(&ComponentSetOptions {
        sourcepath: vec![force_app.clone()],
        ..Default::default()
    })
    .expect("build force-app mergeWith")
    .source_components();

    converter
        .convert(
            &cs,
            TargetFormat::Source,
            ConvertOutput::Merge {
                merge_with,
                default_dir: force_app.clone(),
                project_dir: Some(work.to_path_buf()),
            },
        )
        .expect("merge Email__c");

    // Step 2: extract and merge testing__c from base64 retrieve zip via ZipTree
    let ts_test =
        fs::read_to_string(work.join("snapshots.test.ts")).expect("read snapshots.test.ts");
    let zips = extract_base64_zips(&ts_test);
    assert_eq!(
        zips.len(),
        2,
        "expected 2 base64 zip payloads in snapshots.test.ts"
    );

    let zip_tree_1 = TreeHandle::zip(ZipTree::from_bytes(&zips[0]).expect("zip 1"));
    let cs_zip_1 = ComponentSetBuilder::build_with_tree(
        &ComponentSetOptions {
            sourcepath: vec![PathBuf::from("unpackaged")],
            ..Default::default()
        },
        &zip_tree_1,
    )
    .expect("build cs from zip 1");
    let merge_with_1 = ComponentSetBuilder::build(&ComponentSetOptions {
        sourcepath: vec![force_app.clone()],
        ..Default::default()
    })
    .expect("build mergeWith 1")
    .source_components();

    converter
        .convert(
            &cs_zip_1,
            TargetFormat::Source,
            ConvertOutput::Merge {
                merge_with: merge_with_1,
                default_dir: force_app.clone(),
                project_dir: Some(work.to_path_buf()),
            },
        )
        .expect("merge zip 1");

    let expected_force_app = fixture_root("singleCustomFieldRetrieve")
        .join("__snapshots__/verify-force-app.expected/force-app");
    assert_dirs_match(
        &force_app,
        &expected_force_app,
        "singleCustomFieldRetrieve:force-app",
    );

    // Step 3: package retrieval into TestPackage
    let test_pkg = work.join("TestPackage");
    let zip_tree_2 = TreeHandle::zip(ZipTree::from_bytes(&zips[1]).expect("zip 2"));
    let cs_zip_2 = ComponentSetBuilder::build_with_tree(
        &ComponentSetOptions {
            sourcepath: vec![PathBuf::from("TestPackage")],
            ..Default::default()
        },
        &zip_tree_2,
    )
    .expect("build cs from zip 2");
    converter
        .convert(
            &cs_zip_2,
            TargetFormat::Source,
            ConvertOutput::Directory {
                output_dir: test_pkg.clone(),
                package_name: None,
                unique_dir: UniqueDirPolicy::Exact,
            },
        )
        .expect("convert zip 2");

    let expected_pkg = fixture_root("singleCustomFieldRetrieve")
        .join("__snapshots__/will-not-overwrite-object-meta-xml-in-package-retrieval.expected/TestPackage");
    assert_dirs_match(
        &test_pkg,
        &expected_pkg,
        "singleCustomFieldRetrieve:TestPackage",
    );
}

#[test]
fn snapshot_static_resource() {
    // Upstream `conversions.ts` `shouldIgnore` skips any path containing `leafletjs.resource`
    // (which also matched `leafletjs.resource-meta.xml`), `staticResource/snapshots.test.ts`
    // omitted `await` on `dirsAreIdentical(...)`, and `fileSnap` read `leafletjs/` entries as
    // UTF-8 with CRLF->LF normalization (corrupting PNGs and rewriting CRLF in CSS/JS). Verify
    // every extracted `leafletjs/` asset and `.resource-meta.xml` strictly against `originalMdapi`.
    let leaflet_assets = [
        "main/default/staticresources/leafletjs/images/layers-2x.png",
        "main/default/staticresources/leafletjs/images/layers.png",
        "main/default/staticresources/leafletjs/images/marker-icon-2x.png",
        "main/default/staticresources/leafletjs/images/marker-icon.png",
        "main/default/staticresources/leafletjs/images/marker-shadow.png",
        "main/default/staticresources/leafletjs/leaflet.css",
        "main/default/staticresources/leafletjs/leaflet.js",
    ];
    let mut ignored_actual_source =
        vec!["main/default/staticresources/leafletjs.resource-meta.xml"];
    ignored_actual_source.extend_from_slice(&leaflet_assets);
    let (_tmp, work) = run_mdapi_to_source_and_back_with_exclusions(
        "staticResource",
        &ignored_actual_source,
        &leaflet_assets,
        &[
            "staticresources/leafletjs.resource",
            "staticresources/leafletjs.resource-meta.xml",
        ],
    );
    let orig_meta = fixture_root("staticResource")
        .join("originalMdapi/staticresources/leafletjs.resource-meta.xml");
    let orig_zip =
        fixture_root("staticResource").join("originalMdapi/staticresources/leafletjs.resource");
    let zip_entries = read_zip_contents(&orig_zip);
    let extracted_root = work.join("force-app/main/default/staticresources/leafletjs");
    assert_eq!(
        collect_relative_files(&extracted_root),
        zip_entries
            .keys()
            .map(PathBuf::from)
            .collect::<BTreeSet<_>>(),
        "extracted leafletjs file set mismatch"
    );
    for (rel_entry, expected_bytes) in &zip_entries {
        let actual_bytes = fs::read(extracted_root.join(rel_entry)).expect("read extracted asset");
        assert_eq!(
            actual_bytes, *expected_bytes,
            "extracted leafletjs entry '{rel_entry}' mismatch"
        );
    }
    let expected_meta = fs::read_to_string(&orig_meta).expect("read original leafletjs meta");
    assert_eq!(
        fs::read_to_string(
            work.join("force-app/main/default/staticresources/leafletjs.resource-meta.xml")
        )
        .expect("read source leafletjs meta"),
        expected_meta,
        "leafletjs source meta mismatch"
    );
    assert_eq!(
        fs::read_to_string(work.join("mdapiOutput/staticresources/leafletjs.resource-meta.xml"))
            .expect("read mdapi leafletjs meta"),
        expected_meta,
        "leafletjs mdapi meta mismatch"
    );
    assert_zip_entries_equal(
        &work.join("mdapiOutput/staticresources/leafletjs.resource"),
        &orig_zip,
        "staticResource:leafletjs-zip",
    );
}

#[test]
fn snapshot_territory() {
    run_mdapi_to_source_and_back("territory");
}

#[test]
fn snapshot_ui_bundle() {
    run_mdapi_to_source_and_back("UIBundleSnapshot");
}

#[test]
fn snapshot_variant_raw_perm_set() {
    run_mdapi_to_source_and_back("variant-raw-PermSet");
}

#[test]
fn snapshot_variant_workflow() {
    run_mdapi_to_source_and_back("variant-workflow");
}

#[test]
fn snapshot_wave_template_bundle() {
    run_mdapi_to_source_and_back("waveTemplateBundle");
}

#[test]
fn snapshot_workflow() {
    run_mdapi_to_source_and_back("workflow");
}

#[test]
fn snapshot_xml_comments() {
    run_mdapi_to_source_and_back("xmlComments");
}

fn extract_base64_zips(ts_source: &str) -> Vec<Vec<u8>> {
    let mut zips = Vec::new();
    for line in ts_source.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix('\'')
            && let Some(b64) = rest.strip_suffix("',")
            && b64.starts_with("UEsDB")
        {
            zips.push(decode_base64(b64));
        }
    }
    zips
}

fn decode_base64(input: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(input.len() * 3 / 4);
    let mut buf = 0u32;
    let mut bits = 0u8;
    for byte in input.bytes() {
        let val = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' | b'\n' | b'\r' | b' ' => continue,
            other => panic!("unexpected base64 byte: {other}"),
        };
        buf = (buf << 6) | u32::from(val);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push(u8::try_from((buf >> bits) & 0xFF).expect("masked to u8"));
        }
    }
    out
}
