//! Filesystem preflight guard against corrupt/empty LanceDB manifests.
//!
//! A crash or kill during a LanceDB commit can leave a freshly-created but
//! never-written (zero-byte) manifest in a table's `_versions/` directory.
//! LanceDB selects the *latest* version purely by filename — for the V2 naming
//! scheme that is the lowest-numbered file (`(u64::MAX - version).manifest`) —
//! without inspecting the file's contents. It then tries to read the manifest
//! and fails with `Invalid range 0..0 for object of size 0 bytes`, crash-looping
//! the server even though the data files and older manifests are intact.
//!
//! [`guard_lancedb_path`] runs before the store is opened. For each table it
//! checks whether the manifest LanceDB would pick (the newest version) is
//! readable; if it is zero-byte or truncated, the manifest is *quarantined*
//! (moved aside, never deleted) and the next-newest manifest is re-evaluated.
//! The server then opens the last good version instead of dying.
//!
//! The guard only operates on local filesystem stores, which is all MemCan uses.

use std::fs;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use tracing::{debug, error, info, warn};

/// Directory (relative to a table root) holding per-version manifest files.
const VERSIONS_DIR: &str = "_versions";
/// Directory where corrupt manifests are quarantined (never deleted).
const CORRUPT_DIR: &str = "_corrupt_manifests";
/// Suffix shared by every manifest filename.
const MANIFEST_SUFFIX: &str = ".manifest";
/// Length of the zero-padded inverted version stem used by the V2 naming scheme.
const V2_STEM_LEN: usize = 20;
/// Directory suffix LanceDB uses for a table's data directory.
const LANCE_DIR_SUFFIX: &str = ".lance";

/// Magic written as the final 4 bytes of every complete Lance manifest
/// (`lance_table::format::MAGIC`). A manifest missing this footer was never
/// fully written and is unsafe to open.
const LANCE_MAGIC: &[u8; 4] = b"LANC";
/// Minimum byte length of a valid manifest footer: `i64` position pointer +
/// two `i16` version fields + the 4-byte [`LANCE_MAGIC`].
const MIN_MANIFEST_LEN: u64 = 8 + 2 + 2 + 4;

/// A manifest file discovered inside a table's `_versions/` directory.
#[derive(Debug)]
struct ManifestEntry {
    /// Decoded version number (higher = newer).
    version: u64,
    /// Absolute path to the manifest file.
    path: PathBuf,
    /// File size in bytes.
    len: u64,
}

/// Scan every `*.lance` table under `base` and quarantine corrupt latest
/// manifests so the store opens the last good version.
///
/// Returns the total number of manifests quarantined across all tables. This is
/// a best-effort preflight: per-table I/O errors are logged and skipped rather
/// than propagated, so a transient problem with one table never blocks startup.
pub fn guard_lancedb_path(base: &Path) -> usize {
    let entries = match fs::read_dir(base) {
        Ok(entries) => entries,
        Err(e) => {
            // Missing base dir is normal on first run; anything else is worth a note.
            if e.kind() != io::ErrorKind::NotFound {
                warn!(path = %base.display(), error = %e, "manifest preflight: cannot read LanceDB directory");
            }
            return 0;
        }
    };

    let mut total = 0;
    for entry in entries {
        let path = match entry {
            Ok(entry) => entry.path(),
            Err(e) => {
                warn!(path = %base.display(), error = %e, "manifest preflight: cannot read directory entry");
                continue;
            }
        };
        if !is_lance_table_dir(&path) {
            continue;
        }
        match guard_table_versions(&path) {
            Ok(quarantined) => total += quarantined.len(),
            Err(e) => warn!(
                table = %path.display(),
                error = %e,
                "manifest preflight: skipping table after I/O error"
            ),
        }
    }

    if total > 0 {
        info!(
            path = %base.display(),
            quarantined = total,
            "manifest preflight: quarantined corrupt LanceDB manifest(s); recovered to last good version"
        );
    }
    total
}

/// True when `path` is a directory named `<table>.lance`.
fn is_lance_table_dir(path: &Path) -> bool {
    path.is_dir()
        && path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.ends_with(LANCE_DIR_SUFFIX))
}

/// Quarantine corrupt latest manifests for a single table directory.
///
/// Repeatedly inspects the manifest LanceDB would open (the highest version).
/// While that manifest is corrupt it is moved into a sibling
/// [`CORRUPT_DIR`] and the next-newest is re-evaluated, until a valid latest
/// manifest remains. To stay conservative the guard never empties
/// `_versions/`: if the *only* remaining manifest is corrupt it is left in
/// place and an error is logged, so a human can recover the data files (which
/// are never touched).
///
/// Returns the quarantined files, in the order they were moved.
fn guard_table_versions(table_dir: &Path) -> io::Result<Vec<PathBuf>> {
    let versions_dir = table_dir.join(VERSIONS_DIR);
    if !versions_dir.is_dir() {
        return Ok(Vec::new());
    }

    let mut quarantined = Vec::new();
    loop {
        let mut entries = list_manifest_entries(&versions_dir)?;
        if entries.is_empty() {
            break;
        }
        // LanceDB opens the highest version; evaluate that one.
        entries.sort_by_key(|e| std::cmp::Reverse(e.version));
        let latest = &entries[0];

        if manifest_is_valid(latest) {
            break;
        }

        if entries.len() == 1 {
            error!(
                table = %table_dir.display(),
                manifest = %latest.path.display(),
                bytes = latest.len,
                "manifest preflight: only remaining manifest is corrupt; cannot auto-recover. \
                 Data files are intact — manual recovery required"
            );
            break;
        }

        let dest = quarantine_manifest(table_dir, &latest.path)?;
        warn!(
            table = %table_dir.display(),
            version = latest.version,
            bytes = latest.len,
            from = %latest.path.display(),
            to = %dest.display(),
            "manifest preflight: quarantined corrupt manifest; falling back to previous version"
        );
        quarantined.push(dest);
    }

    Ok(quarantined)
}

/// List parseable manifest files in a `_versions/` directory.
///
/// Files that are not well-formed manifest names (temporary files, detached
/// versions, unrelated entries) are skipped — only files LanceDB would treat as
/// real versions are returned.
fn list_manifest_entries(versions_dir: &Path) -> io::Result<Vec<ManifestEntry>> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(versions_dir)? {
        let entry = entry?;
        let path = entry.path();
        let Some(version) = path
            .file_name()
            .and_then(|n| n.to_str())
            .and_then(manifest_version)
        else {
            continue;
        };
        let len = entry.metadata()?.len();
        entries.push(ManifestEntry { version, path, len });
    }
    Ok(entries)
}

/// Decode the version a manifest filename represents, or `None` if the name is
/// not a regular attached manifest.
///
/// Mirrors `lance_table`'s scheme detection: a 20-character zero-padded stem is
/// V2 (filename stores `u64::MAX - version`), anything else is V1.
fn manifest_version(filename: &str) -> Option<u64> {
    let stem = filename.strip_suffix(MANIFEST_SUFFIX)?;
    let number: u64 = stem.parse().ok()?;
    if stem.len() == V2_STEM_LEN {
        Some(u64::MAX - number)
    } else {
        Some(number)
    }
}

/// Whether a manifest is safe for LanceDB to open.
///
/// A manifest is valid when it is at least [`MIN_MANIFEST_LEN`] bytes and ends
/// with [`LANCE_MAGIC`]. Zero-byte and truncated (partially written) manifests
/// fail this check. A complete manifest always carries the magic footer, so a
/// valid file is never misclassified. If the footer cannot be read because of a
/// transient I/O error the manifest is treated as valid (left untouched), so the
/// guard never quarantines on ambiguous evidence.
fn manifest_is_valid(entry: &ManifestEntry) -> bool {
    if entry.len < MIN_MANIFEST_LEN {
        return false;
    }
    match read_magic_footer(&entry.path) {
        Ok(footer) => &footer == LANCE_MAGIC,
        Err(e) => {
            debug!(
                manifest = %entry.path.display(),
                error = %e,
                "manifest preflight: could not read footer; leaving manifest in place"
            );
            true
        }
    }
}

/// Read the final 4 bytes of a file. The caller guarantees `len >= 4`.
fn read_magic_footer(path: &Path) -> io::Result<[u8; 4]> {
    let mut file = fs::File::open(path)?;
    file.seek(SeekFrom::End(-4))?;
    let mut footer = [0u8; 4];
    file.read_exact(&mut footer)?;
    Ok(footer)
}

/// Move a corrupt manifest into the table's quarantine directory.
///
/// Keeps the original filename; on collision (a prior quarantine of the same
/// version) a numeric suffix is appended so nothing is overwritten.
fn quarantine_manifest(table_dir: &Path, manifest: &Path) -> io::Result<PathBuf> {
    let corrupt_dir = table_dir.join(CORRUPT_DIR);
    fs::create_dir_all(&corrupt_dir)?;

    let filename = manifest.file_name().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "manifest path has no filename")
    })?;
    let mut dest = corrupt_dir.join(filename);
    let mut suffix = 1;
    while dest.exists() {
        dest = corrupt_dir.join(format!("{}.{suffix}", filename.to_string_lossy()));
        suffix += 1;
    }

    fs::rename(manifest, &dest)?;
    Ok(dest)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a V2 manifest filename for the given version.
    fn v2_name(version: u64) -> String {
        format!("{:020}.{}", u64::MAX - version, "manifest")
    }

    /// Write a fully-formed (valid) manifest of `len` bytes ending in the magic.
    fn write_valid_manifest(dir: &Path, version: u64) {
        let path = dir.join(v2_name(version));
        let mut bytes = vec![0u8; 28];
        bytes.extend_from_slice(LANCE_MAGIC);
        fs::write(path, bytes).expect("write valid manifest");
    }

    /// Write a zero-byte manifest (the exact production failure).
    fn write_empty_manifest(dir: &Path, version: u64) {
        fs::write(dir.join(v2_name(version)), b"").expect("write empty manifest");
    }

    /// Names present in a directory, or empty if it does not exist.
    fn names_in(dir: &Path) -> Vec<String> {
        let Ok(rd) = fs::read_dir(dir) else {
            return Vec::new();
        };
        rd.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect()
    }

    fn setup_versions(table_dir: &Path) -> PathBuf {
        let versions = table_dir.join(VERSIONS_DIR);
        fs::create_dir_all(&versions).expect("create _versions");
        versions
    }

    #[test]
    fn manifest_version_parses_v2_newest_as_highest() {
        // Version 4's file sorts lexically *below* version 3's, but decodes higher.
        let v3 = v2_name(3);
        let v4 = v2_name(4);
        assert!(v4 < v3, "newer V2 filename should sort first");
        assert_eq!(manifest_version(&v3), Some(3));
        assert_eq!(manifest_version(&v4), Some(4));
    }

    #[test]
    fn manifest_version_parses_v1() {
        assert_eq!(manifest_version("42.manifest"), Some(42));
        assert_eq!(manifest_version("0.manifest"), Some(0));
    }

    #[test]
    fn manifest_version_rejects_non_manifests() {
        assert_eq!(manifest_version("data.lance"), None);
        assert_eq!(manifest_version("d7.manifest"), None); // detached version
        assert_eq!(manifest_version(".tmp_7.manifest_uuid"), None); // staging temp
        assert_eq!(manifest_version("notanumber.manifest"), None);
    }

    #[test]
    fn manifest_is_valid_detects_corruption() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let dir = tmp.path();

        let empty = dir.join("empty");
        fs::write(&empty, b"").unwrap();
        assert!(!manifest_is_valid(&ManifestEntry {
            version: 1,
            path: empty,
            len: 0,
        }));

        let truncated = dir.join("truncated");
        fs::write(&truncated, vec![0u8; 20]).unwrap();
        assert!(!manifest_is_valid(&ManifestEntry {
            version: 1,
            path: truncated,
            len: 20,
        }));

        let good = dir.join("good");
        let mut bytes = vec![0u8; 12];
        bytes.extend_from_slice(LANCE_MAGIC);
        fs::write(&good, &bytes).unwrap();
        assert!(manifest_is_valid(&ManifestEntry {
            version: 1,
            path: good,
            len: bytes.len() as u64,
        }));
    }

    #[test]
    fn single_zero_byte_latest_recovers_to_prior_version() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let table = tmp.path().join("memories.lance");
        let versions = setup_versions(&table);

        write_valid_manifest(&versions, 1);
        write_valid_manifest(&versions, 2);
        write_valid_manifest(&versions, 3);
        write_empty_manifest(&versions, 3); // overwrite latest as 0-byte

        let moved = guard_table_versions(&table).expect("guard");
        assert_eq!(moved.len(), 1, "exactly one manifest quarantined");

        let remaining = names_in(&versions);
        assert_eq!(remaining.len(), 2, "two good manifests remain");
        assert!(remaining.contains(&v2_name(1)));
        assert!(remaining.contains(&v2_name(2)));
        assert!(
            !remaining.contains(&v2_name(3)),
            "corrupt v3 removed from _versions"
        );

        let quarantined = names_in(&table.join(CORRUPT_DIR));
        assert_eq!(quarantined, vec![v2_name(3)]);
    }

    #[test]
    fn multiple_consecutive_zero_byte_manifests_recover() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let table = tmp.path().join("memories.lance");
        let versions = setup_versions(&table);

        for v in 1..=4 {
            write_valid_manifest(&versions, v);
        }
        write_empty_manifest(&versions, 4);
        write_empty_manifest(&versions, 3);

        let moved = guard_table_versions(&table).expect("guard");
        assert_eq!(moved.len(), 2, "two manifests quarantined");

        let remaining = names_in(&versions);
        assert_eq!(remaining.len(), 2);
        assert!(remaining.contains(&v2_name(1)));
        assert!(remaining.contains(&v2_name(2)));

        let mut quarantined = names_in(&table.join(CORRUPT_DIR));
        quarantined.sort();
        let mut expected = vec![v2_name(3), v2_name(4)];
        expected.sort();
        assert_eq!(quarantined, expected);
    }

    #[test]
    fn all_good_manifests_are_a_noop() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let table = tmp.path().join("memories.lance");
        let versions = setup_versions(&table);

        for v in 1..=3 {
            write_valid_manifest(&versions, v);
        }
        let before = names_in(&versions);

        let moved = guard_table_versions(&table).expect("guard");
        assert!(
            moved.is_empty(),
            "nothing quarantined when all manifests valid"
        );

        let mut after = names_in(&versions);
        let mut before_sorted = before;
        after.sort();
        before_sorted.sort();
        assert_eq!(after, before_sorted, "_versions untouched");
        assert!(
            !table.join(CORRUPT_DIR).exists(),
            "quarantine dir not created on no-op"
        );
    }

    #[test]
    fn never_empties_versions_when_all_corrupt() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let table = tmp.path().join("memories.lance");
        let versions = setup_versions(&table);

        for v in 1..=3 {
            write_empty_manifest(&versions, v);
        }

        let moved = guard_table_versions(&table).expect("guard");
        assert_eq!(moved.len(), 2, "quarantines all but the last manifest");

        let remaining = names_in(&versions);
        assert_eq!(remaining.len(), 1, "one manifest always left in place");
        assert_eq!(remaining, vec![v2_name(1)], "oldest manifest retained");
    }

    #[test]
    fn missing_versions_dir_is_a_noop() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let table = tmp.path().join("memories.lance");
        fs::create_dir_all(&table).expect("create table dir");

        let moved = guard_table_versions(&table).expect("guard");
        assert!(moved.is_empty());
    }

    #[test]
    fn guard_lancedb_path_scans_tables_and_ignores_non_lance_dirs() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let base = tmp.path();

        // A table needing recovery.
        let memories = base.join("memories.lance");
        let mem_versions = setup_versions(&memories);
        write_valid_manifest(&mem_versions, 1);
        write_valid_manifest(&mem_versions, 2);
        write_empty_manifest(&mem_versions, 2);

        // A healthy table.
        let standards = base.join("standards.lance");
        let std_versions = setup_versions(&standards);
        write_valid_manifest(&std_versions, 1);

        // An unrelated directory that must be ignored.
        fs::create_dir_all(base.join("not-a-table")).unwrap();

        let total = guard_lancedb_path(base);
        assert_eq!(total, 1, "only the corrupt table is acted upon");

        assert_eq!(names_in(&mem_versions).len(), 1);
        assert_eq!(names_in(&std_versions), vec![v2_name(1)]);
        assert!(!standards.join(CORRUPT_DIR).exists());
    }

    #[test]
    fn guard_lancedb_path_missing_base_is_a_noop() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let missing = tmp.path().join("does-not-exist");
        assert_eq!(guard_lancedb_path(&missing), 0);
    }
}
