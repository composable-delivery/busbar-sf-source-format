//! Standalone release-mode in-process benchmark runner invoked by
//! `benchmarks/run-comparison.py` to produce apples-to-apples JSON timings alongside
//! Node `@salesforce/source-deploy-retrieve`.

use std::env;
use std::fs;
use std::io::{self, Cursor};
use std::path::{Path, PathBuf};
use std::time::Instant;

use busbar_sf_source_format::convert::{
    ConvertOutput, MetadataConverter, TargetFormat, UniqueDirPolicy,
};
use busbar_sf_source_format::registry::RegistryAccess;
use busbar_sf_source_format::resolve::{ComponentSetBuilder, ComponentSetOptions};
use serde::{Deserialize, Serialize};
use zip::ZipArchive;

#[derive(Debug, Deserialize)]
struct BenchManifest {
    workloads: Vec<WorkloadSpec>,
}

#[derive(Debug, Deserialize)]
struct WorkloadSpec {
    name: String,
    direction: Direction,
    mode: OutputMode,
    input_dir: PathBuf,
    project_dir: Option<PathBuf>,
    output_base_dir: PathBuf,
    warmup: usize,
    iterations: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Direction {
    MdapiToSource,
    SourceToMdapi,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
enum OutputMode {
    Directory,
    Zip,
}

#[derive(Debug, Serialize)]
struct BenchReport {
    engine: &'static str,
    results: Vec<WorkloadResult>,
}

#[derive(Debug, Serialize)]
struct WorkloadResult {
    name: String,
    durations_ms: Vec<f64>,
    output_count: usize,
}

fn main() {
    let manifest_path = env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .expect("usage: bench_harness <manifest.json>");
    let raw = fs::read_to_string(&manifest_path).expect("read benchmark manifest json");
    let manifest: BenchManifest = serde_json::from_str(&raw).expect("parse benchmark manifest");

    let mut results = Vec::with_capacity(manifest.workloads.len());
    for workload in &manifest.workloads {
        results.push(run_workload(workload));
    }

    let report = BenchReport {
        engine: "rust",
        results,
    };
    let stdout = io::stdout().lock();
    serde_json::to_writer_pretty(stdout, &report).expect("serialize benchmark report");
}

fn run_workload(spec: &WorkloadSpec) -> WorkloadResult {
    for i in 0..spec.warmup {
        let (_ms, _count) = run_single_iteration(spec, &format!("warmup_{i}"));
    }

    let mut durations_ms = Vec::with_capacity(spec.iterations);
    let mut output_count = 0usize;
    for i in 0..spec.iterations {
        let (elapsed_ms, count) = run_single_iteration(spec, &format!("iter_{i}"));
        durations_ms.push(elapsed_ms);
        output_count = count;
    }

    WorkloadResult {
        name: spec.name.clone(),
        durations_ms,
        output_count,
    }
}

fn run_single_iteration(spec: &WorkloadSpec, tag: &str) -> (f64, usize) {
    let target_format = match spec.direction {
        Direction::MdapiToSource => TargetFormat::Source,
        Direction::SourceToMdapi => TargetFormat::Metadata,
    };

    let out_dir = spec.output_base_dir.join(tag);
    if spec.mode == OutputMode::Directory {
        if out_dir.exists() {
            fs::remove_dir_all(&out_dir).expect("clean stale output dir");
        }
        fs::create_dir_all(&out_dir).expect("create output dir");
    }

    let output_cfg = match spec.mode {
        OutputMode::Directory => ConvertOutput::Directory {
            output_dir: out_dir.clone(),
            package_name: None,
            unique_dir: UniqueDirPolicy::Exact,
        },
        OutputMode::Zip => ConvertOutput::Zip {
            output_dir: None,
            package_name: None,
            unique_dir: UniqueDirPolicy::Exact,
        },
    };

    let started = Instant::now();
    let registry = match &spec.project_dir {
        Some(dir) => RegistryAccess::from_project_dir(dir).expect("load project registry"),
        None => RegistryAccess::new(),
    };
    let cs = ComponentSetBuilder::build(&ComponentSetOptions {
        sourcepath: vec![spec.input_dir.clone()],
        project_dir: spec.project_dir.clone(),
        ..Default::default()
    })
    .expect("build component set");
    let convert_res = MetadataConverter::new(registry)
        .convert(&cs, target_format, output_cfg)
        .expect("convert metadata");
    let elapsed_ms = started.elapsed().as_secs_f64() * 1000.0;

    let output_count = match spec.mode {
        OutputMode::Directory => {
            let count = count_files_recursive(&out_dir);
            fs::remove_dir_all(&out_dir).expect("cleanup output dir");
            count
        }
        OutputMode::Zip => {
            let bytes = convert_res.zip_bytes.as_deref().expect("zip bytes");
            count_zip_file_entries(bytes)
        }
    };

    (elapsed_ms, output_count)
}

fn count_files_recursive(dir: &Path) -> usize {
    let mut total = 0usize;
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    for entry in entries {
        let entry = entry.expect("dir entry");
        let path = entry.path();
        if path.is_dir() {
            total += count_files_recursive(&path);
        } else {
            total += 1;
        }
    }
    total
}

fn count_zip_file_entries(bytes: &[u8]) -> usize {
    let mut archive = ZipArchive::new(Cursor::new(bytes)).expect("open output zip");
    let mut count = 0usize;
    for i in 0..archive.len() {
        let entry = archive.by_index(i).expect("zip entry");
        if !entry.is_dir() {
            count += 1;
        }
    }
    count
}
