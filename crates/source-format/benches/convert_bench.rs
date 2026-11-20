//! Criterion benchmarks for `sf-source-format` resolution, conversion, and registry loading.

use std::fs;
use std::path::{Path, PathBuf};

use busbar_sf_source_format::convert::{
    ConvertOutput, ConvertResult, MetadataConverter, TargetFormat, UniqueDirPolicy,
};
use busbar_sf_source_format::registry::RegistryAccess;
use busbar_sf_source_format::resolve::{ComponentSetBuilder, ComponentSetOptions};
use criterion::{BatchSize, Criterion};
use std::hint::black_box;

const FIXTURES: &[&str] = &[
    "customObjects-and-children",
    "digitalExperienceBundle",
    "staticResource",
    "preset-decomposedPS2",
    "preset-decomposedESR",
];

fn fixture_root(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/sdr-snapshots")
        .join(name)
}

fn copy_dir_filtered(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).expect("mkdir dst");
    for entry in fs::read_dir(src).expect("read_dir src") {
        let entry = entry.expect("dir entry");
        let name = entry.file_name();
        if name == "__snapshots__" {
            continue;
        }
        let sp = entry.path();
        let dp = dst.join(name);
        if sp.is_dir() {
            copy_dir_filtered(&sp, &dp);
        } else {
            fs::copy(&sp, &dp).expect("copy fixture file");
        }
    }
}

fn prepare_fixture(name: &str) -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().expect("create tempdir");
    let work = tmp.path().join(name);
    copy_dir_filtered(&fixture_root(name), &work);
    (tmp, work)
}

fn prepare_source_fixture(name: &str) -> (tempfile::TempDir, PathBuf, PathBuf) {
    let (tmp, work) = prepare_fixture(name);
    let force_app = work.join("force-app");
    fs::create_dir_all(&force_app).expect("mkdir force-app");
    let registry = RegistryAccess::from_project_dir(&work).expect("load registry");
    let cs = ComponentSetBuilder::build(&ComponentSetOptions {
        sourcepath: vec![work.join("originalMdapi")],
        project_dir: Some(work.clone()),
        ..Default::default()
    })
    .expect("build mdapi cs");
    MetadataConverter::new(registry)
        .convert(
            &cs,
            TargetFormat::Source,
            ConvertOutput::Directory {
                output_dir: force_app.clone(),
                package_name: None,
                unique_dir: UniqueDirPolicy::Exact,
            },
        )
        .expect("prepare sfdx source fixture");
    (tmp, work, force_app)
}

fn bench_mdapi_to_source(c: &mut Criterion) {
    let mut group = c.benchmark_group("mdapi_to_source");
    for &name in FIXTURES {
        let sample_count = if name == "digitalExperienceBundle" {
            10
        } else {
            20
        };
        group.sample_size(sample_count);
        let (_tmp, work) = prepare_fixture(name);
        let mdapi_dir = work.join("originalMdapi");
        group.bench_function(name, |b| {
            b.iter_batched(
                || tempfile::tempdir().expect("out tempdir"),
                |out_tmp| {
                    let registry = RegistryAccess::from_project_dir(&work).expect("registry");
                    let cs = ComponentSetBuilder::build(&ComponentSetOptions {
                        sourcepath: vec![mdapi_dir.clone()],
                        project_dir: Some(work.clone()),
                        ..Default::default()
                    })
                    .expect("build cs");
                    let res = MetadataConverter::new(registry)
                        .convert(
                            &cs,
                            TargetFormat::Source,
                            ConvertOutput::Directory {
                                output_dir: out_tmp.path().to_path_buf(),
                                package_name: None,
                                unique_dir: UniqueDirPolicy::Exact,
                            },
                        )
                        .expect("convert mdapi -> source");
                    black_box(res);
                    out_tmp
                },
                BatchSize::PerIteration,
            );
        });
    }
    group.finish();
}

fn bench_source_to_mdapi_dir(c: &mut Criterion) {
    let mut group = c.benchmark_group("source_to_mdapi_dir");
    for &name in FIXTURES {
        let sample_count = if name == "digitalExperienceBundle" {
            10
        } else {
            20
        };
        group.sample_size(sample_count);
        let (_tmp, work, force_app) = prepare_source_fixture(name);
        group.bench_function(name, |b| {
            b.iter_batched(
                || tempfile::tempdir().expect("out tempdir"),
                |out_tmp| {
                    let registry = RegistryAccess::from_project_dir(&work).expect("registry");
                    let cs = ComponentSetBuilder::build(&ComponentSetOptions {
                        sourcepath: vec![force_app.clone()],
                        project_dir: Some(work.clone()),
                        ..Default::default()
                    })
                    .expect("build cs");
                    let res = MetadataConverter::new(registry)
                        .convert(
                            &cs,
                            TargetFormat::Metadata,
                            ConvertOutput::Directory {
                                output_dir: out_tmp.path().to_path_buf(),
                                package_name: None,
                                unique_dir: UniqueDirPolicy::Exact,
                            },
                        )
                        .expect("convert source -> mdapi");
                    black_box(res);
                    out_tmp
                },
                BatchSize::PerIteration,
            );
        });
    }
    group.finish();
}

fn bench_source_to_mdapi_zip(c: &mut Criterion) {
    let mut group = c.benchmark_group("source_to_mdapi_zip");
    for &name in &[
        "customObjects-and-children",
        "digitalExperienceBundle",
        "staticResource",
    ] {
        let sample_count = if name == "digitalExperienceBundle" {
            10
        } else {
            20
        };
        group.sample_size(sample_count);
        let (_tmp, work, force_app) = prepare_source_fixture(name);
        group.bench_function(name, |b| {
            b.iter(|| -> ConvertResult {
                let registry = RegistryAccess::from_project_dir(&work).expect("registry");
                let cs = ComponentSetBuilder::build(&ComponentSetOptions {
                    sourcepath: vec![force_app.clone()],
                    project_dir: Some(work.clone()),
                    ..Default::default()
                })
                .expect("build cs");
                let res = MetadataConverter::new(registry)
                    .convert(
                        &cs,
                        TargetFormat::Metadata,
                        ConvertOutput::Zip {
                            output_dir: None,
                            package_name: None,
                            unique_dir: UniqueDirPolicy::Exact,
                        },
                    )
                    .expect("convert source -> zip");
                black_box(res)
            });
        });
    }
    group.finish();
}

fn bench_registry(c: &mut Criterion) {
    let mut group = c.benchmark_group("registry_new");
    let (_tmp_std, work_std) = prepare_fixture("customObjects-and-children");
    let (_tmp_preset, work_preset) = prepare_fixture("preset-decomposedPS2");

    group.bench_function("default_embedded", |b| {
        b.iter(|| black_box(RegistryAccess::new()));
    });
    group.bench_function("from_project_dir_default", |b| {
        b.iter(|| {
            black_box(
                RegistryAccess::from_project_dir(&work_std).expect("load project dir registry"),
            )
        });
    });
    group.bench_function("from_project_dir_preset", |b| {
        b.iter(|| {
            black_box(RegistryAccess::from_project_dir(&work_preset).expect("load preset registry"))
        });
    });
    group.finish();
}

fn main() {
    let mut c = Criterion::default().configure_from_args();
    bench_mdapi_to_source(&mut c);
    bench_source_to_mdapi_dir(&mut c);
    bench_source_to_mdapi_zip(&mut c);
    bench_registry(&mut c);
    c.final_summary();
}
