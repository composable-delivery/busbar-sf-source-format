//! Focused deploy-vs-retrieve tests for `UIBundle` (`ui-bundle.json` and `mcp-resources.json`)
//! descriptor validation.

use std::fs;
use std::path::Path;

use busbar_sf_source_format::convert::{
    ConvertError, ConvertOutput, MetadataConverter, TargetFormat, UniqueDirPolicy,
};
use busbar_sf_source_format::registry::RegistryAccess;
use busbar_sf_source_format::resolve::{ComponentSetBuilder, ComponentSetOptions};

fn write_minimal_ui_bundle_source(
    root: &Path,
    ui_bundle_json: Option<&str>,
    mcp_resources_json: Option<&str>,
) {
    let bundle_dir = root.join("force-app/main/default/uiBundles/TestApp");
    let src_dir = bundle_dir.join("src");
    fs::create_dir_all(&src_dir).expect("mkdir ui bundle");
    fs::write(
        bundle_dir.join("TestApp.uibundle-meta.xml"),
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<UIBundle xmlns=\"http://soap.sforce.com/2006/04/metadata\">\n    <masterLabel>TestApp</masterLabel>\n</UIBundle>\n",
    )
    .expect("write meta xml");
    fs::write(src_dir.join("index.html"), "<html></html>").expect("write index.html");
    if let Some(json) = ui_bundle_json {
        fs::write(bundle_dir.join("ui-bundle.json"), json).expect("write ui-bundle.json");
    }
    if let Some(mcp) = mcp_resources_json {
        fs::write(bundle_dir.join("mcp-resources.json"), mcp).expect("write mcp-resources.json");
    }
}

fn convert_to_mdapi(root: &Path) -> Result<(), ConvertError> {
    let cs = ComponentSetBuilder::build(&ComponentSetOptions {
        sourcepath: vec![root.join("force-app")],
        ..Default::default()
    })
    .expect("resolve test UIBundle");
    let converter = MetadataConverter::new(RegistryAccess::new());
    let out_dir = root.join("mdapiOut");
    converter
        .convert(
            &cs,
            TargetFormat::Metadata,
            ConvertOutput::Directory {
                output_dir: out_dir,
                package_name: None,
                unique_dir: UniqueDirPolicy::Exact,
            },
        )
        .map(|_| ())
}

#[test]
fn deploy_rejects_empty_ui_bundle_json() {
    let tmp = tempfile::tempdir().expect("tempdir");
    write_minimal_ui_bundle_source(tmp.path(), Some("   \n"), None);
    let err = convert_to_mdapi(tmp.path()).expect_err("empty ui-bundle.json must fail");
    assert!(
        matches!(err, ConvertError::UiBundleValidation { .. }),
        "unexpected error: {err}"
    );
}

#[test]
fn deploy_rejects_unknown_property_in_ui_bundle_json() {
    let tmp = tempfile::tempdir().expect("tempdir");
    write_minimal_ui_bundle_source(
        tmp.path(),
        Some(r#"{"outputDir":"src","bogus":true}"#),
        None,
    );
    let err = convert_to_mdapi(tmp.path()).expect_err("unknown property must fail");
    assert!(err.to_string().contains("bogus"), "unexpected error: {err}");
}

#[test]
fn deploy_rejects_path_traversal_in_output_dir() {
    let tmp = tempfile::tempdir().expect("tempdir");
    write_minimal_ui_bundle_source(tmp.path(), Some(r#"{"outputDir":"../escape"}"#), None);
    let err = convert_to_mdapi(tmp.path()).expect_err("path traversal must fail");
    assert!(
        matches!(err, ConvertError::UiBundleValidation { .. }),
        "unexpected error: {err}"
    );
}

#[test]
fn deploy_rejects_invalid_mcp_resources_non_html_and_duplicate_name() {
    let tmp = tempfile::tempdir().expect("tempdir");
    write_minimal_ui_bundle_source(
        tmp.path(),
        Some(r#"{"outputDir":"src"}"#),
        Some(r#"{"mcpResources":[{"name":"widget","path":"src/index.js"}]}"#),
    );
    let err = convert_to_mdapi(tmp.path()).expect_err("non-.html mcp resource must fail");
    assert!(
        err.to_string().contains(".html"),
        "unexpected error for non-html mcp resource: {err}"
    );

    let tmp_dup = tempfile::tempdir().expect("tempdir");
    write_minimal_ui_bundle_source(
        tmp_dup.path(),
        Some(r#"{"outputDir":"src"}"#),
        Some(
            r#"{"mcpResources":[{"name":"dup","path":"src/index.html"},{"name":"dup","path":"src/index.html"}]}"#,
        ),
    );
    let dup_err = convert_to_mdapi(tmp_dup.path()).expect_err("duplicate mcp name must fail");
    assert!(
        dup_err.to_string().contains("duplicate"),
        "unexpected error for duplicate mcp name: {dup_err}"
    );
}

#[test]
fn retrieve_skips_ui_bundle_descriptor_validation() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let md_bundle = tmp.path().join("mdapi/uiBundles/TestApp");
    let src_dir = md_bundle.join("src");
    fs::create_dir_all(&src_dir).expect("mkdir mdapi ui bundle");
    fs::write(
        md_bundle.join("TestApp.uibundle-meta.xml"),
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<UIBundle xmlns=\"http://soap.sforce.com/2006/04/metadata\">\n    <masterLabel>TestApp</masterLabel>\n</UIBundle>\n",
    )
    .expect("write meta xml");
    fs::write(src_dir.join("index.html"), "<html></html>").expect("write index.html");
    fs::write(md_bundle.join("ui-bundle.json"), "{ invalid json").expect("write bad json");

    let cs = ComponentSetBuilder::build(&ComponentSetOptions {
        sourcepath: vec![tmp.path().join("mdapi")],
        ..Default::default()
    })
    .expect("resolve mdapi UIBundle");

    let out_dir = tmp.path().join("force-app");
    fs::create_dir_all(&out_dir).expect("mkdir out_dir");
    MetadataConverter::new(RegistryAccess::new())
        .convert(
            &cs,
            TargetFormat::Source,
            ConvertOutput::Directory {
                output_dir: out_dir.clone(),
                package_name: None,
                unique_dir: UniqueDirPolicy::Exact,
            },
        )
        .expect("retrieve to source format must skip ui-bundle.json validation");

    assert!(
        out_dir
            .join("main/default/uiBundles/TestApp/ui-bundle.json")
            .exists(),
        "ui-bundle.json should be copied during retrieve even if malformed"
    );
}
