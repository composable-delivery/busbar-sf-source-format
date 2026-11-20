//! CLI binary for converting Salesforce metadata between MDAPI and SFDX source formats.

use std::io::{self, Write as _};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use busbar_sf_source_format::convert::{
    CacheVerifyMode, ConvertCache, ConvertOutput, MetadataConverter, TargetFormat, UniqueDirPolicy,
};
use busbar_sf_source_format::registry::RegistryAccess;
use busbar_sf_source_format::resolve::{
    ComponentSetBuilder, ComponentSetOptions, ManifestOptions, MetadataOptions,
};
use clap::{Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "sf-source-format",
    version,
    about = "Convert Salesforce metadata between MDAPI and SFDX source formats"
)]
struct Cli {
    #[command(subcommand)]
    command: TopCommand,
}

#[derive(Debug, Subcommand)]
enum TopCommand {
    /// Convert metadata between SFDX source format and Metadata API format.
    Convert {
        #[command(subcommand)]
        target: ConvertSubcommand,
    },
}

#[derive(Debug, Subcommand)]
enum ConvertSubcommand {
    /// Convert SFDX source files into Metadata API format (`package.xml` + metadata files or zip).
    Mdapi(ConvertCommonArgs),
    /// Convert Metadata API files into SFDX source format.
    Source {
        #[command(flatten)]
        common: ConvertCommonArgs,
        /// Existing SFDX source paths to merge converted metadata into.
        #[arg(long = "merge-with", value_name = "PATH", num_args = 1..)]
        merge_with: Vec<PathBuf>,
    },
}

#[derive(Debug, Args)]
struct ConvertCommonArgs {
    /// Source or MDAPI directories/files to convert (or search paths when using --manifest/--metadata).
    #[arg(long, short = 'p', value_name = "PATH", num_args = 1..)]
    sourcepath: Vec<PathBuf>,

    /// Path to a `package.xml` manifest selecting components to convert.
    #[arg(long, short = 'x', value_name = "PATH")]
    manifest: Option<PathBuf>,

    /// Explicit metadata components in `Type:FullName` format.
    #[arg(long, short = 'm', value_name = "TYPE:NAME", num_args = 1..)]
    metadata: Vec<String>,

    /// Target output directory for converted files or zip archive.
    #[arg(long = "output-dir", short = 'd', value_name = "PATH")]
    output_dir: Option<PathBuf>,

    /// Write output as a `.zip` archive instead of loose files.
    #[arg(long)]
    zip: bool,

    /// Package directory/archive name (and `<fullName>` in generated `package.xml`).
    #[arg(long = "package-name", value_name = "NAME")]
    package_name: Option<String>,

    /// Override Metadata API version (e.g., `63.0`).
    #[arg(long = "api-version", value_name = "VER")]
    api_version: Option<String>,

    /// Project root directory containing `sfdx-project.json` and `.forceignore`.
    #[arg(long = "project-dir", value_name = "PATH")]
    project_dir: Option<PathBuf>,

    /// Write directly into `--output-dir` rather than creating a timestamped subdirectory.
    #[arg(long = "exact-dir")]
    exact_dir: bool,

    /// Persistent `blake3` incremental conversion cache directory.
    #[arg(long = "cache-dir", value_name = "PATH")]
    cache_dir: Option<PathBuf>,

    /// Always re-hash files with `blake3` instead of trusting unchanged length/mtime stats.
    #[arg(long = "cache-always-hash")]
    cache_always_hash: bool,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        TopCommand::Convert {
            target: ConvertSubcommand::Mdapi(args),
        } => run_convert(&args, &[], TargetFormat::Metadata),
        TopCommand::Convert {
            target: ConvertSubcommand::Source { common, merge_with },
        } => run_convert(&common, &merge_with, TargetFormat::Source),
    }
}

fn run_convert(
    args: &ConvertCommonArgs,
    merge_with_paths: &[PathBuf],
    target: TargetFormat,
) -> Result<()> {
    let cs_opts = build_component_set_options(args)?;
    let cs = ComponentSetBuilder::build(&cs_opts).context("failed to resolve components")?;

    let registry = match &args.project_dir {
        Some(dir) => RegistryAccess::from_project_dir(dir)
            .with_context(|| format!("failed to load registry from {}", dir.display()))?,
        None => RegistryAccess::new(),
    };
    let converter = MetadataConverter::new(registry);

    let unique_dir = if args.exact_dir {
        UniqueDirPolicy::Exact
    } else {
        UniqueDirPolicy::Generate
    };

    let cache_dir = args.cache_dir.clone();
    let cache_always_hash = args.cache_always_hash;
    let output = build_convert_output(args, merge_with_paths, unique_dir, target)?;
    let result = if let Some(dir) = cache_dir {
        let verify_mode = if cache_always_hash {
            CacheVerifyMode::AlwaysHash
        } else {
            CacheVerifyMode::StatThenHash
        };
        let cache = ConvertCache::new(dir).with_verify_mode(verify_mode);
        converter
            .convert_incremental(&cs, target, output, &cache)
            .context("incremental metadata conversion failed")?
    } else {
        converter
            .convert(&cs, target, output)
            .context("metadata conversion failed")?
    };

    report_result(&result)
}

fn build_component_set_options(args: &ConvertCommonArgs) -> Result<ComponentSetOptions> {
    if args.sourcepath.is_empty() && args.manifest.is_none() && args.metadata.is_empty() {
        bail!("at least one of --sourcepath, --manifest, or --metadata is required");
    }

    for entry in &args.metadata {
        let Some((ty, name)) = entry.split_once(':') else {
            bail!("invalid --metadata value '{entry}': expected Type:FullName");
        };
        if ty.trim().is_empty() || name.trim().is_empty() {
            bail!("invalid --metadata value '{entry}': expected non-empty Type:FullName");
        }
    }

    let manifest = args.manifest.as_ref().map(|manifest_path| ManifestOptions {
        manifest_path: manifest_path.clone(),
        directory_paths: args.sourcepath.clone(),
        ..Default::default()
    });

    let metadata = (!args.metadata.is_empty()).then(|| MetadataOptions {
        metadata_entries: args.metadata.clone(),
        directory_paths: args.sourcepath.clone(),
        ..Default::default()
    });

    let sourcepath = if args.manifest.is_some() {
        Vec::new()
    } else {
        args.sourcepath.clone()
    };

    Ok(ComponentSetOptions {
        sourcepath,
        manifest,
        metadata,
        api_version: args.api_version.clone(),
        source_api_version: None,
        project_dir: args.project_dir.clone(),
    })
}

fn build_convert_output(
    args: &ConvertCommonArgs,
    merge_with_paths: &[PathBuf],
    unique_dir: UniqueDirPolicy,
    target: TargetFormat,
) -> Result<ConvertOutput> {
    if !merge_with_paths.is_empty() {
        if target != TargetFormat::Source {
            bail!("--merge-with is only valid when converting to source format");
        }
        if args.zip {
            bail!("--zip cannot be combined with --merge-with");
        }
        let merge_set = ComponentSetBuilder::build(&ComponentSetOptions {
            sourcepath: merge_with_paths.to_vec(),
            project_dir: args.project_dir.clone(),
            ..Default::default()
        })
        .context("failed to resolve --merge-with components")?;
        let default_dir = args
            .output_dir
            .clone()
            .unwrap_or_else(|| merge_with_paths[0].clone());
        return Ok(ConvertOutput::Merge {
            merge_with: merge_set.source_components(),
            default_dir,
            project_dir: args.project_dir.clone(),
        });
    }

    let out_dir = args
        .output_dir
        .clone()
        .unwrap_or_else(|| PathBuf::from("."));
    if args.zip {
        Ok(ConvertOutput::Zip {
            output_dir: Some(out_dir),
            package_name: args.package_name.clone(),
            unique_dir,
        })
    } else {
        Ok(ConvertOutput::Directory {
            output_dir: out_dir,
            package_name: args.package_name.clone(),
            unique_dir,
        })
    }
}

fn report_result(result: &busbar_sf_source_format::convert::ConvertResult) -> Result<()> {
    let mut out = io::stdout().lock();
    if let Some(pkg) = &result.package_path {
        writeln!(out, "output: {}", pkg.display())?;
    }
    writeln!(out, "converted components: {}", result.converted.len())?;
    if !result.deleted.is_empty() {
        writeln!(out, "deleted files: {}", result.deleted.len())?;
    }
    if let Some(stats) = result.incremental {
        writeln!(
            out,
            "incremental: {}/{} units cached ({} rebuilt), {} written, {} skipped, {} stale removed",
            stats.cached_units,
            stats.total_units,
            stats.rebuilt_units,
            stats.written_files,
            stats.skipped_writes,
            stats.removed_stale_files
        )?;
    }
    Ok(())
}
