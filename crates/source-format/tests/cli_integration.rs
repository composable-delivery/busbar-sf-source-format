//! End-to-end CLI integration tests exercising `sf-source-format`.

use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::Command;

use zip::ZipArchive;

fn bin_path() -> &'static str {
    env!("CARGO_BIN_EXE_sf-source-format")
}

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
            fs::copy(&sp, &dp).expect("copy file");
        }
    }
}

#[test]
fn cli_converts_mdapi_to_source_directory() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let work = tmp.path().join("customLabels-simple");
    copy_dir_filtered(&fixture_root("customLabels-simple"), &work);

    let out = work.join("force-app");
    let status = Command::new(bin_path())
        .args(["convert", "source", "--sourcepath"])
        .arg(work.join("originalMdapi"))
        .arg("--output-dir")
        .arg(&out)
        .arg("--project-dir")
        .arg(&work)
        .arg("--exact-dir")
        .status()
        .expect("run cli convert source");

    assert!(status.success(), "cli convert source failed: {status}");
    assert!(
        out.join("main/default/labels/CustomLabels.labels-meta.xml")
            .exists(),
        "expected converted labels file"
    );
}

#[test]
fn cli_converts_source_to_mdapi_directory_and_zip() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let work = tmp.path().join("customLabels-simple");
    copy_dir_filtered(&fixture_root("customLabels-simple"), &work);

    let sfdx_dir = work.join("force-app");
    let to_src = Command::new(bin_path())
        .args(["convert", "source", "--sourcepath"])
        .arg(work.join("originalMdapi"))
        .arg("--output-dir")
        .arg(&sfdx_dir)
        .arg("--project-dir")
        .arg(&work)
        .arg("--exact-dir")
        .status()
        .expect("prepare sfdx dir");
    assert!(to_src.success(), "prepare sfdx dir failed");

    let mdapi_dir = work.join("mdapiOut");
    let to_md = Command::new(bin_path())
        .args(["convert", "mdapi", "--sourcepath"])
        .arg(&sfdx_dir)
        .arg("--output-dir")
        .arg(&mdapi_dir)
        .arg("--project-dir")
        .arg(&work)
        .arg("--exact-dir")
        .status()
        .expect("run cli convert mdapi");
    assert!(to_md.success(), "cli convert mdapi failed");
    assert!(
        mdapi_dir.join("package.xml").exists(),
        "missing package.xml"
    );
    assert!(
        mdapi_dir.join("labels/CustomLabels.labels").exists(),
        "missing CustomLabels.labels"
    );

    let zip_out_dir = work.join("zips");
    let to_zip = Command::new(bin_path())
        .args(["convert", "mdapi", "--sourcepath"])
        .arg(&sfdx_dir)
        .arg("--output-dir")
        .arg(&zip_out_dir)
        .arg("--package-name")
        .arg("pkg")
        .arg("--zip")
        .status()
        .expect("run cli convert mdapi --zip");
    assert!(to_zip.success(), "cli convert mdapi --zip failed");

    let zip_path = zip_out_dir.join("pkg.zip");
    assert!(zip_path.exists(), "missing pkg.zip");
    let mut archive = ZipArchive::new(File::open(&zip_path).expect("open pkg.zip")).expect("zip");
    assert!(
        archive.by_name("package.xml").is_ok(),
        "zip lacks package.xml"
    );
    assert!(
        archive.by_name("labels/CustomLabels.labels").is_ok(),
        "zip lacks CustomLabels.labels"
    );
}

#[test]
fn cli_converts_source_with_merge_with() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let work = tmp.path().join("customLabels-simple");
    copy_dir_filtered(&fixture_root("customLabels-simple"), &work);

    let force_app = work.join("force-app");
    let first = Command::new(bin_path())
        .args(["convert", "source", "--sourcepath"])
        .arg(work.join("originalMdapi"))
        .arg("--output-dir")
        .arg(&force_app)
        .arg("--exact-dir")
        .status()
        .expect("initial convert");
    assert!(first.success(), "initial convert failed");

    let merge = Command::new(bin_path())
        .args(["convert", "source", "--sourcepath"])
        .arg(work.join("originalMdapi"))
        .arg("--merge-with")
        .arg(&force_app)
        .arg("--output-dir")
        .arg(&force_app)
        .arg("--project-dir")
        .arg(&work)
        .status()
        .expect("merge convert");
    assert!(merge.success(), "merge convert failed");
    assert!(
        force_app
            .join("main/default/labels/CustomLabels.labels-meta.xml")
            .exists()
    );
}

#[test]
fn cli_rejects_missing_inputs_and_invalid_metadata_flag() {
    let no_args = Command::new(bin_path())
        .args(["convert", "mdapi"])
        .output()
        .expect("run cli missing args");
    assert!(!no_args.status.success(), "missing args should fail");

    let bad_meta = Command::new(bin_path())
        .args(["convert", "mdapi", "--metadata", "ApexClassNoColon"])
        .output()
        .expect("run cli bad --metadata");
    assert!(!bad_meta.status.success(), "bad --metadata should fail");
}

#[test]
fn cli_incremental_cache_dir_cold_warm_and_single_edit() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let work = tmp.path().join("customLabels-simple");
    copy_dir_filtered(&fixture_root("customLabels-simple"), &work);

    let sfdx_dir = work.join("force-app");
    let prep = Command::new(bin_path())
        .args(["convert", "source", "--sourcepath"])
        .arg(work.join("originalMdapi"))
        .arg("--output-dir")
        .arg(&sfdx_dir)
        .arg("--project-dir")
        .arg(&work)
        .arg("--exact-dir")
        .status()
        .expect("prepare sfdx");
    assert!(prep.success(), "prepare sfdx failed");

    let mdapi_out = work.join("mdapiCacheOut");
    let cache_dir = work.join("sfCache");

    let cold = Command::new(bin_path())
        .args(["convert", "mdapi", "--sourcepath"])
        .arg(&sfdx_dir)
        .arg("--output-dir")
        .arg(&mdapi_out)
        .arg("--project-dir")
        .arg(&work)
        .arg("--exact-dir")
        .arg("--cache-dir")
        .arg(&cache_dir)
        .output()
        .expect("cold cached cli");
    assert!(cold.status.success(), "cold cached cli failed");
    let cold_stdout = String::from_utf8_lossy(&cold.stdout);
    assert!(
        cold_stdout.contains("0/1 units cached (1 rebuilt)"),
        "unexpected cold stdout: {cold_stdout}"
    );

    let warm = Command::new(bin_path())
        .args(["convert", "mdapi", "--sourcepath"])
        .arg(&sfdx_dir)
        .arg("--output-dir")
        .arg(&mdapi_out)
        .arg("--project-dir")
        .arg(&work)
        .arg("--exact-dir")
        .arg("--cache-dir")
        .arg(&cache_dir)
        .output()
        .expect("warm cached cli");
    assert!(warm.status.success(), "warm cached cli failed");
    let warm_stdout = String::from_utf8_lossy(&warm.stdout);
    assert!(
        warm_stdout.contains("1/1 units cached (0 rebuilt)"),
        "unexpected warm stdout: {warm_stdout}"
    );

    // Edit CustomLabels file and ensure 1 unit rebuilds cleanly.
    let labels_file = sfdx_dir.join("main/default/labels/CustomLabels.labels-meta.xml");
    let updated = fs::read_to_string(&labels_file)
        .expect("read labels")
        .replace("</CustomLabels>", "<!-- edited -->\n</CustomLabels>");
    fs::write(&labels_file, updated).expect("edit labels");

    let edited = Command::new(bin_path())
        .args(["convert", "mdapi", "--sourcepath"])
        .arg(&sfdx_dir)
        .arg("--output-dir")
        .arg(&mdapi_out)
        .arg("--project-dir")
        .arg(&work)
        .arg("--exact-dir")
        .arg("--cache-dir")
        .arg(&cache_dir)
        .output()
        .expect("edited cached cli");
    assert!(edited.status.success(), "edited cached cli failed");
    let edited_stdout = String::from_utf8_lossy(&edited.stdout);
    assert!(
        edited_stdout.contains("0/1 units cached (1 rebuilt)"),
        "unexpected edited stdout: {edited_stdout}"
    );
}
