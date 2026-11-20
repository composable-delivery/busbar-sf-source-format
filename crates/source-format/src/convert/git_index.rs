//! Pure-Rust reader for Git's binary index (`.git/index` v2, v3, and v4) enabling
//! zero-read content-digest reuse for stat-clean tracked files on cold cache builds.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::convert::writer::{ContentDigest, absolutize_with_cwd, metadata_len_mtime};

const DIRC_SIGNATURE: &[u8; 4] = b"DIRC";
const FLAG_EXTENDED: u16 = 0x4000;
const FLAG_STAGE_MASK: u16 = 0x3000;
const EXT_FLAG_INTENT_OR_SKIP: u16 = 0x6000;
const REGULAR_FILE_MODE_MASK: u32 = 0o170_000;
const REGULAR_FILE_MODE: u32 = 0o100_000;

#[derive(Debug, Clone, Copy)]
pub(crate) struct GitIndexEntry {
    pub ctime_sec: u32,
    pub mtime_sec: u32,
    pub mtime_nsec: u32,
    pub ino: u32,
    pub file_size: u32,
    pub digest: ContentDigest,
}

#[derive(Debug, Clone)]
pub(crate) struct GitIndexCache {
    repo_root: PathBuf,
    canonical_root: Option<PathBuf>,
    index_mtime_secs: i64,
    index_mtime_nanos: u32,
    entries: HashMap<String, GitIndexEntry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OidFormat {
    Sha1,
    Sha256,
}

struct GitPaths {
    repo_root: PathBuf,
    git_dir: PathBuf,
    common_dir: PathBuf,
}

impl GitIndexCache {
    /// Discovers a Git repository enclosing `seed_abs_path`, checks safety prerequisites
    /// (no CRLF/filter config or `.gitattributes`), and parses `.git/index` if present.
    pub(crate) fn discover_and_load(seed_abs_path: &Path) -> Option<Self> {
        let paths = find_git_paths(seed_abs_path)?;
        if has_ancestor_gitattributes(seed_abs_path, &paths.repo_root) {
            return None;
        }
        if has_unsafe_info_attributes(&paths.git_dir.join("info/attributes")) {
            return None;
        }
        let oid_format = inspect_git_config(&paths)?;

        let index_path = paths.git_dir.join("index");
        let index_meta = fs::symlink_metadata(&index_path).ok()?;
        if !index_meta.file_type().is_file() {
            return None;
        }
        let (_, index_mtime_secs, index_mtime_nanos) = metadata_len_mtime(&index_meta);
        let raw_index = fs::read(&index_path).ok()?;
        let entries = parse_dirc_index(&raw_index, oid_format)?;
        let canonical_root = fs::canonicalize(&paths.repo_root).ok();

        Some(Self {
            repo_root: paths.repo_root,
            canonical_root,
            index_mtime_secs,
            index_mtime_nanos,
            entries,
        })
    }

    /// Returns `(digest, len, mtime_secs, mtime_nanos)` when `abs_path` matches a non-racy
    /// clean stage-0 regular file entry in the parsed Git index.
    pub(crate) fn lookup_clean_stamp(
        &self,
        abs_path: &Path,
        meta: &fs::Metadata,
    ) -> Option<(ContentDigest, u64, i64, u32)> {
        if !meta.file_type().is_file() {
            return None;
        }
        let rel = self.rel_posix_path(abs_path)?;
        let entry = self.entries.get(&rel)?;

        let (len, mtime_secs, mtime_nanos) = metadata_len_mtime(meta);
        if len >= u64::from(u32::MAX) || u64::from(entry.file_size) != len {
            return None;
        }
        if mtime_secs != i64::from(entry.mtime_sec) {
            return None;
        }
        if entry.mtime_nsec != 0 && mtime_nanos != entry.mtime_nsec {
            return None;
        }
        // Racy-git guard: never trust entries modified at or after index timestamp.
        let is_non_racy = if entry.mtime_nsec != 0 {
            (mtime_secs, mtime_nanos) < (self.index_mtime_secs, self.index_mtime_nanos)
        } else {
            mtime_secs < self.index_mtime_secs
        };
        if !is_non_racy {
            return None;
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;

            if (meta.ino() & 0xFFFF_FFFF) != u64::from(entry.ino) {
                return None;
            }
            if entry.ctime_sec != 0 {
                let ctime_u32 = u64::try_from(meta.ctime()).unwrap_or(0) & 0xFFFF_FFFF;
                if ctime_u32 != u64::from(entry.ctime_sec) {
                    return None;
                }
            }
        }

        Some((entry.digest, len, mtime_secs, mtime_nanos))
    }

    fn rel_posix_path(&self, abs_path: &Path) -> Option<String> {
        let rel = abs_path.strip_prefix(&self.repo_root).ok().or_else(|| {
            let can_root = self.canonical_root.as_deref()?;
            abs_path.strip_prefix(can_root).ok()
        })?;
        let mut parts: Vec<String> = Vec::new();
        for comp in rel.components() {
            match comp {
                std::path::Component::Normal(s) => parts.push(s.to_string_lossy().into_owned()),
                std::path::Component::CurDir => {}
                _ => return None,
            }
        }
        if parts.is_empty() {
            None
        } else {
            Some(parts.join("/"))
        }
    }
}

fn find_git_paths(seed_abs_path: &Path) -> Option<GitPaths> {
    let mut cur = if seed_abs_path.is_file() {
        seed_abs_path.parent()?
    } else {
        seed_abs_path
    };

    loop {
        let dot_git = cur.join(".git");
        if dot_git.is_dir() {
            let common_dir = resolve_common_dir(&dot_git);
            return Some(GitPaths {
                repo_root: cur.to_path_buf(),
                git_dir: dot_git,
                common_dir,
            });
        }
        if dot_git.is_file()
            && let Ok(text) = fs::read_to_string(&dot_git)
            && let Some(raw_dir) = text.trim().strip_prefix("gitdir:")
        {
            let target = Path::new(raw_dir.trim());
            let git_dir = absolutize_with_cwd(target, Some(cur));
            if git_dir.is_dir() {
                let common_dir = resolve_common_dir(&git_dir);
                return Some(GitPaths {
                    repo_root: cur.to_path_buf(),
                    git_dir,
                    common_dir,
                });
            }
        }
        cur = cur.parent()?;
    }
}

fn resolve_common_dir(git_dir: &Path) -> PathBuf {
    let commondir_file = git_dir.join("commondir");
    if let Ok(text) = fs::read_to_string(commondir_file) {
        let trimmed = text.trim();
        if !trimmed.is_empty() {
            return absolutize_with_cwd(Path::new(trimmed), Some(git_dir));
        }
    }
    git_dir.to_path_buf()
}

fn has_ancestor_gitattributes(seed_abs_path: &Path, repo_root: &Path) -> bool {
    let mut cur_opt = if seed_abs_path.is_dir() {
        Some(seed_abs_path)
    } else {
        seed_abs_path.parent()
    };
    while let Some(cur) = cur_opt {
        if !cur.starts_with(repo_root) {
            break;
        }
        if cur.join(".gitattributes").exists() {
            return true;
        }
        if cur == repo_root {
            break;
        }
        cur_opt = cur.parent();
    }
    repo_root.join(".gitattributes").exists()
}

fn has_unsafe_info_attributes(path: &Path) -> bool {
    let Ok(text) = fs::read_to_string(path) else {
        return false;
    };
    text.lines().any(|raw| {
        let line = raw.trim();
        !line.is_empty()
            && !line.starts_with('#')
            && (line.contains("filter=")
                || line.contains("eol=")
                || line.contains("ident")
                || line.contains("working-tree-encoding")
                || line
                    .split_whitespace()
                    .any(|tok| tok == "text" || tok == "crlf" || tok.starts_with("text=")))
    })
}

fn collect_user_git_configs() -> Vec<PathBuf> {
    let mut out = Vec::new();
    let home = std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    if let Some(ref h) = home {
        out.push(h.join(".gitconfig"));
    }
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
    {
        out.push(xdg.join("git/config"));
    } else if let Some(ref h) = home {
        out.push(h.join(".config/git/config"));
    }
    out
}

fn inspect_git_config(paths: &GitPaths) -> Option<OidFormat> {
    let mut format = OidFormat::Sha1;
    let mut candidates = collect_user_git_configs();
    candidates.push(paths.common_dir.join("config"));
    candidates.push(paths.git_dir.join("config.worktree"));
    for cfg_path in candidates {
        let Ok(text) = fs::read_to_string(&cfg_path) else {
            continue;
        };
        if !parse_git_config_safe(&text, &mut format) {
            return None;
        }
    }
    Some(format)
}

fn parse_git_config_safe(text: &str, oid_format: &mut OidFormat) -> bool {
    let mut section = String::new();
    for raw_line in text.lines() {
        let line = raw_line.split(['#', ';']).next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        if let Some(rest) = line.strip_prefix('[')
            && let Some(end) = rest.find(']')
        {
            section = rest[..end].trim().to_ascii_lowercase();
            if section == "filter"
                || section.starts_with("filter ")
                || section.starts_with("filter\t")
            {
                return false;
            }
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let key = k.trim().to_ascii_lowercase();
        let val = v.trim().to_ascii_lowercase();
        if section == "core"
            && ((key == "autocrlf" && val == "true")
                || (key == "eol" && val == "crlf")
                || (key == "attributesfile" && !val.is_empty()))
        {
            return false;
        }
        if section == "extensions" && key == "objectformat" {
            if val == "sha256" {
                *oid_format = OidFormat::Sha256;
            } else if val != "sha1" {
                return false;
            }
        }
    }
    true
}

fn read_be_u16(bytes: &[u8], pos: &mut usize) -> Option<u16> {
    let slice = bytes.get(*pos..*pos + 2)?;
    *pos += 2;
    Some(u16::from_be_bytes([slice[0], slice[1]]))
}

fn read_be_u32(bytes: &[u8], pos: &mut usize) -> Option<u32> {
    let slice = bytes.get(*pos..*pos + 4)?;
    *pos += 4;
    Some(u32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

fn decode_v4_varint(bytes: &[u8], pos: &mut usize) -> Option<usize> {
    let mut b = *bytes.get(*pos)?;
    *pos += 1;
    let mut val = usize::from(b & 0x7f);
    while (b & 0x80) != 0 {
        val = val.checked_add(1)?;
        b = *bytes.get(*pos)?;
        *pos += 1;
        val = val.checked_shl(7)?.checked_add(usize::from(b & 0x7f))?;
    }
    Some(val)
}

fn read_entry_digest(
    bytes: &[u8],
    pos: &mut usize,
    oid_format: OidFormat,
) -> Option<ContentDigest> {
    match oid_format {
        OidFormat::Sha1 => {
            let slice = bytes.get(*pos..*pos + 20)?;
            *pos += 20;
            let mut oid = [0u8; 20];
            oid.copy_from_slice(slice);
            Some(ContentDigest::git_sha1(oid))
        }
        OidFormat::Sha256 => {
            let slice = bytes.get(*pos..*pos + 32)?;
            *pos += 32;
            let mut oid = [0u8; 32];
            oid.copy_from_slice(slice);
            Some(ContentDigest::git_sha256(oid))
        }
    }
}

fn parse_dirc_index(bytes: &[u8], oid_format: OidFormat) -> Option<HashMap<String, GitIndexEntry>> {
    if bytes.get(0..4)? != DIRC_SIGNATURE {
        return None;
    }
    let mut pos = 4usize;
    let version = read_be_u32(bytes, &mut pos)?;
    if !matches!(version, 2..=4) {
        return None;
    }
    let entry_count = usize::try_from(read_be_u32(bytes, &mut pos)?).ok()?;
    let mut entries = HashMap::with_capacity(entry_count);
    let mut prev_path: Vec<u8> = Vec::new();

    for _ in 0..entry_count {
        let entry_start = pos;
        let ctime_sec = read_be_u32(bytes, &mut pos)?;
        let _ctime_nsec = read_be_u32(bytes, &mut pos)?;
        let mtime_secs = read_be_u32(bytes, &mut pos)?;
        let mtime_nanos = read_be_u32(bytes, &mut pos)?;
        let _dev = read_be_u32(bytes, &mut pos)?;
        let ino = read_be_u32(bytes, &mut pos)?;
        let mode = read_be_u32(bytes, &mut pos)?;
        let _uid = read_be_u32(bytes, &mut pos)?;
        let _gid = read_be_u32(bytes, &mut pos)?;
        let file_size = read_be_u32(bytes, &mut pos)?;
        let digest = read_entry_digest(bytes, &mut pos, oid_format)?;
        let flags = read_be_u16(bytes, &mut pos)?;
        let ext_flags = if version >= 3 && (flags & FLAG_EXTENDED) != 0 {
            read_be_u16(bytes, &mut pos)?
        } else {
            0
        };

        let rel_bytes = parse_entry_path(bytes, &mut pos, entry_start, version, &mut prev_path)?;
        let rel_str = std::str::from_utf8(&rel_bytes).ok()?;
        if rel_str == ".gitattributes" || rel_str.ends_with("/.gitattributes") {
            return None;
        }

        let is_stage_zero = (flags & FLAG_STAGE_MASK) == 0;
        let is_regular_file = (mode & REGULAR_FILE_MODE_MASK) == REGULAR_FILE_MODE;
        let is_active = (ext_flags & EXT_FLAG_INTENT_OR_SKIP) == 0;
        if is_stage_zero && is_regular_file && is_active {
            entries.insert(
                rel_str.to_owned(),
                GitIndexEntry {
                    ctime_sec,
                    mtime_sec: mtime_secs,
                    mtime_nsec: mtime_nanos,
                    ino,
                    file_size,
                    digest,
                },
            );
        }
    }

    Some(entries)
}

fn parse_entry_path(
    bytes: &[u8],
    pos: &mut usize,
    entry_start: usize,
    version: u32,
    prev_path: &mut Vec<u8>,
) -> Option<Vec<u8>> {
    if version == 4 {
        let strip_len = decode_v4_varint(bytes, pos)?;
        let prefix_len = prev_path.len().checked_sub(strip_len)?;
        let rel_nul = bytes.get(*pos..)?.iter().position(|&b| b == 0)?;
        let suffix = bytes.get(*pos..*pos + rel_nul)?;
        *pos += rel_nul + 1;
        let mut full = Vec::with_capacity(prefix_len + suffix.len());
        full.extend_from_slice(&prev_path[..prefix_len]);
        full.extend_from_slice(suffix);
        prev_path.clone_from(&full);
        return Some(full);
    }

    let rel_nul = bytes.get(*pos..)?.iter().position(|&b| b == 0)?;
    let path_bytes = bytes.get(*pos..*pos + rel_nul)?.to_vec();
    let len_no_nul = (*pos - entry_start) + rel_nul;
    let pad = 8 - (len_no_nul % 8);
    *pos = entry_start + len_no_nul + pad;
    Some(path_bytes)
}
