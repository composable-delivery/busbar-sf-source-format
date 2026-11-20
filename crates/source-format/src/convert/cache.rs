//! Persistent `blake3` content-addressed incremental conversion cache.
//!
//! Partitions [`SourceComponent`] inputs into disjoint work units (single standalone components or
//! multi-component recomposition/finalizer buckets), hashes inputs and environment configuration
//! with `blake3`, skips unchanged writes or ZIP re-compressions, and prunes stale outputs.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::fs::{self, File};
use std::io::{Cursor, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use crate::registry::{RegistryAccess, SfdxProjectConfig, TransformerStrategy};
use crate::tree::{ForceIgnore, TreeContainer, ensure_no_symlink_on_path, normalize_lexical};
use indexmap::IndexMap;
use rayon::prelude::*;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, PreparedZipFile, ZipArchive, ZipWriter};

use crate::convert::MetadataConverter;
use crate::convert::converter::{
    PreparedOutput, prepare_component_set, resolve_converted_paths_parallel,
    resolve_scope_for_component, transform_component_to_source_with_check,
};
use crate::convert::error::ConvertError;
use crate::convert::finalizers::ConvertContext;
use crate::convert::git_index::GitIndexCache;
use crate::convert::replacements::mark_replacements;
use crate::convert::transformers::{
    ParentDiskCheck, effective_transformer_strategy, transform_to_metadata,
    try_transform_to_metadata_parallel,
};
use crate::convert::types::{
    CacheVerifyMode, ConvertCache, ConvertOutput, ConvertResult, IncrementalResolvePolicy,
    IncrementalStats, ResolveScope, TargetFormat, UniqueDirPolicy, WriteAction, WriteSource,
    WriterFormat,
};
use crate::convert::writer::{
    ContentDigest, DigestKind, StandardWriter, WrittenFileStamp, absolutize_with_cwd,
    metadata_len_mtime, resolve_write_source,
};
use crate::io_pool::io_install;
use crate::resolve::{ComponentSet, SourceComponent, TreeHandle};

const CACHE_MAGIC: &[u8; 8] = b"SFSFC002";
const PACKAGE_XML_FILE: &str = "package.xml";
const STREAM_HASH_THRESHOLD_BYTES: u64 = 256 * 1024;
static TEMP_FILE_SEQ: AtomicU64 = AtomicU64::new(0);

// ---------------------------------------------------------------------------
// Cache index schema & binary codec
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FileStamp {
    digest: ContentDigest,
    len: u64,
    mtime_secs: i64,
    mtime_nanos: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct OutputFileRecord {
    rel_path: String,
    stamp: FileStamp,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct UnitCacheEntry {
    unit_digest: [u8; 32],
    outputs: Vec<OutputFileRecord>,
    converted_rel_paths: Vec<String>,
}

#[derive(Debug, Clone, Default)]
struct CacheIndex {
    env_hash: [u8; 32],
    archive_stamp: Option<FileStamp>,
    manifests: BTreeMap<String, OutputFileRecord>,
    units: BTreeMap<String, UnitCacheEntry>,
    input_stamps: BTreeMap<String, FileStamp>,
}

impl CacheIndex {
    fn encode(&self) -> Result<Vec<u8>, ConvertError> {
        let mut w = BinWriter::new();
        w.write_bytes_raw(CACHE_MAGIC);
        w.write_hash(&self.env_hash);
        match self.archive_stamp {
            Some(stamp) => {
                w.write_u8(1);
                w.write_stamp(&stamp);
            }
            None => w.write_u8(0),
        }

        w.write_usize(self.manifests.len())?;
        for (key, rec) in &self.manifests {
            w.write_str(key)?;
            w.write_output_record(rec)?;
        }

        w.write_usize(self.units.len())?;
        for (key, entry) in &self.units {
            w.write_str(key)?;
            w.write_hash(&entry.unit_digest);
            w.write_usize(entry.outputs.len())?;
            for rec in &entry.outputs {
                w.write_output_record(rec)?;
            }
            w.write_usize(entry.converted_rel_paths.len())?;
            for rel in &entry.converted_rel_paths {
                w.write_str(rel)?;
            }
        }

        w.write_usize(self.input_stamps.len())?;
        for (path, stamp) in &self.input_stamps {
            w.write_str(path)?;
            w.write_stamp(stamp);
        }

        let checksum = blake3::hash(&w.buf);
        w.write_bytes_raw(checksum.as_bytes());
        Ok(w.buf)
    }

    fn decode(bytes: &[u8]) -> Option<Self> {
        if bytes.len() < CACHE_MAGIC.len() + 32 {
            return None;
        }
        let (payload, checksum) = bytes.split_at(bytes.len() - 32);
        let expected = blake3::hash(payload);
        if expected.as_bytes() != checksum {
            return None;
        }

        let mut r = BinReader::new(payload);
        let magic = r.read_exact::<8>()?;
        if &magic != CACHE_MAGIC {
            return None;
        }

        let env_hash = r.read_exact::<32>()?;
        let has_archive = r.read_u8()?;
        let archive_stamp = match has_archive {
            0 => None,
            1 => Some(r.read_stamp()?),
            _ => return None,
        };

        let manifest_count = r.read_usize()?;
        let mut manifests = BTreeMap::new();
        for _ in 0..manifest_count {
            let key = r.read_str()?;
            let rec = r.read_output_record()?;
            manifests.insert(key, rec);
        }

        let unit_count = r.read_usize()?;
        let mut units = BTreeMap::new();
        for _ in 0..unit_count {
            let key = r.read_str()?;
            let unit_digest = r.read_exact::<32>()?;
            let out_count = r.read_usize()?;
            let mut outputs = Vec::with_capacity(out_count.min(4096));
            for _ in 0..out_count {
                outputs.push(r.read_output_record()?);
            }
            let conv_count = r.read_usize()?;
            let mut converted_rel_paths = Vec::with_capacity(conv_count.min(4096));
            for _ in 0..conv_count {
                converted_rel_paths.push(r.read_str()?);
            }
            units.insert(
                key,
                UnitCacheEntry {
                    unit_digest,
                    outputs,
                    converted_rel_paths,
                },
            );
        }

        let stamp_count = r.read_usize()?;
        let mut input_stamps = BTreeMap::new();
        for _ in 0..stamp_count {
            let path = r.read_str()?;
            let stamp = r.read_stamp()?;
            input_stamps.insert(path, stamp);
        }

        if !r.is_empty() {
            return None;
        }

        Some(Self {
            env_hash,
            archive_stamp,
            manifests,
            units,
            input_stamps,
        })
    }

    fn load_from(path: &Path) -> Option<Self> {
        let bytes = fs::read(path).ok()?;
        Self::decode(&bytes)
    }

    fn save_to(&self, path: &Path) -> Result<(), ConvertError> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| ConvertError::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        let encoded = self.encode()?;
        atomic_write_bytes(path, &encoded)
    }
}

struct BinWriter {
    buf: Vec<u8>,
}

impl BinWriter {
    fn new() -> Self {
        Self {
            buf: Vec::with_capacity(4096),
        }
    }

    fn write_bytes_raw(&mut self, bytes: &[u8]) {
        self.buf.extend_from_slice(bytes);
    }

    fn write_u8(&mut self, v: u8) {
        self.buf.push(v);
    }

    fn write_u32(&mut self, v: u32) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    fn write_u64(&mut self, v: u64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    fn write_i64(&mut self, v: i64) {
        self.buf.extend_from_slice(&v.to_le_bytes());
    }

    fn write_usize(&mut self, v: usize) -> Result<(), ConvertError> {
        let len_u64 = u64::try_from(v)
            .map_err(|_| io_invalid_data(PathBuf::from("<cache-encode>"), "length exceeds u64"))?;
        self.write_u64(len_u64);
        Ok(())
    }

    fn write_str(&mut self, s: &str) -> Result<(), ConvertError> {
        self.write_usize(s.len())?;
        self.write_bytes_raw(s.as_bytes());
        Ok(())
    }

    fn write_hash(&mut self, hash: &[u8; 32]) {
        self.buf.extend_from_slice(hash);
    }

    fn write_stamp(&mut self, stamp: &FileStamp) {
        self.write_u8(stamp.digest.kind.to_u8());
        self.write_hash(&stamp.digest.bytes);
        self.write_u64(stamp.len);
        self.write_i64(stamp.mtime_secs);
        self.write_u32(stamp.mtime_nanos);
    }

    fn write_output_record(&mut self, rec: &OutputFileRecord) -> Result<(), ConvertError> {
        self.write_str(&rec.rel_path)?;
        self.write_stamp(&rec.stamp);
        Ok(())
    }
}

struct BinReader<'a> {
    rest: &'a [u8],
}

impl<'a> BinReader<'a> {
    fn new(rest: &'a [u8]) -> Self {
        Self { rest }
    }

    fn is_empty(&self) -> bool {
        self.rest.is_empty()
    }

    fn read_slice(&mut self, len: usize) -> Option<&'a [u8]> {
        if self.rest.len() < len {
            return None;
        }
        let (head, tail) = self.rest.split_at(len);
        self.rest = tail;
        Some(head)
    }

    fn read_exact<const N: usize>(&mut self) -> Option<[u8; N]> {
        let slice = self.read_slice(N)?;
        let mut out = [0u8; N];
        out.copy_from_slice(slice);
        Some(out)
    }

    fn read_u8(&mut self) -> Option<u8> {
        let [b] = self.read_exact::<1>()?;
        Some(b)
    }

    fn read_u32(&mut self) -> Option<u32> {
        Some(u32::from_le_bytes(self.read_exact::<4>()?))
    }

    fn read_u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.read_exact::<8>()?))
    }

    fn read_i64(&mut self) -> Option<i64> {
        Some(i64::from_le_bytes(self.read_exact::<8>()?))
    }

    fn read_usize(&mut self) -> Option<usize> {
        usize::try_from(self.read_u64()?).ok()
    }

    fn read_str(&mut self) -> Option<String> {
        let len = self.read_usize()?;
        let bytes = self.read_slice(len)?;
        std::str::from_utf8(bytes).ok().map(str::to_owned)
    }

    fn read_stamp(&mut self) -> Option<FileStamp> {
        let kind = DigestKind::from_u8(self.read_u8()?)?;
        let bytes = self.read_exact::<32>()?;
        Some(FileStamp {
            digest: ContentDigest { kind, bytes },
            len: self.read_u64()?,
            mtime_secs: self.read_i64()?,
            mtime_nanos: self.read_u32()?,
        })
    }

    fn read_output_record(&mut self) -> Option<OutputFileRecord> {
        Some(OutputFileRecord {
            rel_path: self.read_str()?,
            stamp: self.read_stamp()?,
        })
    }
}

fn io_invalid_data(path: PathBuf, msg: &str) -> ConvertError {
    ConvertError::Io {
        path,
        source: std::io::Error::new(std::io::ErrorKind::InvalidData, msg),
    }
}

fn atomic_write_bytes(dest: &Path, bytes: &[u8]) -> Result<(), ConvertError> {
    let parent = dest.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent).map_err(|source| ConvertError::Io {
        path: parent.to_path_buf(),
        source,
    })?;
    let seq = TEMP_FILE_SEQ.fetch_add(1, Ordering::Relaxed);
    let pid = std::process::id();
    let file_name = dest
        .file_name()
        .map_or_else(|| "cache".to_owned(), |n| n.to_string_lossy().into_owned());
    let tmp = parent.join(format!(".{file_name}.tmp.{pid}.{seq}"));

    {
        let mut f = File::create(&tmp).map_err(|source| ConvertError::Io {
            path: tmp.clone(),
            source,
        })?;
        f.write_all(bytes).map_err(|source| ConvertError::Io {
            path: tmp.clone(),
            source,
        })?;
    }

    fs::rename(&tmp, dest).map_err(|source| {
        let _ = fs::remove_file(&tmp);
        ConvertError::Io {
            path: dest.to_path_buf(),
            source,
        }
    })
}

// ---------------------------------------------------------------------------
// Path normalization, hashing, and target/env fingerprints
// ---------------------------------------------------------------------------

fn hash_bytes_fast_or_stream(path: &Path, len: u64) -> Result<[u8; 32], ConvertError> {
    if len <= STREAM_HASH_THRESHOLD_BYTES {
        let bytes = fs::read(path).map_err(|source| ConvertError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        return Ok(*blake3::hash(&bytes).as_bytes());
    }
    let mut file = File::open(path).map_err(|source| ConvertError::Io {
        path: path.to_path_buf(),
        source,
    })?;
    let mut hasher = blake3::Hasher::new();
    hasher
        .update_reader(&mut file)
        .map_err(|source| ConvertError::Io {
            path: path.to_path_buf(),
            source,
        })?;
    Ok(*hasher.finalize().as_bytes())
}

fn stamp_regular_file_with_git(
    path: &Path,
    cached: Option<&FileStamp>,
    verify_mode: CacheVerifyMode,
    git_index: Option<&GitIndexCache>,
    cwd: Option<&Path>,
) -> Result<Option<FileStamp>, ConvertError> {
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(source) => {
            return Err(ConvertError::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    if !meta.file_type().is_file() {
        return Ok(None);
    }
    let (len, mtime_secs, mtime_nanos) = metadata_len_mtime(&meta);
    if verify_mode == CacheVerifyMode::StatThenHash
        && let Some(prev) = cached
        && prev.len == len
        && prev.mtime_secs == mtime_secs
        && prev.mtime_nanos == mtime_nanos
    {
        return Ok(Some(*prev));
    }
    if verify_mode == CacheVerifyMode::StatThenHash
        && let Some(gi) = git_index
        && let abs_path = absolutize_with_cwd(path, cwd)
        && let Some((digest, git_len, git_secs, git_nanos)) =
            gi.lookup_clean_stamp(&abs_path, &meta)
    {
        return Ok(Some(FileStamp {
            digest,
            len: git_len,
            mtime_secs: git_secs,
            mtime_nanos: git_nanos,
        }));
    }
    let blake3_bytes = hash_bytes_fast_or_stream(path, len)?;
    Ok(Some(FileStamp {
        digest: ContentDigest::from_blake3_bytes(blake3_bytes),
        len,
        mtime_secs,
        mtime_nanos,
    }))
}

fn stamp_regular_file(
    path: &Path,
    cached: Option<&FileStamp>,
    verify_mode: CacheVerifyMode,
) -> Result<Option<FileStamp>, ConvertError> {
    stamp_regular_file_with_git(path, cached, verify_mode, None, None)
}

fn verify_parent_dirs_no_symlink<'a>(
    abs_root: &Path,
    rel_paths: impl Iterator<Item = &'a str>,
) -> HashSet<PathBuf> {
    let mut checked: HashMap<PathBuf, bool> = HashMap::new();
    let mut valid_dirs: HashSet<PathBuf> = HashSet::new();
    for rel in rel_paths {
        let Some(abs_file) = rel_posix_to_abs(abs_root, rel) else {
            continue;
        };
        let Some(parent) = abs_file.parent() else {
            continue;
        };
        let is_ok = *checked
            .entry(parent.to_path_buf())
            .or_insert_with(|| ensure_no_symlink_on_path(abs_root, parent).is_ok());
        if is_ok {
            valid_dirs.insert(parent.to_path_buf());
        }
    }
    valid_dirs
}

fn verify_output_file_meta(
    abs_path: &Path,
    expected: &FileStamp,
    verify_mode: CacheVerifyMode,
) -> bool {
    let Ok(meta) = fs::symlink_metadata(abs_path) else {
        return false;
    };
    if !meta.file_type().is_file() {
        return false;
    }
    let (len, mtime_secs, mtime_nanos) = metadata_len_mtime(&meta);
    if len != expected.len {
        return false;
    }
    if verify_mode == CacheVerifyMode::StatThenHash
        && mtime_secs == expected.mtime_secs
        && mtime_nanos == expected.mtime_nanos
    {
        return true;
    }
    if expected.digest.kind != DigestKind::Blake3 {
        return false;
    }
    hash_bytes_fast_or_stream(abs_path, len).is_ok_and(|actual| actual == expected.digest.bytes)
}

fn verify_output_on_disk_fast(
    valid_dirs: &HashSet<PathBuf>,
    abs_path: &Path,
    expected: &FileStamp,
    verify_mode: CacheVerifyMode,
) -> bool {
    let Some(parent) = abs_path.parent() else {
        return false;
    };
    if !valid_dirs.contains(parent) {
        return false;
    }
    verify_output_file_meta(abs_path, expected, verify_mode)
}

fn verify_output_on_disk(
    abs_root: &Path,
    abs_path: &Path,
    expected: &FileStamp,
    verify_mode: CacheVerifyMode,
) -> bool {
    if ensure_no_symlink_on_path(abs_root, abs_path).is_err() {
        return false;
    }
    verify_output_file_meta(abs_path, expected, verify_mode)
}

fn normalize_rel_posix(path: &Path) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    for comp in path.components() {
        match comp {
            Component::Normal(seg) => parts.push(seg.to_string_lossy().into_owned()),
            Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => return None,
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("/"))
    }
}

fn rel_posix_to_abs(abs_root: &Path, rel_posix: &str) -> Option<PathBuf> {
    let rel_path = Path::new(rel_posix);
    let clean = normalize_rel_posix(rel_path)?;
    let mut out = abs_root.to_path_buf();
    for seg in clean.split('/') {
        out.push(seg);
    }
    Some(out)
}

fn hash_feed_str(h: &mut blake3::Hasher, tag: &str, value: &str) {
    h.update(tag.as_bytes());
    h.update(&[0]);
    h.update(&u64::try_from(value.len()).unwrap_or(0).to_le_bytes());
    h.update(value.as_bytes());
}

fn hash_feed_u64(h: &mut blake3::Hasher, tag: &str, value: u64) {
    h.update(tag.as_bytes());
    h.update(&[0]);
    h.update(&value.to_le_bytes());
}

fn compute_target_cache_key(
    output: &ConvertOutput,
    target_format: TargetFormat,
    prepared: &PreparedOutput,
    cwd: Option<&Path>,
) -> String {
    let mut h = blake3::Hasher::new();
    h.update(CACHE_MAGIC);
    let fmt_tag = match target_format {
        TargetFormat::Source => "source",
        TargetFormat::Metadata => "metadata",
    };
    hash_feed_str(&mut h, "target_format", fmt_tag);

    match output {
        ConvertOutput::Directory { .. } => {
            hash_feed_str(&mut h, "kind", "dir");
            let anchor = prepared
                .default_directory
                .as_deref()
                .map(|p| absolutize_with_cwd(p, cwd))
                .unwrap_or_default();
            hash_feed_str(&mut h, "root", &anchor.to_string_lossy());
        }
        ConvertOutput::Zip {
            output_dir,
            package_name,
            unique_dir,
        } => {
            hash_feed_str(&mut h, "kind", "zip");
            if let Some(dir) = output_dir {
                if *unique_dir == UniqueDirPolicy::Generate && package_name.is_none() {
                    hash_feed_str(&mut h, "zip_mode", "gen");
                    hash_feed_str(
                        &mut h,
                        "dir",
                        &absolutize_with_cwd(dir, cwd).to_string_lossy(),
                    );
                } else if let Some(pkg_path) = &prepared.package_path {
                    hash_feed_str(&mut h, "zip_mode", "exact");
                    hash_feed_str(
                        &mut h,
                        "zip_path",
                        &absolutize_with_cwd(pkg_path, cwd).to_string_lossy(),
                    );
                }
            } else {
                hash_feed_str(&mut h, "zip_mode", "mem");
                hash_feed_str(&mut h, "pkg_name", package_name.as_deref().unwrap_or(""));
                if let Some(proj) = &prepared.project_dir {
                    hash_feed_str(
                        &mut h,
                        "proj_dir",
                        &absolutize_with_cwd(proj, cwd).to_string_lossy(),
                    );
                }
            }
        }
        ConvertOutput::Merge { .. } => {
            hash_feed_str(&mut h, "kind", "merge");
        }
    }

    h.finalize().to_hex().to_string()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReplacementMode {
    Apply,
    Skip,
}

fn compute_env_hash(
    registry: &RegistryAccess,
    cs: &ComponentSet,
    target_format: TargetFormat,
    prepared: &PreparedOutput,
    replacement_mode: ReplacementMode,
    cwd: Option<&Path>,
) -> Result<[u8; 32], ConvertError> {
    let mut h = blake3::Hasher::new();
    h.update(CACHE_MAGIC);
    hash_feed_str(&mut h, "pkg_version", env!("CARGO_PKG_VERSION"));
    hash_feed_str(
        &mut h,
        "target_format",
        match target_format {
            TargetFormat::Source => "source",
            TargetFormat::Metadata => "metadata",
        },
    );
    hash_feed_str(&mut h, "api_version", &cs.resolved_api_version());
    hash_feed_str(
        &mut h,
        "manifest_purpose",
        &format!("{:?}", cs.manifest_purpose()),
    );
    hash_feed_str(&mut h, "full_name", cs.full_name().unwrap_or(""));
    hash_feed_str(
        &mut h,
        "write_manifests",
        if prepared.write_manifests { "1" } else { "0" },
    );
    hash_feed_str(&mut h, "is_zip", if prepared.is_zip { "1" } else { "0" });
    hash_feed_str(
        &mut h,
        "apply_replacements",
        match replacement_mode {
            ReplacementMode::Apply => "1",
            ReplacementMode::Skip => "0",
        },
    );

    let destructive = cs.destructive_member_keys();
    hash_feed_u64(
        &mut h,
        "destructive_len",
        u64::try_from(destructive.len()).unwrap_or(0),
    );
    for (kind, key) in destructive {
        hash_feed_str(&mut h, "destructive_kind", kind.filename());
        hash_feed_str(&mut h, "destructive_key", &key);
    }

    if registry.is_default() {
        hash_feed_str(&mut h, "registry", "default");
    } else {
        let serialized = serde_json::to_vec(registry.registry())
            .map_err(|err| io_invalid_data(PathBuf::from("<registry>"), &err.to_string()))?;
        hash_feed_str(&mut h, "registry", "custom");
        h.update(&serialized);
    }

    if let Some(proj_dir) = prepared
        .project_dir
        .as_deref()
        .or_else(|| cs.project_directory())
    {
        hash_feed_str(
            &mut h,
            "project_dir",
            &absolutize_with_cwd(proj_dir, cwd).to_string_lossy(),
        );
        if let Ok(Some((proj_file, _))) = SfdxProjectConfig::find_and_load(proj_dir)
            && let Ok(bytes) = fs::read(&proj_file)
        {
            hash_feed_str(&mut h, "sfdx_project_path", &proj_file.to_string_lossy());
            h.update(&bytes);
        }
    }

    if !prepared.is_zip
        && let Some(root_dest) = prepared.default_directory.as_deref()
    {
        let ignore_anchor = prepared.project_dir.as_deref().unwrap_or(root_dest);
        if let Some(ignore_path) = ForceIgnore::find_path(ignore_anchor)
            && let Ok(bytes) = fs::read(&ignore_path)
        {
            hash_feed_str(
                &mut h,
                "forceignore_path",
                &absolutize_with_cwd(&ignore_path, cwd).to_string_lossy(),
            );
            h.update(&bytes);
        }
    }

    let mut ignored: Vec<String> = cs
        .force_ignored_paths()
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    ignored.sort();
    hash_feed_u64(
        &mut h,
        "ignored_len",
        u64::try_from(ignored.len()).unwrap_or(0),
    );
    for p in ignored {
        hash_feed_str(&mut h, "ignored", &p);
    }

    Ok(*h.finalize().as_bytes())
}

// ---------------------------------------------------------------------------
// Work units, input discovery & parallel blake3 fingerprinting
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum UnitExecKind {
    MetadataStandalone,
    MetadataRecomp,
    MetadataFinalized,
    SourceAll,
}

#[derive(Debug, Clone)]
struct InputRef {
    canonical_key: String,
    tree_path: PathBuf,
    tree: TreeHandle,
    optional: bool,
}

struct WorkUnit {
    key: String,
    exec_kind: UnitExecKind,
    components: Vec<(SourceComponent, Vec<SourceComponent>)>,
    inputs: Vec<InputRef>,
    always_rebuild: bool,
    digest: [u8; 32],
}

#[derive(Debug, Clone)]
enum InputDigest {
    Present(FileStamp),
    NonFsHash([u8; 32]),
    Missing,
}

struct UniqueInputKey {
    canonical_key: String,
    tree_path: PathBuf,
    tree: TreeHandle,
}

fn is_default_dir_in_project_packages(
    default_dir: Option<&Path>,
    cs: &ComponentSet,
    cwd: Option<&Path>,
) -> bool {
    let Some(dir) = default_dir else {
        return false;
    };
    let search_from = cs.project_directory().unwrap_or(dir);
    let Ok(Some((proj_file, cfg))) = SfdxProjectConfig::find_and_load(search_from) else {
        return false;
    };
    let Some(proj_root) = proj_file.parent() else {
        return false;
    };
    let abs_dir = absolutize_with_cwd(dir, cwd);
    cfg.package_directories
        .iter()
        .any(|pkg| absolutize_with_cwd(&proj_root.join(&pkg.path), cwd) == abs_dir)
}

fn unit_key_and_kind(
    comp: &SourceComponent,
    target_format: TargetFormat,
) -> (String, UnitExecKind) {
    let root = comp.parent().unwrap_or(comp);
    let root_id = &root.metadata_type().id;
    let root_name = root.full_name();
    let strategy = effective_transformer_strategy(comp);

    match target_format {
        TargetFormat::Metadata => match strategy {
            TransformerStrategy::Decomposed | TransformerStrategy::NonDecomposed => (
                format!("recomp:{root_id}#{root_name}"),
                UnitExecKind::MetadataRecomp,
            ),
            TransformerStrategy::Profile => (
                format!("profile:{}", comp.full_name()),
                UnitExecKind::MetadataFinalized,
            ),
            TransformerStrategy::DecomposedLabels => (
                "declabels:CustomLabels".to_owned(),
                UnitExecKind::MetadataFinalized,
            ),
            TransformerStrategy::DecomposedPermissionSet => (
                format!("decps:{root_name}"),
                UnitExecKind::MetadataFinalized,
            ),
            TransformerStrategy::DecomposeExternalServiceRegistration => (
                format!("decesr:{}", comp.full_name()),
                UnitExecKind::MetadataFinalized,
            ),
            TransformerStrategy::Standard
            | TransformerStrategy::StaticResource
            | TransformerStrategy::UiBundle => (
                format!("comp:{}#{}", comp.metadata_type().id, comp.full_name()),
                UnitExecKind::MetadataStandalone,
            ),
        },
        TargetFormat::Source => match strategy {
            TransformerStrategy::NonDecomposed => {
                (format!("src_nondec:{root_id}"), UnitExecKind::SourceAll)
            }
            TransformerStrategy::Profile => (
                format!("profile:{}", comp.full_name()),
                UnitExecKind::SourceAll,
            ),
            TransformerStrategy::Decomposed => (
                format!("src_decomp:{root_id}#{root_name}"),
                UnitExecKind::SourceAll,
            ),
            TransformerStrategy::DecomposedLabels => (
                format!("src_declabels:{}", comp.full_name()),
                UnitExecKind::SourceAll,
            ),
            TransformerStrategy::DecomposedPermissionSet => (
                format!("src_decps:{root_id}#{root_name}"),
                UnitExecKind::SourceAll,
            ),
            TransformerStrategy::DecomposeExternalServiceRegistration => (
                format!("src_decesr:{}", comp.full_name()),
                UnitExecKind::SourceAll,
            ),
            TransformerStrategy::Standard
            | TransformerStrategy::StaticResource
            | TransformerStrategy::UiBundle => (
                format!("comp:{}#{}", comp.metadata_type().id, comp.full_name()),
                UnitExecKind::SourceAll,
            ),
        },
    }
}

fn canonical_input_key(tree: &TreeHandle, path: &Path, cwd: Option<&Path>) -> String {
    if let Some(fs_path) = tree.fs_path(path) {
        absolutize_with_cwd(&fs_path, cwd)
            .to_string_lossy()
            .into_owned()
    } else {
        format!("mem:{}", normalize_lexical(path).to_string_lossy())
    }
}

fn push_component_inputs(
    comp: &SourceComponent,
    target_format: TargetFormat,
    skip_walk_content: SkipWalkContent,
    cwd: Option<&Path>,
    out: &mut Vec<InputRef>,
) -> Result<(), ConvertError> {
    let tree = comp.tree();
    if let Some(xml) = comp.xml() {
        out.push(InputRef {
            canonical_key: canonical_input_key(tree, xml, cwd),
            tree_path: xml.to_path_buf(),
            tree: tree.clone(),
            optional: false,
        });
        if target_format == TargetFormat::Metadata
            && effective_transformer_strategy(comp)
                == TransformerStrategy::DecomposeExternalServiceRegistration
        {
            let yaml = xml.with_file_name(format!("{}.yaml", comp.full_name()));
            out.push(InputRef {
                canonical_key: canonical_input_key(tree, &yaml, cwd),
                tree_path: yaml,
                tree: tree.clone(),
                optional: true,
            });
        }
    }
    if skip_walk_content == SkipWalkContent::Walk && comp.content().is_some() {
        for file in comp.walk_content()? {
            out.push(InputRef {
                canonical_key: canonical_input_key(tree, &file, cwd),
                tree_path: file,
                tree: tree.clone(),
                optional: false,
            });
        }
    }
    if let Some(parent) = comp.parent()
        && let Some(pxml) = parent.xml()
    {
        out.push(InputRef {
            canonical_key: canonical_input_key(parent.tree(), pxml, cwd),
            tree_path: pxml.to_path_buf(),
            tree: parent.tree().clone(),
            optional: false,
        });
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SkipWalkContent {
    Walk,
    Skip,
}

fn hash_component_descriptor(h: &mut blake3::Hasher, comp: &SourceComponent) {
    hash_feed_str(h, "ctype", &comp.metadata_type().id);
    hash_feed_str(h, "cname", &comp.full_name());
    hash_feed_str(
        h,
        "cxml",
        comp.xml().map_or("", |p| p.to_str().unwrap_or("")),
    );
    hash_feed_str(
        h,
        "ccontent",
        comp.content().map_or("", |p| p.to_str().unwrap_or("")),
    );
    if let Some(parent) = comp.parent() {
        hash_feed_str(h, "ptype", &parent.metadata_type().id);
        hash_feed_str(h, "pname", &parent.full_name());
        hash_feed_str(
            h,
            "pxml",
            parent.xml().map_or("", |p| p.to_str().unwrap_or("")),
        );
    }
    hash_feed_u64(
        h,
        "repl_files",
        u64::try_from(comp.replacements.len()).unwrap_or(0),
    );
    for (path, rules) in &comp.replacements {
        hash_feed_str(h, "rpath", &path.to_string_lossy());
        hash_feed_u64(h, "rrules", u64::try_from(rules.len()).unwrap_or(0));
        for rule in rules {
            hash_feed_str(h, "rregex", rule.to_replace.as_str());
            hash_feed_str(h, "rwith", &rule.replace_with);
        }
    }
}

#[derive(Clone, Copy)]
struct IncrementalCtx<'a> {
    converter: &'a MetadataConverter,
    cs: &'a ComponentSet,
    target_format: TargetFormat,
    cache: &'a ConvertCache,
    target_key: &'a str,
    env_hash: [u8; 32],
    source_comps: &'a [SourceComponent],
    cwd: Option<&'a Path>,
}

type BuiltWorkUnits = (
    Vec<WorkUnit>,
    BTreeMap<String, FileStamp>,
    HashMap<PathBuf, ContentDigest>,
);

struct FingerprintedInputs {
    digest_by_key: HashMap<String, InputDigest>,
    input_stamps: BTreeMap<String, FileStamp>,
    known_input_hashes: HashMap<PathBuf, ContentDigest>,
}

fn resolve_pre_children(
    active: &[&SourceComponent],
    target_format: TargetFormat,
) -> Result<Vec<Vec<SourceComponent>>, ConvertError> {
    if target_format != TargetFormat::Metadata {
        return Ok(vec![Vec::new(); active.len()]);
    }
    let resolve_one = |comp: &&SourceComponent| -> Result<Vec<SourceComponent>, ConvertError> {
        if matches!(
            effective_transformer_strategy(comp),
            TransformerStrategy::Decomposed | TransformerStrategy::NonDecomposed
        ) && comp.parent().is_none()
        {
            Ok(comp.children()?)
        } else {
            Ok(Vec::new())
        }
    };
    if active.len() >= 8 {
        io_install(|| {
            active
                .par_iter()
                .map(resolve_one)
                .collect::<Result<Vec<_>, _>>()
        })
    } else {
        active.iter().map(resolve_one).collect()
    }
}

fn fingerprint_unique_inputs(
    buckets: &IndexMap<String, WorkUnit>,
    prev_index: Option<&CacheIndex>,
    verify_mode: CacheVerifyMode,
    cwd: Option<&Path>,
) -> Result<FingerprintedInputs, ConvertError> {
    let mut unique_inputs: IndexMap<String, UniqueInputKey> = IndexMap::new();
    for unit in buckets.values() {
        for inp in &unit.inputs {
            unique_inputs
                .entry(inp.canonical_key.clone())
                .or_insert_with(|| UniqueInputKey {
                    canonical_key: inp.canonical_key.clone(),
                    tree_path: inp.tree_path.clone(),
                    tree: inp.tree.clone(),
                });
        }
    }

    let unique_list: Vec<UniqueInputKey> = unique_inputs.into_values().collect();
    let git_index = if verify_mode == CacheVerifyMode::StatThenHash {
        unique_list
            .iter()
            .find_map(|item| item.tree.fs_path(&item.tree_path))
            .map(|p| absolutize_with_cwd(&p, cwd))
            .and_then(|abs_p| GitIndexCache::discover_and_load(&abs_p))
    } else {
        None
    };

    let hash_one = |item: &UniqueInputKey| -> Result<(String, InputDigest), ConvertError> {
        if let Some(fs_path) = item.tree.fs_path(&item.tree_path) {
            let cached = prev_index.and_then(|idx| idx.input_stamps.get(&item.canonical_key));
            let digest = match stamp_regular_file_with_git(
                &fs_path,
                cached,
                verify_mode,
                git_index.as_ref(),
                cwd,
            )? {
                Some(stamp) => InputDigest::Present(stamp),
                None => InputDigest::Missing,
            };
            return Ok((item.canonical_key.clone(), digest));
        }
        if !item.tree.exists(&item.tree_path) {
            return Ok((item.canonical_key.clone(), InputDigest::Missing));
        }
        let bytes = item.tree.read_file(&item.tree_path)?;
        let digest = *blake3::hash(&bytes).as_bytes();
        Ok((item.canonical_key.clone(), InputDigest::NonFsHash(digest)))
    };

    let hashed_inputs: Vec<(String, InputDigest)> = if unique_list.len() >= 16 {
        io_install(|| {
            unique_list
                .par_iter()
                .map(hash_one)
                .collect::<Result<Vec<_>, _>>()
        })?
    } else {
        unique_list
            .iter()
            .map(hash_one)
            .collect::<Result<Vec<_>, _>>()?
    };

    let mut digest_by_key: HashMap<String, InputDigest> =
        HashMap::with_capacity(hashed_inputs.len());
    let mut input_stamps: BTreeMap<String, FileStamp> = BTreeMap::new();
    for (key, digest) in hashed_inputs {
        if let InputDigest::Present(stamp) = digest {
            input_stamps.insert(key.clone(), stamp);
        }
        digest_by_key.insert(key, digest);
    }

    let mut known_input_hashes: HashMap<PathBuf, ContentDigest> =
        HashMap::with_capacity(unique_list.len() * 2);
    for unit in buckets.values() {
        for inp in &unit.inputs {
            if let Some(InputDigest::Present(stamp)) = digest_by_key.get(&inp.canonical_key) {
                known_input_hashes.insert(inp.tree_path.clone(), stamp.digest);
                if let Some(fs_path) = inp.tree.fs_path(&inp.tree_path) {
                    known_input_hashes.insert(fs_path, stamp.digest);
                }
            }
        }
    }

    Ok(FingerprintedInputs {
        digest_by_key,
        input_stamps,
        known_input_hashes,
    })
}

fn compute_single_unit_digest(
    unit: &mut WorkUnit,
    env_hash: [u8; 32],
    digest_by_key: &HashMap<String, InputDigest>,
) {
    let mut h = blake3::Hasher::new();
    h.update(&env_hash);
    hash_feed_str(&mut h, "unit_key", &unit.key);
    hash_feed_u64(
        &mut h,
        "comp_count",
        u64::try_from(unit.components.len()).unwrap_or(0),
    );
    for (comp, children) in &unit.components {
        hash_component_descriptor(&mut h, comp);
        hash_feed_u64(
            &mut h,
            "child_count",
            u64::try_from(children.len()).unwrap_or(0),
        );
        for child in children {
            hash_component_descriptor(&mut h, child);
        }
    }

    let mut sorted_inputs: Vec<(&str, bool, &InputDigest)> = unit
        .inputs
        .iter()
        .filter_map(|inp| {
            digest_by_key
                .get(&inp.canonical_key)
                .map(|d| (inp.canonical_key.as_str(), inp.optional, d))
        })
        .collect();
    sorted_inputs.sort_by(|a, b| a.0.cmp(b.0));
    sorted_inputs.dedup_by(|a, b| a.0 == b.0);
    for (path, optional, digest) in sorted_inputs {
        hash_feed_str(&mut h, "in_path", path);
        match digest {
            InputDigest::Present(stamp) => {
                h.update(&[1, stamp.digest.kind.to_u8()]);
                h.update(&stamp.digest.bytes);
            }
            InputDigest::NonFsHash(hash) => {
                h.update(&[2]);
                h.update(hash);
            }
            InputDigest::Missing => {
                h.update(&[if optional { 3 } else { 4 }]);
            }
        }
    }
    unit.digest = *h.finalize().as_bytes();
}

fn compute_unit_digests(
    units: &mut [WorkUnit],
    env_hash: [u8; 32],
    digest_by_key: &HashMap<String, InputDigest>,
) {
    if units.len() >= 64 {
        units
            .par_iter_mut()
            .for_each(|unit| compute_single_unit_digest(unit, env_hash, digest_by_key));
    } else {
        for unit in units {
            compute_single_unit_digest(unit, env_hash, digest_by_key);
        }
    }
}

struct CollectedComponentWork<'a> {
    key: String,
    exec_kind: UnitExecKind,
    always_rebuild: bool,
    inputs: Vec<InputRef>,
    comp: &'a SourceComponent,
    children: Vec<SourceComponent>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DefaultPackageStatus {
    PackageDir,
    Other,
}

fn collect_one_component_work<'a>(
    comp: &'a SourceComponent,
    children: Vec<SourceComponent>,
    target_format: TargetFormat,
    default_pkg_status: DefaultPackageStatus,
    cwd: Option<&Path>,
) -> Result<CollectedComponentWork<'a>, ConvertError> {
    let (key, exec_kind) = unit_key_and_kind(comp, target_format);
    let strategy = effective_transformer_strategy(comp);
    let always_rebuild = default_pkg_status == DefaultPackageStatus::PackageDir
        && strategy == TransformerStrategy::NonDecomposed;
    let skip_parent_walk = target_format == TargetFormat::Metadata
        && matches!(
            strategy,
            TransformerStrategy::Decomposed | TransformerStrategy::NonDecomposed
        )
        && comp.parent().is_none();
    let parent_walk = if skip_parent_walk {
        SkipWalkContent::Skip
    } else {
        SkipWalkContent::Walk
    };

    let mut inputs = Vec::new();
    push_component_inputs(comp, target_format, parent_walk, cwd, &mut inputs)?;
    for child in &children {
        push_component_inputs(
            child,
            target_format,
            SkipWalkContent::Skip,
            cwd,
            &mut inputs,
        )?;
    }
    Ok(CollectedComponentWork {
        key,
        exec_kind,
        always_rebuild,
        inputs,
        comp,
        children,
    })
}

fn build_work_units(
    ctx: &IncrementalCtx<'_>,
    prepared: &PreparedOutput,
    prev_index: Option<&CacheIndex>,
) -> Result<BuiltWorkUnits, ConvertError> {
    let IncrementalCtx {
        cs,
        target_format,
        cache,
        env_hash,
        source_comps,
        cwd,
        ..
    } = *ctx;
    let active: Vec<&SourceComponent> = source_comps
        .iter()
        .filter(|c| !c.is_marked_for_delete())
        .collect();
    let pre_children = resolve_pre_children(&active, target_format)?;
    let default_pkg_status = if target_format == TargetFormat::Source
        && is_default_dir_in_project_packages(prepared.default_directory.as_deref(), cs, cwd)
    {
        DefaultPackageStatus::PackageDir
    } else {
        DefaultPackageStatus::Other
    };

    let pairs: Vec<(&SourceComponent, Vec<SourceComponent>)> =
        active.into_iter().zip(pre_children).collect();
    let collected: Vec<CollectedComponentWork<'_>> = if pairs.len() >= 16 {
        io_install(|| {
            pairs
                .into_par_iter()
                .map(|(comp, children)| {
                    collect_one_component_work(
                        comp,
                        children,
                        target_format,
                        default_pkg_status,
                        cwd,
                    )
                })
                .collect::<Result<Vec<_>, _>>()
        })?
    } else {
        pairs
            .into_iter()
            .map(|(comp, children)| {
                collect_one_component_work(comp, children, target_format, default_pkg_status, cwd)
            })
            .collect::<Result<Vec<_>, _>>()?
    };

    let mut buckets: IndexMap<String, WorkUnit> = IndexMap::new();
    for item in collected {
        let entry = buckets.entry(item.key.clone()).or_insert_with(|| WorkUnit {
            key: item.key,
            exec_kind: item.exec_kind,
            components: Vec::new(),
            inputs: Vec::new(),
            always_rebuild: item.always_rebuild,
            digest: [0u8; 32],
        });
        entry.always_rebuild |= item.always_rebuild;
        entry.inputs.extend(item.inputs);
        entry.components.push((item.comp.clone(), item.children));
    }

    let FingerprintedInputs {
        digest_by_key,
        input_stamps,
        known_input_hashes,
    } = fingerprint_unique_inputs(&buckets, prev_index, cache.verify_mode(), cwd)?;

    let mut units: Vec<WorkUnit> = buckets.into_values().collect();
    compute_unit_digests(&mut units, env_hash, &digest_by_key);
    Ok((units, input_stamps, known_input_hashes))
}

fn execute_unit(
    converter: &MetadataConverter,
    unit: &WorkUnit,
    target_format: TargetFormat,
    prepared: &PreparedOutput,
    parent_check: ParentDiskCheck,
) -> Result<Vec<WriterFormat>, ConvertError> {
    let registry = converter.registry();
    let mut context: Option<ConvertContext> = None;
    let mut items = match unit.exec_kind {
        UnitExecKind::MetadataStandalone => {
            let mut out = Vec::new();
            for (comp, _) in &unit.components {
                if let Some(res) = try_transform_to_metadata_parallel(comp) {
                    let infos = res?;
                    if !infos.is_empty() {
                        out.push(WriterFormat {
                            write_infos: infos,
                            resolve_scope: resolve_scope_for_component(comp),
                        });
                    }
                } else {
                    let ctx = context.get_or_insert_with(ConvertContext::new);
                    let infos = transform_to_metadata(ctx, registry, comp)?;
                    if !infos.is_empty() {
                        out.push(WriterFormat {
                            write_infos: infos,
                            resolve_scope: resolve_scope_for_component(comp),
                        });
                    }
                }
            }
            out
        }
        UnitExecKind::MetadataRecomp => {
            let ctx = context.get_or_insert_with(ConvertContext::new);
            for (comp, children) in &unit.components {
                ctx.recomposition
                    .add_resolved_component(comp.clone(), children.clone());
            }
            Vec::new()
        }
        UnitExecKind::MetadataFinalized => {
            let refs: Vec<&SourceComponent> = unit.components.iter().map(|(c, _)| c).collect();
            let ctx = context.get_or_insert_with(ConvertContext::new);
            converter.run_metadata_pipeline(&refs, ctx)?
        }
        UnitExecKind::SourceAll => {
            let refs: Vec<&SourceComponent> = unit.components.iter().map(|(c, _)| c).collect();
            let ctx = context.get_or_insert_with(ConvertContext::new);
            if prepared.merge_set.is_none() {
                converter.run_source_pipeline_with_check(&refs, prepared, ctx, parent_check)?
            } else {
                let mut out = Vec::new();
                for comp in refs {
                    let infos = transform_component_to_source_with_check(
                        registry,
                        ctx,
                        comp,
                        prepared,
                        parent_check,
                    )?;
                    if !infos.is_empty() {
                        out.push(WriterFormat {
                            write_infos: infos,
                            resolve_scope: resolve_scope_for_component(comp),
                        });
                    }
                }
                out
            }
        }
    };

    if (target_format == TargetFormat::Metadata || unit.exec_kind == UnitExecKind::SourceAll)
        && let Some(ctx) = context.as_mut()
    {
        items.extend(ctx.execute_finalizers(
            prepared.default_directory.as_deref(),
            registry,
            &TreeHandle::fs(),
        )?);
    }

    Ok(items)
}

fn execute_one_miss_unit(
    converter: &MetadataConverter,
    unit: &WorkUnit,
    target_format: TargetFormat,
    prepared: &PreparedOutput,
) -> Result<Vec<WriterFormat>, ConvertError> {
    let parent_check = if unit.always_rebuild {
        ParentDiskCheck::CheckDisk
    } else {
        prepared.parent_disk_check
    };
    execute_unit(converter, unit, target_format, prepared, parent_check)
}

// ---------------------------------------------------------------------------
// Incremental Directory execution
// ---------------------------------------------------------------------------

struct DirHitPlan {
    hits: Vec<bool>,
    cached_units: usize,
    rebuilt_units: usize,
    skipped_writes: usize,
    miss_indices: Vec<usize>,
    hit_output_rels: HashSet<String>,
    prev_all_unit_rels: HashSet<String>,
    prev_output_by_rel: HashMap<String, FileStamp>,
    pre_removed_abs: Vec<PathBuf>,
    pre_removed_rels: HashSet<String>,
}

fn run_miss_units(
    converter: &MetadataConverter,
    units: &[WorkUnit],
    miss_indices: &[usize],
    target_format: TargetFormat,
    prepared: &PreparedOutput,
) -> Result<Vec<(usize, Vec<WriterFormat>)>, ConvertError> {
    if miss_indices.len() >= 8 {
        miss_indices
            .par_iter()
            .map(|&idx| {
                let wf = execute_one_miss_unit(converter, &units[idx], target_format, prepared)?;
                Ok::<_, ConvertError>((idx, wf))
            })
            .collect()
    } else {
        miss_indices
            .iter()
            .map(|&idx| {
                let wf = execute_one_miss_unit(converter, &units[idx], target_format, prepared)?;
                Ok::<_, ConvertError>((idx, wf))
            })
            .collect()
    }
}

fn pre_delete_rel_outputs<'a>(
    abs_root: &Path,
    rels: impl IntoIterator<Item = &'a str>,
) -> Result<(Vec<PathBuf>, HashSet<String>), ConvertError> {
    let mut removed_abs: Vec<PathBuf> = Vec::new();
    let mut removed_rels: HashSet<String> = HashSet::new();
    for rel in rels {
        if let Some(abs_p) = rel_posix_to_abs(abs_root, rel)
            && ensure_no_symlink_on_path(abs_root, &abs_p).is_ok()
            && fs::symlink_metadata(&abs_p).is_ok_and(|m| m.file_type().is_file())
        {
            fs::remove_file(&abs_p).map_err(|source| ConvertError::Io {
                path: abs_p.clone(),
                source,
            })?;
            removed_abs.push(abs_p);
            removed_rels.insert(rel.to_owned());
        }
    }
    Ok((removed_abs, removed_rels))
}

fn classify_dir_units(
    units: &[WorkUnit],
    prev_index: Option<&CacheIndex>,
    env_hash: [u8; 32],
    abs_root: &Path,
    verify_mode: CacheVerifyMode,
) -> Result<DirHitPlan, ConvertError> {
    let prev_env_matches = prev_index.is_some_and(|idx| idx.env_hash == env_hash);
    let valid_dirs = if prev_env_matches {
        verify_parent_dirs_no_symlink(
            abs_root,
            units
                .iter()
                .filter(|unit| !unit.always_rebuild)
                .filter_map(|unit| {
                    let prev_unit = prev_index.and_then(|idx| idx.units.get(&unit.key))?;
                    (prev_unit.unit_digest == unit.digest).then_some(prev_unit)
                })
                .flat_map(|prev_unit| prev_unit.outputs.iter().map(|rec| rec.rel_path.as_str())),
        )
    } else {
        HashSet::new()
    };

    let check_hit = |unit: &WorkUnit| -> bool {
        if !prev_env_matches || unit.always_rebuild {
            return false;
        }
        let Some(prev_unit) = prev_index.and_then(|idx| idx.units.get(&unit.key)) else {
            return false;
        };
        if prev_unit.unit_digest != unit.digest {
            return false;
        }
        prev_unit.outputs.iter().all(|rec| {
            rel_posix_to_abs(abs_root, &rec.rel_path).is_some_and(|abs_p| {
                verify_output_on_disk_fast(&valid_dirs, &abs_p, &rec.stamp, verify_mode)
            })
        })
    };

    let hits: Vec<bool> = if units.len() >= 32 {
        io_install(|| units.par_iter().map(check_hit).collect())
    } else {
        units.iter().map(check_hit).collect()
    };

    let mut hit_output_rels: HashSet<String> = HashSet::new();
    let mut cached_units = 0usize;
    let mut rebuilt_units = 0usize;
    let mut skipped_writes = 0usize;
    let mut miss_indices: Vec<usize> = Vec::new();
    for (idx, (unit, is_hit)) in units.iter().zip(hits.iter().copied()).enumerate() {
        if is_hit && let Some(prev_unit) = prev_index.and_then(|i| i.units.get(&unit.key)) {
            cached_units += 1;
            skipped_writes += prev_unit.outputs.len();
            for rec in &prev_unit.outputs {
                hit_output_rels.insert(rec.rel_path.clone());
            }
        } else {
            rebuilt_units += 1;
            miss_indices.push(idx);
        }
    }

    let mut prev_all_unit_rels: HashSet<String> = HashSet::new();
    let mut prev_output_by_rel: HashMap<String, FileStamp> = HashMap::new();
    if let Some(prev) = prev_index {
        for prev_unit in prev.units.values() {
            for rec in &prev_unit.outputs {
                prev_all_unit_rels.insert(rec.rel_path.clone());
                if prev_env_matches && !hit_output_rels.contains(&rec.rel_path) {
                    prev_output_by_rel.insert(rec.rel_path.clone(), rec.stamp);
                }
            }
        }
    }

    // Only pre-delete outputs before running MISS units when env config changed or for
    // `always_rebuild` units (`NonDecompositionFinalizer` merges across disk outputs).
    let (pre_removed_abs, pre_removed_rels) = if prev_env_matches {
        let mut always_rels: Vec<&str> = Vec::new();
        for &idx in &miss_indices {
            if units[idx].always_rebuild
                && let Some(prev_unit) = prev_index.and_then(|i| i.units.get(&units[idx].key))
            {
                for rec in &prev_unit.outputs {
                    if !hit_output_rels.contains(&rec.rel_path) {
                        prev_output_by_rel.remove(&rec.rel_path);
                        always_rels.push(rec.rel_path.as_str());
                    }
                }
            }
        }
        pre_delete_rel_outputs(abs_root, always_rels)?
    } else {
        pre_delete_rel_outputs(abs_root, prev_all_unit_rels.iter().map(String::as_str))?
    };

    Ok(DirHitPlan {
        hits,
        cached_units,
        rebuilt_units,
        skipped_writes,
        miss_indices,
        hit_output_rels,
        prev_all_unit_rels,
        prev_output_by_rel,
        pre_removed_abs,
        pre_removed_rels,
    })
}

struct WriteSlotMeta {
    abs_out: PathBuf,
    rel_posix: Option<String>,
}

struct DiffCandidate<'a> {
    slot_id: usize,
    abs_out: PathBuf,
    rel_posix: String,
    prev_stamp: FileStamp,
    source: &'a WriteSource,
}

#[derive(Clone, Copy)]
enum DiffDecision {
    Skip(FileStamp),
    Write(Option<ContentDigest>),
}

struct DiffUnitRecord {
    unit_idx: usize,
    kept_rels: Vec<(PathBuf, String)>,
    skipped_outputs: Vec<OutputFileRecord>,
    skipped_converted: Vec<String>,
}

struct ContentDiffResult {
    filtered_items: Vec<WriterFormat>,
    precomputed_hashes: HashMap<PathBuf, ContentDigest>,
    unit_records: Vec<DiffUnitRecord>,
    skipped_writes: usize,
}

fn resolve_write_slot(abs_root: &Path, output: &Path) -> WriteSlotMeta {
    let has_parent_dir = output
        .components()
        .any(|c| matches!(c, Component::ParentDir));
    let abs_out = if output.is_absolute() {
        normalize_lexical(output)
    } else {
        normalize_lexical(&abs_root.join(output))
    };
    let rel_posix = if has_parent_dir {
        None
    } else {
        abs_out
            .strip_prefix(abs_root)
            .ok()
            .and_then(normalize_rel_posix)
    };
    WriteSlotMeta { abs_out, rel_posix }
}

fn collect_cold_miss_outputs(
    miss_outputs: Vec<(usize, Vec<WriterFormat>)>,
    abs_root: &Path,
) -> ContentDiffResult {
    let mut filtered_items = Vec::new();
    let mut unit_records = Vec::with_capacity(miss_outputs.len());
    for (unit_idx, wf_list) in miss_outputs {
        let mut kept_rels = Vec::new();
        for wf in wf_list {
            for info in &wf.write_infos {
                if matches!(info.action, WriteAction::Write(_)) {
                    let meta = resolve_write_slot(abs_root, &info.output);
                    if let Some(rel) = meta.rel_posix {
                        kept_rels.push((meta.abs_out, rel));
                    }
                }
            }
            filtered_items.push(wf);
        }
        unit_records.push(DiffUnitRecord {
            unit_idx,
            kept_rels,
            skipped_outputs: Vec::new(),
            skipped_converted: Vec::new(),
        });
    }
    ContentDiffResult {
        filtered_items,
        precomputed_hashes: HashMap::new(),
        unit_records,
        skipped_writes: 0,
    }
}

fn evaluate_diff_candidate(
    cand: &DiffCandidate<'_>,
    valid_dirs: &HashSet<PathBuf>,
    known_input_hashes: &HashMap<PathBuf, ContentDigest>,
    verify_mode: CacheVerifyMode,
) -> Result<(usize, DiffDecision), ConvertError> {
    if !verify_output_on_disk_fast(valid_dirs, &cand.abs_out, &cand.prev_stamp, verify_mode) {
        return Ok((cand.slot_id, DiffDecision::Write(None)));
    }
    let decision = match cand.source {
        WriteSource::Bytes(bytes) => {
            let digest = ContentDigest::blake3(bytes);
            if cand.prev_stamp.digest.kind == DigestKind::Blake3
                && u64::try_from(bytes.len()).ok() == Some(cand.prev_stamp.len)
                && digest == cand.prev_stamp.digest
            {
                DiffDecision::Skip(cand.prev_stamp)
            } else {
                DiffDecision::Write(Some(digest))
            }
        }
        WriteSource::TreeFile {
            tree,
            path,
            replacements,
        } => {
            if replacements.is_empty() {
                if let Some(&known) = known_input_hashes
                    .get(path)
                    .or_else(|| tree.fs_path(path).and_then(|p| known_input_hashes.get(&p)))
                {
                    if known == cand.prev_stamp.digest {
                        DiffDecision::Skip(cand.prev_stamp)
                    } else if known.kind == cand.prev_stamp.digest.kind {
                        DiffDecision::Write(Some(known))
                    } else {
                        DiffDecision::Write(None)
                    }
                } else if cand.prev_stamp.digest.kind == DigestKind::Blake3 {
                    let bytes = resolve_write_source(cand.source)?;
                    let digest = ContentDigest::blake3(&bytes);
                    if u64::try_from(bytes.len()).ok() == Some(cand.prev_stamp.len)
                        && digest == cand.prev_stamp.digest
                    {
                        DiffDecision::Skip(cand.prev_stamp)
                    } else {
                        DiffDecision::Write(Some(digest))
                    }
                } else {
                    DiffDecision::Write(None)
                }
            } else if cand.prev_stamp.digest.kind == DigestKind::Blake3 {
                let bytes = resolve_write_source(cand.source)?;
                let digest = ContentDigest::blake3(&bytes);
                if u64::try_from(bytes.len()).ok() == Some(cand.prev_stamp.len)
                    && digest == cand.prev_stamp.digest
                {
                    DiffDecision::Skip(cand.prev_stamp)
                } else {
                    DiffDecision::Write(Some(digest))
                }
            } else {
                DiffDecision::Write(None)
            }
        }
    };
    Ok((cand.slot_id, decision))
}

fn filter_diff_miss_items(
    miss_outputs: Vec<(usize, Vec<WriterFormat>)>,
    mut slot_metas: Vec<Option<WriteSlotMeta>>,
    decisions: &[DiffDecision],
) -> ContentDiffResult {
    let mut filtered_items = Vec::new();
    let mut precomputed_hashes = HashMap::new();
    let mut unit_records = Vec::with_capacity(miss_outputs.len());
    let mut skipped_writes = 0usize;
    let mut slot_cursor = 0usize;

    for (unit_idx, wf_list) in miss_outputs {
        let mut kept_rels = Vec::new();
        let mut skipped_outputs = Vec::new();
        let mut skipped_converted = Vec::new();

        for wf in wf_list {
            let scope = wf.resolve_scope;
            let mut first_write_seen = false;
            let mut kept_infos = Vec::with_capacity(wf.write_infos.len());

            for info in wf.write_infos {
                if !matches!(info.action, WriteAction::Write(_)) {
                    kept_infos.push(info);
                    continue;
                }
                let slot_id = slot_cursor;
                slot_cursor += 1;
                let resolve_eligible = scope == ResolveScope::AllFiles || !first_write_seen;
                first_write_seen = true;
                let Some(meta) = slot_metas[slot_id].take() else {
                    kept_infos.push(info);
                    continue;
                };
                match (decisions[slot_id], meta.rel_posix) {
                    (DiffDecision::Skip(prev_stamp), Some(rel_posix)) => {
                        skipped_writes += 1;
                        skipped_outputs.push(OutputFileRecord {
                            rel_path: rel_posix.clone(),
                            stamp: prev_stamp,
                        });
                        if resolve_eligible {
                            skipped_converted.push(rel_posix);
                        }
                    }
                    (DiffDecision::Write(digest_opt), rel_opt) => {
                        if let Some(digest) = digest_opt {
                            precomputed_hashes.insert(meta.abs_out.clone(), digest);
                        }
                        if let Some(rel_posix) = rel_opt {
                            kept_rels.push((meta.abs_out, rel_posix));
                        }
                        kept_infos.push(info);
                    }
                    (DiffDecision::Skip(_), None) => {
                        kept_infos.push(info);
                    }
                }
            }
            if !kept_infos.is_empty() {
                filtered_items.push(WriterFormat {
                    write_infos: kept_infos,
                    resolve_scope: scope,
                });
            }
        }
        unit_records.push(DiffUnitRecord {
            unit_idx,
            kept_rels,
            skipped_outputs,
            skipped_converted,
        });
    }

    ContentDiffResult {
        filtered_items,
        precomputed_hashes,
        unit_records,
        skipped_writes,
    }
}

fn content_diff_miss_outputs(
    miss_outputs: Vec<(usize, Vec<WriterFormat>)>,
    prev_output_by_rel: &HashMap<String, FileStamp>,
    known_input_hashes: &HashMap<PathBuf, ContentDigest>,
    abs_root: &Path,
    verify_mode: CacheVerifyMode,
) -> Result<ContentDiffResult, ConvertError> {
    if prev_output_by_rel.is_empty() {
        return Ok(collect_cold_miss_outputs(miss_outputs, abs_root));
    }

    let mut slot_metas: Vec<Option<WriteSlotMeta>> = Vec::new();
    let mut write_counts: HashMap<PathBuf, usize> = HashMap::new();
    for (_, wf_list) in &miss_outputs {
        for wf in wf_list {
            for info in &wf.write_infos {
                if matches!(info.action, WriteAction::Write(_)) {
                    let meta = resolve_write_slot(abs_root, &info.output);
                    *write_counts.entry(meta.abs_out.clone()).or_insert(0) += 1;
                    slot_metas.push(Some(meta));
                }
            }
        }
    }

    let mut candidates: Vec<DiffCandidate<'_>> = Vec::new();
    let mut slot_cursor = 0usize;
    for (_, wf_list) in &miss_outputs {
        for wf in wf_list {
            for info in &wf.write_infos {
                if let WriteAction::Write(source) = &info.action {
                    let slot_id = slot_cursor;
                    slot_cursor += 1;
                    if let Some(meta) = &slot_metas[slot_id]
                        && write_counts.get(&meta.abs_out) == Some(&1)
                        && let Some(ref rel_posix) = meta.rel_posix
                        && let Some(&prev_stamp) = prev_output_by_rel.get(rel_posix)
                    {
                        candidates.push(DiffCandidate {
                            slot_id,
                            abs_out: meta.abs_out.clone(),
                            rel_posix: rel_posix.clone(),
                            prev_stamp,
                            source,
                        });
                    }
                }
            }
        }
    }

    let valid_dirs =
        verify_parent_dirs_no_symlink(abs_root, candidates.iter().map(|c| c.rel_posix.as_str()));
    let evaluated: Vec<(usize, DiffDecision)> = if candidates.len() >= 8 {
        candidates
            .par_iter()
            .map(|c| evaluate_diff_candidate(c, &valid_dirs, known_input_hashes, verify_mode))
            .collect::<Result<Vec<_>, _>>()?
    } else {
        candidates
            .iter()
            .map(|c| evaluate_diff_candidate(c, &valid_dirs, known_input_hashes, verify_mode))
            .collect::<Result<Vec<_>, _>>()?
    };

    let mut decisions = vec![DiffDecision::Write(None); slot_metas.len()];
    for (slot_id, dec) in evaluated {
        decisions[slot_id] = dec;
    }

    Ok(filter_diff_miss_items(miss_outputs, slot_metas, &decisions))
}

fn remove_stale_dir_outputs(
    abs_root: &Path,
    plan: &DirHitPlan,
    new_all_unit_rels: &HashSet<String>,
) -> Result<usize, ConvertError> {
    let mut removed_stale_files = 0usize;
    let mut prune_dirs: Vec<PathBuf> = Vec::new();
    for old_rel in &plan.prev_all_unit_rels {
        if !new_all_unit_rels.contains(old_rel) {
            removed_stale_files += 1;
            if let Some(abs_p) = rel_posix_to_abs(abs_root, old_rel) {
                if !plan.pre_removed_rels.contains(old_rel)
                    && ensure_no_symlink_on_path(abs_root, &abs_p).is_ok()
                    && fs::symlink_metadata(&abs_p).is_ok_and(|m| m.file_type().is_file())
                {
                    fs::remove_file(&abs_p).map_err(|source| ConvertError::Io {
                        path: abs_p.clone(),
                        source,
                    })?;
                }
                if let Some(parent) = abs_p.parent() {
                    prune_dirs.push(parent.to_path_buf());
                }
            }
        }
    }
    for abs_p in &plan.pre_removed_abs {
        if !abs_p.exists()
            && let Some(parent) = abs_p.parent()
        {
            prune_dirs.push(parent.to_path_buf());
        }
    }
    prune_empty_ancestors(abs_root, prune_dirs);
    Ok(removed_stale_files)
}

fn record_dir_miss_and_prune_stale(
    units: &[WorkUnit],
    plan: &DirHitPlan,
    prev_index: Option<&CacheIndex>,
    unit_records: Vec<DiffUnitRecord>,
    written_stamps: &HashMap<PathBuf, WrittenFileStamp>,
    converted_paths: &[PathBuf],
    abs_root: &Path,
) -> Result<(BTreeMap<String, UnitCacheEntry>, usize), ConvertError> {
    let converted_set: HashSet<&Path> = converted_paths.iter().map(PathBuf::as_path).collect();
    let mut new_units: BTreeMap<String, UnitCacheEntry> = BTreeMap::new();
    let mut new_all_unit_rels: HashSet<String> = plan.hit_output_rels.clone();

    for (idx, is_hit) in plan.hits.iter().copied().enumerate() {
        if is_hit && let Some(prev_unit) = prev_index.and_then(|i| i.units.get(&units[idx].key)) {
            new_units.insert(units[idx].key.clone(), prev_unit.clone());
        }
    }

    for rec in unit_records {
        let mut unit_outputs: Vec<OutputFileRecord> = Vec::new();
        let mut seen_unit_rels: HashSet<String> = HashSet::new();
        let mut converted_rels: Vec<String> = Vec::new();
        let mut seen_converted_rels: HashSet<String> = HashSet::new();

        for out_rec in rec.skipped_outputs {
            if seen_unit_rels.insert(out_rec.rel_path.clone()) {
                new_all_unit_rels.insert(out_rec.rel_path.clone());
                unit_outputs.push(out_rec);
            }
        }
        for rel in rec.skipped_converted {
            if seen_converted_rels.insert(rel.clone()) {
                converted_rels.push(rel);
            }
        }
        for (abs_out, rel_posix) in rec.kept_rels {
            let Some(stamp) = written_stamps.get(&abs_out) else {
                continue;
            };
            if seen_unit_rels.insert(rel_posix.clone()) {
                new_all_unit_rels.insert(rel_posix.clone());
                unit_outputs.push(OutputFileRecord {
                    rel_path: rel_posix.clone(),
                    stamp: FileStamp {
                        digest: stamp.digest,
                        len: stamp.len,
                        mtime_secs: stamp.mtime_secs,
                        mtime_nanos: stamp.mtime_nanos,
                    },
                });
            }
            if converted_set.contains(abs_out.as_path())
                && seen_converted_rels.insert(rel_posix.clone())
            {
                converted_rels.push(rel_posix);
            }
        }

        new_units.insert(
            units[rec.unit_idx].key.clone(),
            UnitCacheEntry {
                unit_digest: units[rec.unit_idx].digest,
                outputs: unit_outputs,
                converted_rel_paths: converted_rels,
            },
        );
    }

    let removed_stale_files = remove_stale_dir_outputs(abs_root, plan, &new_all_unit_rels)?;
    Ok((new_units, removed_stale_files))
}

struct ManifestSyncResult {
    manifests: BTreeMap<String, OutputFileRecord>,
    skipped: usize,
    written: usize,
    stale: usize,
}

fn sync_dir_manifests(
    cs: &ComponentSet,
    target_format: TargetFormat,
    prepared: &PreparedOutput,
    abs_root: &Path,
    prev_index: Option<&CacheIndex>,
    verify_mode: CacheVerifyMode,
) -> Result<ManifestSyncResult, ConvertError> {
    let mut new_manifests: BTreeMap<String, OutputFileRecord> = BTreeMap::new();
    let mut skipped_writes = 0usize;
    let mut written_files = 0usize;
    let mut removed_stale_files = 0usize;

    if !prepared.write_manifests || target_format != TargetFormat::Metadata {
        return Ok(ManifestSyncResult {
            manifests: new_manifests,
            skipped: skipped_writes,
            written: written_files,
            stale: removed_stale_files,
        });
    }

    let mut desired: Vec<(&'static str, Vec<u8>)> = Vec::new();
    desired.push((PACKAGE_XML_FILE, cs.render_package_xml(None)?.into_bytes()));
    for kind in cs.destructive_changes_kinds() {
        desired.push((
            kind.filename(),
            cs.render_package_xml(Some(kind))?.into_bytes(),
        ));
    }

    for (name, bytes) in desired {
        let digest = *blake3::hash(&bytes).as_bytes();
        let abs_manifest = abs_root.join(name);
        let prev_manifest = prev_index.and_then(|i| i.manifests.get(name));
        let can_skip = prev_manifest.is_some_and(|rec| {
            rec.stamp.digest == ContentDigest::from_blake3_bytes(digest)
                && verify_output_on_disk(abs_root, &abs_manifest, &rec.stamp, verify_mode)
        });

        if can_skip && let Some(prev_rec) = prev_manifest {
            skipped_writes += 1;
            new_manifests.insert(name.to_owned(), prev_rec.clone());
        } else {
            ensure_no_symlink_on_path(abs_root, &abs_manifest)?;
            fs::write(&abs_manifest, &bytes).map_err(|source| ConvertError::Io {
                path: abs_manifest.clone(),
                source,
            })?;
            let meta = fs::metadata(&abs_manifest).map_err(|source| ConvertError::Io {
                path: abs_manifest.clone(),
                source,
            })?;
            let (len, mtime_secs, mtime_nanos) = metadata_len_mtime(&meta);
            written_files += 1;
            new_manifests.insert(
                name.to_owned(),
                OutputFileRecord {
                    rel_path: name.to_owned(),
                    stamp: FileStamp {
                        digest: ContentDigest::from_blake3_bytes(digest),
                        len,
                        mtime_secs,
                        mtime_nanos,
                    },
                },
            );
        }
    }

    if let Some(prev) = prev_index {
        for old_name in prev.manifests.keys() {
            if !new_manifests.contains_key(old_name) {
                let abs_old = abs_root.join(old_name);
                if ensure_no_symlink_on_path(abs_root, &abs_old).is_ok()
                    && fs::symlink_metadata(&abs_old).is_ok_and(|m| m.file_type().is_file())
                {
                    fs::remove_file(&abs_old).map_err(|source| ConvertError::Io {
                        path: abs_old,
                        source,
                    })?;
                    removed_stale_files += 1;
                }
            }
        }
    }

    Ok(ManifestSyncResult {
        manifests: new_manifests,
        skipped: skipped_writes,
        written: written_files,
        stale: removed_stale_files,
    })
}

fn resolve_dir_converted(
    registry: &RegistryAccess,
    policy: IncrementalResolvePolicy,
    units: &[WorkUnit],
    new_units: &BTreeMap<String, UnitCacheEntry>,
    writer: &StandardWriter,
    abs_root: &Path,
    seed_ignore: (Option<PathBuf>, ForceIgnore),
) -> Result<Vec<SourceComponent>, ConvertError> {
    match policy {
        IncrementalResolvePolicy::RebuiltOnly => {
            if writer.converted_paths.is_empty() && writer.deleted_paths.is_empty() {
                Ok(Vec::new())
            } else {
                resolve_converted_paths_parallel(registry, &writer.converted_paths, seed_ignore)
            }
        }
        IncrementalResolvePolicy::All => {
            let mut all_paths: Vec<PathBuf> = Vec::new();
            let mut seen: HashSet<PathBuf> = HashSet::new();
            for unit in units {
                if let Some(entry) = new_units.get(&unit.key) {
                    for rel in &entry.converted_rel_paths {
                        if let Some(abs_p) = rel_posix_to_abs(abs_root, rel)
                            && seen.insert(abs_p.clone())
                        {
                            all_paths.push(abs_p);
                        }
                    }
                }
            }
            for p in &writer.converted_paths {
                if seen.insert(p.clone()) {
                    all_paths.push(p.clone());
                }
            }
            if all_paths.is_empty() {
                Ok(Vec::new())
            } else {
                resolve_converted_paths_parallel(registry, &all_paths, seed_ignore)
            }
        }
    }
}

fn load_anchor_force_ignore(
    prepared: &PreparedOutput,
    root_dest: &Path,
) -> Result<(Option<PathBuf>, ForceIgnore), ConvertError> {
    let ignore_anchor = prepared.project_dir.as_deref().unwrap_or(root_dest);
    let anchor_ignore_path = ForceIgnore::find_path(ignore_anchor);
    let force_ignore = match &anchor_ignore_path {
        Some(path) => ForceIgnore::from_file(path)?,
        None => ForceIgnore::empty(),
    };
    Ok((anchor_ignore_path, force_ignore))
}

struct DirWriteOutcome {
    writer: StandardWriter,
    unit_records: Vec<DiffUnitRecord>,
    written_files: usize,
    written_stamps: HashMap<PathBuf, WrittenFileStamp>,
    diff_skipped_writes: usize,
    anchor_ignore_path: Option<PathBuf>,
    force_ignore: ForceIgnore,
}

fn diff_and_write_dir_miss(
    prepared: &PreparedOutput,
    root_dest: &Path,
    abs_root: &Path,
    plan: &DirHitPlan,
    miss_outputs: Vec<(usize, Vec<WriterFormat>)>,
    known_input_hashes: HashMap<PathBuf, ContentDigest>,
    verify_mode: CacheVerifyMode,
) -> Result<DirWriteOutcome, ConvertError> {
    let (anchor_ignore_path, force_ignore) = load_anchor_force_ignore(prepared, root_dest)?;
    let diff = content_diff_miss_outputs(
        miss_outputs,
        &plan.prev_output_by_rel,
        &known_input_hashes,
        abs_root,
        verify_mode,
    )?;
    let mut merged_hashes = known_input_hashes;
    merged_hashes.extend(diff.precomputed_hashes);

    let mut writer = StandardWriter::new(root_dest, prepared.project_dir.as_deref());
    let written_stamps =
        writer.write_all_tracked(&diff.filtered_items, &force_ignore, &merged_hashes)?;
    let written_files = written_stamps.len();
    Ok(DirWriteOutcome {
        writer,
        unit_records: diff.unit_records,
        written_files,
        written_stamps,
        diff_skipped_writes: diff.skipped_writes,
        anchor_ignore_path,
        force_ignore,
    })
}

fn run_incremental_directory(
    ctx: &IncrementalCtx<'_>,
    mut prepared: PreparedOutput,
) -> Result<ConvertResult, ConvertError> {
    let IncrementalCtx {
        converter,
        cs,
        target_format,
        cache,
        target_key,
        env_hash,
        cwd,
        ..
    } = *ctx;
    let root_dest = prepared
        .default_directory
        .clone()
        .ok_or_else(|| ConvertError::Io {
            path: PathBuf::new(),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "missing output directory",
            ),
        })?;
    let abs_root = absolutize_with_cwd(&root_dest, cwd);
    let index_path = cache.cache_dir().join(format!("{target_key}.sfsfc"));
    let prev_index = CacheIndex::load_from(&index_path);

    let (units, new_input_stamps, known_input_hashes) =
        build_work_units(ctx, &prepared, prev_index.as_ref())?;
    let plan = classify_dir_units(
        &units,
        prev_index.as_ref(),
        env_hash,
        &abs_root,
        cache.verify_mode(),
    )?;

    prepared.parent_disk_check = ParentDiskCheck::AlwaysEmit;
    let miss_outputs = run_miss_units(
        converter,
        &units,
        &plan.miss_indices,
        target_format,
        &prepared,
    )?;
    let out = diff_and_write_dir_miss(
        &prepared,
        &root_dest,
        &abs_root,
        &plan,
        miss_outputs,
        known_input_hashes,
        cache.verify_mode(),
    )?;

    let (new_units, stale_unit_files) = record_dir_miss_and_prune_stale(
        &units,
        &plan,
        prev_index.as_ref(),
        out.unit_records,
        &out.written_stamps,
        &out.writer.converted_paths,
        &abs_root,
    )?;
    let manifest_sync = sync_dir_manifests(
        cs,
        target_format,
        &prepared,
        &abs_root,
        prev_index.as_ref(),
        cache.verify_mode(),
    )?;
    let resolved_converted = resolve_dir_converted(
        converter.registry(),
        cache.resolve_policy(),
        &units,
        &new_units,
        &out.writer,
        &abs_root,
        (out.anchor_ignore_path, out.force_ignore),
    )?;

    CacheIndex {
        env_hash,
        archive_stamp: None,
        manifests: manifest_sync.manifests,
        units: new_units,
        input_stamps: new_input_stamps,
    }
    .save_to(&index_path)?;

    Ok(ConvertResult {
        package_path: prepared.package_path,
        zip_bytes: None,
        zip_file_count: 0,
        converted: resolved_converted,
        deleted: out.writer.deleted_paths,
        incremental: Some(IncrementalStats {
            total_units: units.len(),
            cached_units: plan.cached_units,
            rebuilt_units: plan.rebuilt_units,
            skipped_writes: plan.skipped_writes + out.diff_skipped_writes + manifest_sync.skipped,
            written_files: out.written_files + manifest_sync.written,
            removed_stale_files: stale_unit_files + manifest_sync.stale,
        }),
    })
}

fn prune_empty_ancestors(abs_root: &Path, mut dirs: Vec<PathBuf>) {
    dirs.sort_by_key(|b| std::cmp::Reverse(b.components().count()));
    dirs.dedup();
    for start_dir in dirs {
        let mut cur = start_dir.as_path();
        while cur != abs_root && cur.starts_with(abs_root) {
            if ensure_no_symlink_on_path(abs_root, cur).is_err() {
                break;
            }
            match fs::remove_dir(cur) {
                Ok(()) => {
                    cur = match cur.parent() {
                        Some(p) => p,
                        None => break,
                    };
                }
                Err(_) => break,
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Incremental ZIP execution
// ---------------------------------------------------------------------------

enum ZipEntrySource {
    CopyFromPrev,
    FreshBytes(Vec<u8>),
    Prepared(PreparedZipFile),
}

fn backing_zip_path(
    cache: &ConvertCache,
    target_key: &str,
    output: &ConvertOutput,
    prepared: &PreparedOutput,
) -> PathBuf {
    match (output, &prepared.package_path) {
        (
            ConvertOutput::Zip {
                package_name: None,
                unique_dir: UniqueDirPolicy::Generate,
                ..
            },
            _,
        )
        | (_, None) => cache.cache_dir().join(format!("{target_key}.archive.zip")),
        (_, Some(pkg_zip)) => pkg_zip.clone(),
    }
}

struct DesiredZipManifest {
    name: String,
    bytes: Vec<u8>,
    hash: [u8; 32],
    unchanged: bool,
}

type ZipUnitEntryMap = HashMap<usize, Vec<(String, Vec<u8>, [u8; 32])>>;

struct ZipHitPlan {
    hits: Vec<bool>,
    cached_units: usize,
    rebuilt_units: usize,
    hit_output_count: usize,
    miss_indices: Vec<usize>,
    desired_manifests: Vec<DesiredZipManifest>,
    prev_all_entries: HashSet<String>,
    is_noop: bool,
}

struct BuiltZipPlan {
    entries: IndexMap<String, ZipEntrySource>,
    manifests: BTreeMap<String, OutputFileRecord>,
    units: BTreeMap<String, UnitCacheEntry>,
    skipped_writes: usize,
    written_files: usize,
    current_entry_names: HashSet<String>,
}

fn classify_zip_units(
    units: &[WorkUnit],
    cs: &ComponentSet,
    target_format: TargetFormat,
    prepared: &PreparedOutput,
    prev_index: Option<&CacheIndex>,
    prev_entry_names: Option<&HashSet<String>>,
) -> Result<ZipHitPlan, ConvertError> {
    let archive_ready = prev_entry_names.is_some();
    let empty_set = HashSet::new();
    let entry_names = prev_entry_names.unwrap_or(&empty_set);

    let mut hits: Vec<bool> = Vec::with_capacity(units.len());
    let mut cached_units = 0usize;
    let mut rebuilt_units = 0usize;
    let mut miss_indices: Vec<usize> = Vec::new();
    let mut hit_output_count = 0usize;

    for (idx, unit) in units.iter().enumerate() {
        let is_hit = archive_ready
            && !unit.always_rebuild
            && prev_index
                .and_then(|i| i.units.get(&unit.key))
                .is_some_and(|prev_unit| {
                    prev_unit.unit_digest == unit.digest
                        && prev_unit
                            .outputs
                            .iter()
                            .all(|rec| entry_names.contains(&rec.rel_path))
                });
        if is_hit && let Some(prev_unit) = prev_index.and_then(|i| i.units.get(&unit.key)) {
            hits.push(true);
            cached_units += 1;
            hit_output_count += prev_unit.outputs.len();
        } else {
            hits.push(false);
            rebuilt_units += 1;
            miss_indices.push(idx);
        }
    }

    let mut desired_manifests: Vec<DesiredZipManifest> = Vec::new();
    if prepared.write_manifests && target_format == TargetFormat::Metadata {
        let mut raw_manifests = Vec::new();
        raw_manifests.push((
            PACKAGE_XML_FILE.to_owned(),
            cs.render_package_xml(None)?.into_bytes(),
        ));
        for kind in cs.destructive_changes_kinds() {
            raw_manifests.push((
                kind.filename().to_owned(),
                cs.render_package_xml(Some(kind))?.into_bytes(),
            ));
        }
        for (name, bytes) in raw_manifests {
            let hash = *blake3::hash(&bytes).as_bytes();
            let unchanged = archive_ready
                && entry_names.contains(&name)
                && prev_index
                    .and_then(|i| i.manifests.get(&name))
                    .is_some_and(|rec| rec.stamp.digest == ContentDigest::from_blake3_bytes(hash));
            desired_manifests.push(DesiredZipManifest {
                name,
                bytes,
                hash,
                unchanged,
            });
        }
    }

    let mut prev_all_entries: HashSet<String> = HashSet::new();
    if let Some(prev) = prev_index {
        for name in prev.manifests.keys() {
            prev_all_entries.insert(name.clone());
        }
        for unit in prev.units.values() {
            for rec in &unit.outputs {
                prev_all_entries.insert(rec.rel_path.clone());
            }
        }
    }

    let prev_hit_entries: HashSet<String> = units
        .iter()
        .zip(hits.iter().copied())
        .filter(|(_, h)| *h)
        .filter_map(|(u, _)| prev_index.and_then(|i| i.units.get(&u.key)))
        .flat_map(|e| e.outputs.iter().map(|r| r.rel_path.clone()))
        .collect();
    let manifests_unchanged = archive_ready
        && desired_manifests.iter().all(|m| m.unchanged)
        && prev_index.is_some_and(|prev| prev.manifests.len() == desired_manifests.len());
    let no_deleted_units = prev_all_entries
        .difference(&prev_hit_entries)
        .all(|p| desired_manifests.iter().any(|m| &m.name == p));

    Ok(ZipHitPlan {
        hits,
        cached_units,
        rebuilt_units,
        hit_output_count,
        miss_indices,
        desired_manifests,
        prev_all_entries,
        is_noop: archive_ready && rebuilt_units == 0 && manifests_unchanged && no_deleted_units,
    })
}

fn resolve_miss_zip_entries(
    miss_outputs: &[(usize, Vec<WriterFormat>)],
) -> Result<ZipUnitEntryMap, ConvertError> {
    let mut pending_miss: Vec<(usize, String, WriteSource)> = Vec::new();
    for (unit_idx, wf_list) in miss_outputs {
        for wf in wf_list {
            for info in &wf.write_infos {
                if let WriteAction::Write(source) = &info.action {
                    let posix = info.output.to_string_lossy().replace('\\', "/");
                    let clean = posix.trim_start_matches('/').to_owned();
                    pending_miss.push((*unit_idx, clean, source.clone()));
                }
            }
        }
    }

    let resolve_one = |(unit_idx, name, source): &(usize, String, WriteSource)| {
        let bytes = resolve_write_source(source)?;
        let hash = *blake3::hash(&bytes).as_bytes();
        Ok::<_, ConvertError>((*unit_idx, name.clone(), bytes, hash))
    };

    let resolved_miss: Vec<(usize, String, Vec<u8>, [u8; 32])> = if pending_miss.len() >= 16 {
        io_install(|| {
            pending_miss
                .par_iter()
                .map(resolve_one)
                .collect::<Result<Vec<_>, _>>()
        })?
    } else {
        pending_miss
            .iter()
            .map(resolve_one)
            .collect::<Result<Vec<_>, _>>()?
    };

    let mut miss_entries_by_unit: ZipUnitEntryMap = HashMap::new();
    for (unit_idx, name, bytes, hash) in resolved_miss {
        miss_entries_by_unit
            .entry(unit_idx)
            .or_default()
            .push((name, bytes, hash));
    }
    Ok(miss_entries_by_unit)
}

fn compress_fresh_zip_entries(
    plan: &mut IndexMap<String, ZipEntrySource>,
) -> Result<(), ConvertError> {
    let options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .compression_level(Some(3));
    let fresh_items: Vec<(String, Vec<u8>)> = plan
        .iter()
        .filter_map(|(name, src)| match src {
            ZipEntrySource::FreshBytes(bytes) => Some((name.clone(), bytes.clone())),
            _ => None,
        })
        .collect();
    if fresh_items.is_empty() {
        return Ok(());
    }
    let prepare_one =
        |(name, data): (String, Vec<u8>)| -> Result<(String, PreparedZipFile), ConvertError> {
            let prep = PreparedZipFile::from_data(name.clone(), options, &data)?;
            Ok((name, prep))
        };
    let prepared_list: Vec<(String, PreparedZipFile)> = if fresh_items.len() >= 16 {
        fresh_items
            .into_par_iter()
            .map(prepare_one)
            .collect::<Result<Vec<_>, _>>()?
    } else {
        fresh_items
            .into_iter()
            .map(prepare_one)
            .collect::<Result<Vec<_>, _>>()?
    };
    for (name, prep) in prepared_list {
        plan.insert(name, ZipEntrySource::Prepared(prep));
    }
    Ok(())
}

fn build_zip_plan(
    units: &[WorkUnit],
    hits: &[bool],
    prev_index: Option<&CacheIndex>,
    desired_manifests: Vec<DesiredZipManifest>,
    mut miss_entries_by_unit: ZipUnitEntryMap,
) -> Result<BuiltZipPlan, ConvertError> {
    let mut plan: IndexMap<String, ZipEntrySource> = IndexMap::new();
    let mut new_manifests: BTreeMap<String, OutputFileRecord> = BTreeMap::new();
    let mut new_units: BTreeMap<String, UnitCacheEntry> = BTreeMap::new();
    let mut skipped_writes = 0usize;
    let mut written_files = 0usize;
    let mut current_entry_names: HashSet<String> = HashSet::new();

    for manifest in desired_manifests {
        let len = u64::try_from(manifest.bytes.len()).unwrap_or(0);
        current_entry_names.insert(manifest.name.clone());
        new_manifests.insert(
            manifest.name.clone(),
            OutputFileRecord {
                rel_path: manifest.name.clone(),
                stamp: FileStamp {
                    digest: ContentDigest::from_blake3_bytes(manifest.hash),
                    len,
                    mtime_secs: 0,
                    mtime_nanos: 0,
                },
            },
        );
        if manifest.unchanged {
            skipped_writes += 1;
            plan.insert(manifest.name, ZipEntrySource::CopyFromPrev);
        } else {
            written_files += 1;
            plan.insert(manifest.name, ZipEntrySource::FreshBytes(manifest.bytes));
        }
    }

    for (idx, unit) in units.iter().enumerate() {
        if hits[idx]
            && let Some(prev_unit) = prev_index.and_then(|i| i.units.get(&unit.key))
        {
            for rec in &prev_unit.outputs {
                skipped_writes += 1;
                current_entry_names.insert(rec.rel_path.clone());
                plan.insert(rec.rel_path.clone(), ZipEntrySource::CopyFromPrev);
            }
            new_units.insert(unit.key.clone(), prev_unit.clone());
        } else {
            let entries = miss_entries_by_unit.remove(&idx).unwrap_or_default();
            let mut records = Vec::with_capacity(entries.len());
            for (name, bytes, hash) in entries {
                let len = u64::try_from(bytes.len()).unwrap_or(0);
                written_files += 1;
                current_entry_names.insert(name.clone());
                records.push(OutputFileRecord {
                    rel_path: name.clone(),
                    stamp: FileStamp {
                        digest: ContentDigest::from_blake3_bytes(hash),
                        len,
                        mtime_secs: 0,
                        mtime_nanos: 0,
                    },
                });
                plan.insert(name, ZipEntrySource::FreshBytes(bytes));
            }
            new_units.insert(
                unit.key.clone(),
                UnitCacheEntry {
                    unit_digest: unit.digest,
                    outputs: records,
                    converted_rel_paths: Vec::new(),
                },
            );
        }
    }

    compress_fresh_zip_entries(&mut plan)?;

    Ok(BuiltZipPlan {
        entries: plan,
        manifests: new_manifests,
        units: new_units,
        skipped_writes,
        written_files,
        current_entry_names,
    })
}

fn assemble_zip_bytes(
    plan: IndexMap<String, ZipEntrySource>,
    mut prev_archive_opt: Option<ZipArchive<Cursor<&[u8]>>>,
    backing_path: &Path,
) -> Result<(Vec<u8>, usize), ConvertError> {
    let mut dir_entries: BTreeSet<String> = BTreeSet::new();
    for name in plan.keys() {
        let parts: Vec<&str> = name.split('/').collect();
        for i in 1..parts.len() {
            dir_entries.insert(format!("{}/", parts[..i].join("/")));
        }
    }
    let total_zip_entries = dir_entries.len() + plan.len();

    let mut out_buf = Cursor::new(Vec::new());
    {
        let mut zip = ZipWriter::new(&mut out_buf);
        for dir in &dir_entries {
            zip.add_directory(dir, SimpleFileOptions::default())?;
        }
        for (name, src) in plan {
            match src {
                ZipEntrySource::CopyFromPrev => {
                    let prev_archive = prev_archive_opt.as_mut().ok_or_else(|| {
                        io_invalid_data(backing_path.to_path_buf(), "missing previous zip archive")
                    })?;
                    let file = prev_archive.by_name(&name)?;
                    zip.raw_copy_file(file)?;
                }
                ZipEntrySource::Prepared(prep) => {
                    zip.add_prepared_files(std::iter::once(prep))?;
                }
                ZipEntrySource::FreshBytes(_) => {}
            }
        }
        zip.finish()?;
    }
    Ok((out_buf.into_inner(), total_zip_entries))
}

fn load_valid_prev_zip(
    backing_path: &Path,
    prev_index: Option<&CacheIndex>,
    env_hash: [u8; 32],
    verify_mode: CacheVerifyMode,
) -> Result<Option<Vec<u8>>, ConvertError> {
    let prev_env_matches = prev_index.is_some_and(|idx| idx.env_hash == env_hash);
    if !prev_env_matches {
        return Ok(None);
    }
    let backing_stamp = stamp_regular_file(
        backing_path,
        prev_index.and_then(|i| i.archive_stamp.as_ref()),
        verify_mode,
    )?;
    let archive_valid = prev_index
        .and_then(|i| i.archive_stamp)
        .zip(backing_stamp)
        .is_some_and(|(prev_s, cur_s)| prev_s.digest == cur_s.digest);
    if archive_valid {
        Ok(fs::read(backing_path).ok())
    } else {
        Ok(None)
    }
}

fn collect_zip_entry_names(archive: &mut ZipArchive<Cursor<&[u8]>>) -> HashSet<String> {
    (0..archive.len())
        .filter_map(|i| {
            archive
                .name_for_index(i)
                .and_then(Result::ok)
                .map(std::borrow::Cow::into_owned)
        })
        .collect()
}

fn finish_zip_convert_result(
    package_path: Option<PathBuf>,
    zip_bytes: Option<Vec<u8>>,
    entry_count: usize,
    stats: IncrementalStats,
) -> ConvertResult {
    if let Some(dest_zip) = package_path {
        ConvertResult {
            package_path: Some(dest_zip),
            zip_bytes: None,
            zip_file_count: 0,
            converted: Vec::new(),
            deleted: Vec::new(),
            incremental: Some(stats),
        }
    } else {
        ConvertResult {
            package_path: None,
            zip_bytes,
            zip_file_count: entry_count,
            converted: Vec::new(),
            deleted: Vec::new(),
            incremental: Some(stats),
        }
    }
}

fn handle_zip_noop(
    package_path: Option<PathBuf>,
    backing_path: &Path,
    prev_zip_bytes: Option<Vec<u8>>,
    entry_count: usize,
    total_units: usize,
    zip_plan: &ZipHitPlan,
    cwd: Option<&Path>,
) -> Result<ConvertResult, ConvertError> {
    let stats = IncrementalStats {
        total_units,
        cached_units: zip_plan.cached_units,
        rebuilt_units: 0,
        skipped_writes: zip_plan.hit_output_count + zip_plan.desired_manifests.len(),
        written_files: 0,
        removed_stale_files: 0,
    };
    if let Some(dest_zip) = &package_path
        && absolutize_with_cwd(dest_zip, cwd) != absolutize_with_cwd(backing_path, cwd)
    {
        atomic_copy_file(backing_path, dest_zip)?;
    }
    let in_mem = if package_path.is_none() {
        Some(prev_zip_bytes.ok_or_else(|| {
            io_invalid_data(backing_path.to_path_buf(), "missing backing archive bytes")
        })?)
    } else {
        None
    };
    Ok(finish_zip_convert_result(
        package_path,
        in_mem,
        entry_count,
        stats,
    ))
}

fn run_incremental_zip(
    ctx: &IncrementalCtx<'_>,
    output: &ConvertOutput,
    prepared: PreparedOutput,
) -> Result<ConvertResult, ConvertError> {
    let IncrementalCtx {
        converter,
        cs,
        target_format,
        cache,
        target_key,
        env_hash,
        cwd,
        ..
    } = *ctx;
    let index_path = cache.cache_dir().join(format!("{target_key}.sfsfc"));
    let prev_index = CacheIndex::load_from(&index_path);
    let (units, new_input_stamps, _) = build_work_units(ctx, &prepared, prev_index.as_ref())?;
    let backing_path = backing_zip_path(cache, target_key, output, &prepared);

    let prev_zip_bytes = load_valid_prev_zip(
        &backing_path,
        prev_index.as_ref(),
        env_hash,
        cache.verify_mode(),
    )?;
    let mut prev_archive_opt: Option<ZipArchive<Cursor<&[u8]>>> = prev_zip_bytes
        .as_deref()
        .and_then(|bytes| ZipArchive::new(Cursor::new(bytes)).ok());
    let prev_entry_names = prev_archive_opt.as_mut().map(collect_zip_entry_names);

    let zip_plan = classify_zip_units(
        &units,
        cs,
        target_format,
        &prepared,
        prev_index.as_ref(),
        prev_entry_names.as_ref(),
    )?;

    if zip_plan.is_noop {
        let entry_count = prev_archive_opt.as_ref().map_or(0, ZipArchive::len);
        return handle_zip_noop(
            prepared.package_path,
            &backing_path,
            prev_zip_bytes,
            entry_count,
            units.len(),
            &zip_plan,
            cwd,
        );
    }

    let miss_outputs = run_miss_units(
        converter,
        &units,
        &zip_plan.miss_indices,
        target_format,
        &prepared,
    )?;
    let miss_entries_by_unit = resolve_miss_zip_entries(&miss_outputs)?;
    let built = build_zip_plan(
        &units,
        &zip_plan.hits,
        prev_index.as_ref(),
        zip_plan.desired_manifests,
        miss_entries_by_unit,
    )?;
    let removed_stale_files = zip_plan
        .prev_all_entries
        .iter()
        .filter(|name| !built.current_entry_names.contains(*name))
        .count();

    let (zip_bytes, total_zip_entries) =
        assemble_zip_bytes(built.entries, prev_archive_opt, &backing_path)?;
    atomic_write_bytes(&backing_path, &zip_bytes)?;
    let new_archive_stamp = stamp_regular_file(&backing_path, None, CacheVerifyMode::AlwaysHash)?;
    if let Some(dest_zip) = &prepared.package_path
        && absolutize_with_cwd(dest_zip, cwd) != absolutize_with_cwd(&backing_path, cwd)
    {
        atomic_write_bytes(dest_zip, &zip_bytes)?;
    }

    CacheIndex {
        env_hash,
        archive_stamp: new_archive_stamp,
        manifests: built.manifests,
        units: built.units,
        input_stamps: new_input_stamps,
    }
    .save_to(&index_path)?;

    let stats = IncrementalStats {
        total_units: units.len(),
        cached_units: zip_plan.cached_units,
        rebuilt_units: zip_plan.rebuilt_units,
        skipped_writes: built.skipped_writes,
        written_files: built.written_files,
        removed_stale_files,
    };
    let in_mem = if prepared.package_path.is_none() {
        Some(zip_bytes)
    } else {
        None
    };
    Ok(finish_zip_convert_result(
        prepared.package_path,
        in_mem,
        total_zip_entries,
        stats,
    ))
}

fn atomic_copy_file(src: &Path, dst: &Path) -> Result<(), ConvertError> {
    let bytes = fs::read(src).map_err(|source| ConvertError::Io {
        path: src.to_path_buf(),
        source,
    })?;
    atomic_write_bytes(dst, &bytes)
}

// ---------------------------------------------------------------------------
// Public entry point invoked from MetadataConverter::convert_incremental
// ---------------------------------------------------------------------------

pub(crate) fn execute_incremental(
    converter: &MetadataConverter,
    components: &ComponentSet,
    target_format: TargetFormat,
    output: &ConvertOutput,
    cache: &ConvertCache,
) -> Result<ConvertResult, ConvertError> {
    let registry = converter.registry();
    let cs = prepare_component_set(components, output);
    let prepared = converter.prepare_output(target_format, output.clone())?;

    let mut source_comps: Vec<SourceComponent> = cs
        .source_components()
        .into_iter()
        .filter(SourceComponent::addressable)
        .collect();

    let apply_replacements = target_format == TargetFormat::Metadata
        && (prepared.is_zip
            || std::env::var("SF_APPLY_REPLACEMENTS_ON_CONVERT").as_deref() == Ok("true"));
    if apply_replacements {
        mark_replacements(&mut source_comps, cs.project_directory())?;
    }
    let replacement_mode = if apply_replacements {
        ReplacementMode::Apply
    } else {
        ReplacementMode::Skip
    };

    let cwd = std::env::current_dir().ok();
    let target_key = compute_target_cache_key(output, target_format, &prepared, cwd.as_deref());
    let env_hash = compute_env_hash(
        registry,
        &cs,
        target_format,
        &prepared,
        replacement_mode,
        cwd.as_deref(),
    )?;

    let ctx = IncrementalCtx {
        converter,
        cs: &cs,
        target_format,
        cache,
        target_key: &target_key,
        env_hash,
        source_comps: &source_comps,
        cwd: cwd.as_deref(),
    };

    if prepared.is_zip {
        run_incremental_zip(&ctx, output, prepared)
    } else {
        run_incremental_directory(&ctx, prepared)
    }
}
