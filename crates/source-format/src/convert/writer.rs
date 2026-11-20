//! Filesystem (`StandardWriter`) and ZIP (`ZipWriter`) output sinks.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fs;
use std::io::{Cursor, Write as _};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use crate::tree::{
    ForceIgnore, PathKindHint, TreeContainer, TreeError, ensure_no_symlink_on_path,
    normalize_lexical,
};
use indexmap::IndexMap;
use rayon::prelude::*;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, PreparedZipFile};

use crate::convert::error::ConvertError;
use crate::convert::replacements::{apply_replacements, is_binary_bytes};
use crate::convert::types::{ResolveScope, WriteAction, WriteSource, WriterFormat};
use crate::io_pool::io_install;

/// Hashing algorithm tag stored inside [`ContentDigest`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum DigestKind {
    /// 32-byte `blake3` digest computed directly from file bytes.
    Blake3,
    /// 20-byte Git SHA-1 blob OID zero-padded into 32 bytes.
    GitSha1,
    /// 32-byte Git SHA-256 blob OID.
    GitSha256,
}

impl DigestKind {
    pub(crate) fn to_u8(self) -> u8 {
        match self {
            Self::Blake3 => 0,
            Self::GitSha1 => 1,
            Self::GitSha256 => 2,
        }
    }

    pub(crate) fn from_u8(v: u8) -> Option<Self> {
        match v {
            0 => Some(Self::Blake3),
            1 => Some(Self::GitSha1),
            2 => Some(Self::GitSha256),
            _ => None,
        }
    }
}

/// Domain-separated cryptographic file content digest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) struct ContentDigest {
    pub kind: DigestKind,
    pub bytes: [u8; 32],
}

impl ContentDigest {
    pub(crate) fn blake3(data: &[u8]) -> Self {
        Self::from_blake3_bytes(*blake3::hash(data).as_bytes())
    }

    pub(crate) fn from_blake3_bytes(bytes: [u8; 32]) -> Self {
        Self {
            kind: DigestKind::Blake3,
            bytes,
        }
    }

    pub(crate) fn git_sha1(oid: [u8; 20]) -> Self {
        let mut bytes = [0u8; 32];
        bytes[..20].copy_from_slice(&oid);
        Self {
            kind: DigestKind::GitSha1,
            bytes,
        }
    }

    pub(crate) fn git_sha256(oid: [u8; 32]) -> Self {
        Self {
            kind: DigestKind::GitSha256,
            bytes: oid,
        }
    }
}

/// Content hash and stat metadata recorded for a file written by [`StandardWriter`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WrittenFileStamp {
    pub digest: ContentDigest,
    pub len: u64,
    pub mtime_secs: i64,
    pub mtime_nanos: u32,
}

/// Extracts `(len, mtime_secs, mtime_nanos)` from filesystem metadata without precision loss.
pub(crate) fn metadata_len_mtime(meta: &fs::Metadata) -> (u64, i64, u32) {
    let len = meta.len();
    match meta.modified() {
        Ok(sys_time) => match sys_time.duration_since(UNIX_EPOCH) {
            Ok(dur) => (
                len,
                i64::try_from(dur.as_secs()).unwrap_or(i64::MAX),
                dur.subsec_nanos(),
            ),
            Err(err) => {
                let dur = err.duration();
                (
                    len,
                    i64::try_from(dur.as_secs()).map_or(i64::MIN, |s| -s),
                    dur.subsec_nanos(),
                )
            }
        },
        Err(_) => (len, 0, 0),
    }
}

/// Writes converted metadata components to a filesystem directory or existing project tree.
pub(crate) struct StandardWriter {
    root_destination: PathBuf,
    symlink_boundary: PathBuf,
    pub converted_paths: Vec<PathBuf>,
    pub deleted_paths: Vec<PathBuf>,
}

struct PendingWrite<'a> {
    abs_output: PathBuf,
    boundary: PathBuf,
    source: &'a WriteSource,
    can_fast_copy: bool,
}

impl StandardWriter {
    pub(crate) fn new(root_destination: &Path, project_dir: Option<&Path>) -> Self {
        let abs_root = absolutize_lexical(root_destination);
        let symlink_boundary = project_dir.map_or_else(|| abs_root.clone(), absolutize_lexical);
        Self {
            root_destination: abs_root,
            symlink_boundary,
            converted_paths: Vec::new(),
            deleted_paths: Vec::new(),
        }
    }

    pub(crate) fn write_all(
        &mut self,
        items: &[WriterFormat],
        force_ignore: &ForceIgnore,
    ) -> Result<(), ConvertError> {
        self.write_all_impl(items, force_ignore, None)?;
        Ok(())
    }

    pub(crate) fn write_all_tracked(
        &mut self,
        items: &[WriterFormat],
        force_ignore: &ForceIgnore,
        known_hashes: &HashMap<PathBuf, ContentDigest>,
    ) -> Result<HashMap<PathBuf, WrittenFileStamp>, ConvertError> {
        self.write_all_impl(items, force_ignore, Some(known_hashes))
    }

    fn write_all_impl(
        &mut self,
        items: &[WriterFormat],
        force_ignore: &ForceIgnore,
        known_hashes: Option<&HashMap<PathBuf, ContentDigest>>,
    ) -> Result<HashMap<PathBuf, WrittenFileStamp>, ConvertError> {
        let check_ignore = !force_ignore.is_empty();
        let root_dest_empty =
            fs::read_dir(&self.root_destination).is_ok_and(|mut it| it.next().is_none());
        let mut pending: IndexMap<PathBuf, PendingWrite<'_>> = IndexMap::new();
        let mut seen_converted: HashSet<PathBuf> = HashSet::new();

        for item in items {
            let mut recorded_for_item = false;
            for info in &item.write_infos {
                let abs_output = if info.output.is_absolute() {
                    normalize_lexical(&info.output)
                } else {
                    resolve_within_abs_root(&self.root_destination, &info.output)?
                };

                match &info.action {
                    WriteAction::Delete => {
                        if abs_output.exists() {
                            ensure_no_symlink_on_path(&self.symlink_boundary, &abs_output)?;
                            fs::remove_file(&abs_output).map_err(|source| ConvertError::Io {
                                path: abs_output.clone(),
                                source,
                            })?;
                            self.deleted_paths.push(abs_output);
                        }
                    }
                    WriteAction::Write(source) => {
                        let inside_root = abs_output.starts_with(&self.root_destination)
                            && abs_output != self.root_destination;
                        let output_missing = root_dest_empty && inside_root;
                        if check_ignore
                            && (output_missing || !abs_output.exists())
                            && force_ignore
                                .denies_with_kind(&abs_output, PathKindHint::DirectoryOrUnwritten)
                        {
                            continue;
                        }
                        if (item.resolve_scope == ResolveScope::AllFiles || !recorded_for_item)
                            && seen_converted.insert(abs_output.clone())
                        {
                            self.converted_paths.push(abs_output.clone());
                        }
                        recorded_for_item = true;

                        let boundary = if abs_output.starts_with(&self.symlink_boundary) {
                            self.symlink_boundary.clone()
                        } else {
                            self.root_destination.clone()
                        };
                        pending.insert(
                            abs_output.clone(),
                            PendingWrite {
                                abs_output,
                                boundary,
                                source,
                                can_fast_copy: output_missing,
                            },
                        );
                    }
                }
            }
        }

        let root_state = if root_dest_empty {
            RootSymlinkState::EmptyRoot
        } else {
            RootSymlinkState::NonEmptyRoot
        };
        prepare_parent_directories(
            &pending,
            root_state,
            &self.root_destination,
            &self.symlink_boundary,
        )?;

        let writes: Vec<PendingWrite<'_>> = pending.into_values().collect();
        let records = if writes.len() >= 16 {
            io_install(|| {
                writes
                    .into_par_iter()
                    .map(|w| execute_pending_write(w, known_hashes))
                    .collect::<Result<Vec<_>, _>>()
            })?
        } else {
            writes
                .into_iter()
                .map(|w| execute_pending_write(w, known_hashes))
                .collect::<Result<Vec<_>, _>>()?
        };

        let mut stamped = HashMap::new();
        for rec in records.into_iter().flatten() {
            stamped.insert(rec.0, rec.1);
        }
        Ok(stamped)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RootSymlinkState {
    EmptyRoot,
    NonEmptyRoot,
}

fn prepare_parent_directories(
    pending: &IndexMap<PathBuf, PendingWrite<'_>>,
    root_state: RootSymlinkState,
    root_destination: &Path,
    symlink_boundary: &Path,
) -> Result<(), ConvertError> {
    if root_state == RootSymlinkState::EmptyRoot {
        let root_boundary = if root_destination.starts_with(symlink_boundary) {
            symlink_boundary
        } else {
            root_destination
        };
        ensure_no_symlink_on_path(root_boundary, root_destination)?;
    }

    let mut checked_dir_boundaries: HashSet<(PathBuf, PathBuf)> = HashSet::new();
    let mut created_dirs: HashSet<PathBuf> = HashSet::new();
    let mut unique_dirs: Vec<PathBuf> = Vec::new();

    for entry in pending.values() {
        let Some(parent) = entry.abs_output.parent() else {
            continue;
        };
        let needs_symlink_check =
            root_state != RootSymlinkState::EmptyRoot || !parent.starts_with(root_destination);
        let parent_buf = parent.to_path_buf();
        if needs_symlink_check
            && checked_dir_boundaries.insert((entry.boundary.clone(), parent_buf.clone()))
        {
            ensure_no_symlink_on_path(&entry.boundary, parent)?;
        }
        if created_dirs.insert(parent_buf.clone()) {
            unique_dirs.push(parent_buf);
        }
    }

    if unique_dirs.len() >= 16 {
        io_install(|| {
            unique_dirs.into_par_iter().try_for_each(|dir| {
                fs::create_dir_all(&dir).map_err(|source| ConvertError::Io { path: dir, source })
            })
        })?;
    } else {
        for dir in unique_dirs {
            fs::create_dir_all(&dir).map_err(|source| ConvertError::Io { path: dir, source })?;
        }
    }
    Ok(())
}

fn execute_pending_write(
    write: PendingWrite<'_>,
    known_hashes: Option<&HashMap<PathBuf, ContentDigest>>,
) -> Result<Option<(PathBuf, WrittenFileStamp)>, ConvertError> {
    if write.can_fast_copy
        && let WriteSource::TreeFile {
            tree,
            path,
            replacements,
        } = write.source
        && replacements.is_empty()
        && let Some(src_path) = tree.fs_path(path)
    {
        fs::copy(&src_path, &write.abs_output).map_err(|source| ConvertError::Io {
            path: write.abs_output.clone(),
            source,
        })?;
        let Some(hashes) = known_hashes else {
            return Ok(None);
        };
        let digest = if let Some(h) = hashes.get(path).or_else(|| hashes.get(&src_path)) {
            *h
        } else {
            let copied = fs::read(&write.abs_output).map_err(|source| ConvertError::Io {
                path: write.abs_output.clone(),
                source,
            })?;
            ContentDigest::blake3(&copied)
        };
        let meta = fs::metadata(&write.abs_output).map_err(|source| ConvertError::Io {
            path: write.abs_output.clone(),
            source,
        })?;
        let (len, mtime_secs, mtime_nanos) = metadata_len_mtime(&meta);
        return Ok(Some((
            write.abs_output,
            WrittenFileStamp {
                digest,
                len,
                mtime_secs,
                mtime_nanos,
            },
        )));
    }

    let bytes = resolve_write_source(write.source)?;
    let digest_opt = known_hashes.map(|hashes| {
        if let Some(h) = hashes.get(&write.abs_output) {
            *h
        } else if let WriteSource::TreeFile {
            path, replacements, ..
        } = write.source
            && replacements.is_empty()
            && let Some(h) = hashes.get(path)
        {
            *h
        } else {
            ContentDigest::blake3(&bytes)
        }
    });

    let meta_opt = write_file_no_symlink(
        &write,
        &bytes,
        if digest_opt.is_some() {
            CaptureMeta::Yes
        } else {
            CaptureMeta::No
        },
    )?;
    let (Some(digest), Some((len, mtime_secs, mtime_nanos))) = (digest_opt, meta_opt) else {
        return Ok(None);
    };
    Ok(Some((
        write.abs_output,
        WrittenFileStamp {
            digest,
            len,
            mtime_secs,
            mtime_nanos,
        },
    )))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CaptureMeta {
    Yes,
    No,
}

#[cfg(unix)]
fn write_file_no_symlink(
    write: &PendingWrite<'_>,
    bytes: &[u8],
    capture_meta: CaptureMeta,
) -> Result<Option<(u64, i64, u32)>, ConvertError> {
    use std::os::unix::fs::OpenOptionsExt as _;

    let open_res = fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&write.abs_output);
    let mut file = match open_res {
        Ok(f) => f,
        Err(err) if err.raw_os_error() == Some(libc::ELOOP) => {
            return Err(ConvertError::Tree(TreeError::SymlinkOnPath {
                symlink: write.abs_output.clone(),
                destination: write.abs_output.clone(),
                root: write.boundary.clone(),
            }));
        }
        Err(source) => {
            return Err(ConvertError::Io {
                path: write.abs_output.clone(),
                source,
            });
        }
    };
    file.write_all(bytes).map_err(|source| ConvertError::Io {
        path: write.abs_output.clone(),
        source,
    })?;
    if capture_meta == CaptureMeta::No {
        return Ok(None);
    }
    let meta = file.metadata().map_err(|source| ConvertError::Io {
        path: write.abs_output.clone(),
        source,
    })?;
    Ok(Some(metadata_len_mtime(&meta)))
}

#[cfg(not(unix))]
fn write_file_no_symlink(
    write: &PendingWrite<'_>,
    bytes: &[u8],
    capture_meta: CaptureMeta,
) -> Result<Option<(u64, i64, u32)>, ConvertError> {
    match fs::symlink_metadata(&write.abs_output) {
        Ok(meta) if meta.file_type().is_symlink() => {
            return Err(ConvertError::Tree(TreeError::SymlinkOnPath {
                symlink: write.abs_output.clone(),
                destination: write.abs_output.clone(),
                root: write.boundary.clone(),
            }));
        }
        Ok(_) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(ConvertError::Io {
                path: write.abs_output.clone(),
                source,
            });
        }
    }
    fs::write(&write.abs_output, bytes).map_err(|source| ConvertError::Io {
        path: write.abs_output.clone(),
        source,
    })?;
    if capture_meta == CaptureMeta::No {
        return Ok(None);
    }
    let meta = fs::metadata(&write.abs_output).map_err(|source| ConvertError::Io {
        path: write.abs_output.clone(),
        source,
    })?;
    Ok(Some(metadata_len_mtime(&meta)))
}

pub(crate) fn absolutize_lexical(path: &Path) -> PathBuf {
    absolutize_with_cwd(path, std::env::current_dir().ok().as_deref())
}

pub(crate) fn absolutize_with_cwd(path: &Path, cwd: Option<&Path>) -> PathBuf {
    if path.is_absolute() {
        normalize_lexical(path)
    } else if let Some(base) = cwd {
        normalize_lexical(&base.join(path))
    } else {
        normalize_lexical(path)
    }
}

fn resolve_within_abs_root(abs_root: &Path, candidate: &Path) -> Result<PathBuf, TreeError> {
    let joined = if candidate.is_absolute() {
        normalize_lexical(candidate)
    } else {
        normalize_lexical(&abs_root.join(candidate))
    };
    if joined == abs_root || joined.starts_with(abs_root) {
        Ok(joined)
    } else {
        Err(TreeError::PathOutsideRoot {
            path: candidate.to_path_buf(),
            root: abs_root.to_path_buf(),
        })
    }
}

/// Writes converted metadata components and manifests into an in-memory ZIP buffer.
pub(crate) struct ArchiveWriter {
    entries: Vec<(String, Vec<u8>)>,
    dir_entries: BTreeSet<String>,
    file_count: usize,
}

impl ArchiveWriter {
    pub(crate) fn new() -> Self {
        Self {
            entries: Vec::new(),
            dir_entries: BTreeSet::new(),
            file_count: 0,
        }
    }

    pub(crate) fn add_bytes(&mut self, path: &Path, bytes: Vec<u8>) {
        let posix = path.to_string_lossy().replace('\\', "/");
        let clean = posix.trim_start_matches('/').to_owned();
        let parts: Vec<&str> = clean.split('/').collect();
        for i in 1..parts.len() {
            self.dir_entries
                .insert(format!("{}/", parts[..i].join("/")));
        }
        self.file_count += 1;
        self.entries.push((clean, bytes));
    }

    pub(crate) fn add_all(&mut self, items: &[WriterFormat]) -> Result<(), ConvertError> {
        let pending: Vec<(&Path, &WriteSource)> = items
            .iter()
            .flat_map(|item| item.write_infos.iter())
            .filter_map(|info| match &info.action {
                WriteAction::Write(source) => Some((info.output.as_path(), source)),
                WriteAction::Delete => None,
            })
            .collect();

        if pending.len() >= 16 {
            let resolved: Vec<(&Path, Vec<u8>)> = io_install(|| {
                pending
                    .into_par_iter()
                    .map(|(path, source)| {
                        Ok::<_, ConvertError>((path, resolve_write_source(source)?))
                    })
                    .collect::<Result<Vec<_>, _>>()
            })?;
            for (path, bytes) in resolved {
                self.add_bytes(path, bytes);
            }
        } else {
            for (path, source) in pending {
                let bytes = resolve_write_source(source)?;
                self.add_bytes(path, bytes);
            }
        }
        Ok(())
    }

    pub(crate) fn finish(self) -> Result<(Vec<u8>, usize), ConvertError> {
        let entry_count = self.file_count + self.dir_entries.len();
        let mut buf = Cursor::new(Vec::new());
        {
            let mut zip = zip::ZipWriter::new(&mut buf);
            let options = SimpleFileOptions::default()
                .compression_method(CompressionMethod::Deflated)
                .compression_level(Some(3));
            for dir in &self.dir_entries {
                zip.add_directory(dir, SimpleFileOptions::default())?;
            }
            if self.entries.len() >= 16 {
                let prepared: Vec<PreparedZipFile> = self
                    .entries
                    .into_par_iter()
                    .map(|(name, data)| PreparedZipFile::from_data(name, options, &data))
                    .collect::<Result<Vec<_>, _>>()?;
                zip.add_prepared_files(prepared)?;
            } else {
                for (name, data) in self.entries {
                    zip.start_file(name, options)?;
                    zip.write_all(&data).map_err(|source| ConvertError::Io {
                        path: PathBuf::from("<zip>"),
                        source,
                    })?;
                }
            }
            zip.finish()?;
        }
        Ok((buf.into_inner(), entry_count))
    }
}

pub(crate) fn resolve_write_source(source: &WriteSource) -> Result<Vec<u8>, ConvertError> {
    match source {
        WriteSource::Bytes(bytes) => Ok(bytes.clone()),
        WriteSource::TreeFile {
            tree,
            path,
            replacements,
        } => {
            let bytes = tree.read_file(path)?;
            if replacements.is_empty() || is_binary_bytes(&bytes) {
                return Ok(bytes);
            }
            let text = String::from_utf8_lossy(&bytes);
            let replaced = apply_replacements(&text, replacements);
            Ok(replaced.into_bytes())
        }
    }
}
