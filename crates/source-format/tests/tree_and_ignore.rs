//! Tests for `FsTree`, `ZipTree`, `MemoryTree`, `ForceIgnore`, and path safety checks.

use std::fs;
use std::io::{Cursor, Write as _};
use std::path::Path;

use busbar_sf_source_format::tree::{
    ForceIgnore, FsTree, MemoryTree, TreeContainer, TreeError, TreeFileKind, ZipTree,
    ensure_no_symlink_on_path, ensure_safe_directory_name, ensure_within_root,
};
use zip::ZipWriter;
use zip::write::SimpleFileOptions;

#[test]
fn force_ignore_matches_fixture_rules_and_defaults() {
    let fixture_dir =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/sdr-snapshots/forceignore");
    let fi = ForceIgnore::find_and_create(&fixture_dir.join("originalMdapi"))
        .expect("load .forceignore");

    assert!(fi.denies(&fixture_dir.join("originalMdapi/profiles/Admin.profile")));
    assert!(
        fi.denies(&fixture_dir.join("force-app/main/default/profiles/Admin.profile-meta.xml"))
            || fi.accepts(
                &fixture_dir.join("force-app/main/default/profiles/Admin.profile-meta.xml")
            )
    );
    // Verify exact rule `**/*.profile`: denies `.profile`, accepts `.cls`, denies `.dup` default ignore
    assert!(fi.accepts(&fixture_dir.join("originalMdapi/classes/OneClass.cls")));
    assert!(fi.denies(&fixture_dir.join("originalMdapi/classes/OneClass.cls.dup")));
    assert!(fi.denies(&fixture_dir.join(".git/config")));
    assert!(fi.accepts(&fixture_dir.join(".sf/orgs/123/remoteMetadata/classes/A.cls")));
    assert!(fi.accepts(Path::new("/completely/different/root/Admin.profile")));
}

#[test]
fn fs_and_memory_tree_find_content_and_metadata_xml() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let classes = tmp.path().join("classes");
    fs::create_dir_all(&classes).expect("mkdir");
    fs::write(classes.join("Foo.cls"), "public class Foo {}").expect("write cls");
    fs::write(classes.join("Foo.cls-meta.xml"), "<ApexClass/>").expect("write meta");

    let fs_tree = FsTree::new();
    assert!(fs_tree.exists(&classes));
    assert!(fs_tree.is_directory(&classes).expect("is_dir"));
    assert_eq!(
        fs_tree.find(TreeFileKind::Content, "Foo", &classes),
        Some(classes.join("Foo.cls"))
    );
    assert_eq!(
        fs_tree.find(TreeFileKind::MetadataXml, "Foo", &classes),
        Some(classes.join("Foo.cls-meta.xml"))
    );

    let mut mem = MemoryTree::new();
    mem.insert_file("classes/Bar.cls", b"public class Bar {}".to_vec());
    mem.insert_file("classes/Bar.cls-meta.xml", b"<ApexClass/>".to_vec());
    assert_eq!(
        mem.read_to_string(Path::new("classes/Bar.cls"))
            .expect("read mem"),
        "public class Bar {}"
    );
    assert_eq!(
        mem.find(TreeFileKind::MetadataXml, "Bar", Path::new("classes")),
        Some(Path::new("classes/Bar.cls-meta.xml").to_path_buf())
    );
}

#[test]
fn zip_tree_reads_entries_and_synthesizes_parent_dirs() {
    let mut buf = Cursor::new(Vec::new());
    {
        let mut writer = ZipWriter::new(&mut buf);
        writer
            .start_file(
                "unpackaged/objects/Broker__c.object",
                SimpleFileOptions::default(),
            )
            .expect("start file");
        writer.write_all(b"<CustomObject/>").expect("write");
        writer.finish().expect("finish");
    }

    let zip_tree = ZipTree::from_bytes(buf.get_ref()).expect("zip tree");
    assert!(
        zip_tree
            .is_directory(Path::new("unpackaged"))
            .expect("is_dir")
    );
    assert!(
        zip_tree
            .is_directory(Path::new("unpackaged/objects"))
            .expect("is_dir")
    );
    assert_eq!(
        zip_tree
            .read_directory(Path::new("."))
            .expect("root children"),
        vec!["unpackaged".to_owned()]
    );
    assert_eq!(
        zip_tree
            .read_to_string(Path::new("unpackaged/objects/Broker__c.object"))
            .expect("read xml"),
        "<CustomObject/>"
    );
}

#[test]
fn safety_guards_reject_traversal_and_symlinks() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let outside = tempfile::tempdir().expect("outside");

    let err =
        ensure_within_root(tmp.path(), Path::new("../escape.txt")).expect_err("reject escape");
    assert!(matches!(err, TreeError::PathOutsideRoot { .. }));

    assert!(ensure_within_root(tmp.path(), Path::new("sub/ok.xml")).is_ok());

    let err = ensure_safe_directory_name("../evil", "Evil").expect_err("reject ..");
    assert!(matches!(err, TreeError::PathTraversal { .. }));

    #[cfg(unix)]
    {
        let link_path = tmp.path().join("symlink_dir");
        std::os::unix::fs::symlink(outside.path(), &link_path).expect("create symlink");
        let err = ensure_no_symlink_on_path(tmp.path(), &link_path.join("file.txt"))
            .expect_err("reject symlink");
        assert!(matches!(err, TreeError::SymlinkOnPath { .. }));
    }
}
