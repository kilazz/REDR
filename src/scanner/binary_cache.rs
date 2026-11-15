//! The binary format of the scan cache.
//!
//! The cache holds one entry per directory of a scanned tree. As a JSON
//! document that grows to hundreds of megabytes on a large volume, so the
//! entries are stored in a compact binary file instead.
//!
//! The paths are kept in a single table; everything else refers to an index
//! in that table, so a path is written exactly once:
//!
//! ```text
//! magic:      "RDRC"          (4 bytes)
//! version:    u8              (1 byte)
//! reserved:   u8              (1 byte)
//! root length: u16             (2 bytes, little endian)
//! root:       utf-8 bytes
//! timestamp:  u64             (8 bytes, little endian)
//! settings:   u64             (8 bytes, little endian)
//! path count: u32             (4 bytes, little endian)
//! paths:
//!   length:   u16
//!   bytes:    utf-8
//! directory count: u32        (4 bytes, little endian)
//! directories (one per path, in the same order):
//!   state:       u8
//!   depth:       i32
//!   stamp:       u64
//!   child count: u32
//! result count: u32           (4 bytes, little endian)
//! results:
//!   path index: u32
//!   status:     i32
//! ```
//!
//! The results hold every reported directory, the parents included, so the
//! tree can be rebuilt from the cache.

use std::collections::BTreeMap;
use std::fs;
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::Path;

/// The magic bytes at the start of a cache file.
const MAGIC: &[u8; 4] = b"RDRC";
/// The version of the format.
const VERSION: u8 = 1;

/// The binary content of a cache.
#[derive(Default, Clone, Debug)]
pub struct BinaryCache {
    /// The root the scan was made on.
    pub root: String,
    /// When the scan was made (Unix seconds).
    pub timestamp: u64,
    /// The fingerprint of the settings the scan was made with.
    pub settings_hash: u64,
    /// Every reported directory with its status, parents included.
    pub all_dirs: BTreeMap<String, i32>,
    /// The modification time of every visited directory.
    pub dir_stamps: BTreeMap<String, u64>,
    /// The state of every visited directory.
    pub dir_states: BTreeMap<String, u8>,
    /// The depth of every reported directory.
    pub dir_depths: BTreeMap<String, i32>,
    /// The number of entries of every visited directory.
    pub dir_child_counts: BTreeMap<String, usize>,
}

impl BinaryCache {
    /// The paths of the visited directories, in the order they are stored.
    fn paths(&self) -> Vec<&String> {
        let mut paths: Vec<&String> = self.dir_states.keys().collect();
        // The results may mention a directory that has no state yet.
        for path in self.all_dirs.keys() {
            if !paths.contains(&path) {
                paths.push(path);
            }
        }
        paths
    }
}

/// Reads a cache from a reader.
pub fn read<R: Read>(reader: R) -> Result<BinaryCache, String> {
    let mut reader = BufReader::new(reader);

    let mut magic = [0u8; 4];
    reader
        .read_exact(&mut magic)
        .map_err(|e| format!("Cannot read the magic bytes: {}", e))?;
    if &magic != MAGIC {
        return Err("Not a REDR cache file".to_string());
    }

    let mut version = [0u8; 1];
    reader
        .read_exact(&mut version)
        .map_err(|e| format!("Cannot read the version: {}", e))?;
    if version[0] != VERSION {
        return Err(format!("Unsupported cache version: {}", version[0]));
    }

    let mut reserved = [0u8; 1];
    reader
        .read_exact(&mut reserved)
        .map_err(|e| format!("Cannot read the header: {}", e))?;

    let root = read_string(&mut reader)?;
    let timestamp = read_u64(&mut reader)?;
    let settings_hash = read_u64(&mut reader)?;

    // The path table.
    let path_count = read_u32(&mut reader)? as usize;
    let mut paths: Vec<String> = Vec::with_capacity(path_count);
    for _ in 0..path_count {
        paths.push(read_string(&mut reader)?);
    }

    // The metadata of every directory, in the order of the table.
    let dir_count = read_u32(&mut reader)? as usize;
    let mut cache = BinaryCache {
        root,
        timestamp,
        settings_hash,
        ..Default::default()
    };
    for path in paths.iter().take(dir_count) {
        let mut state = [0u8; 1];
        reader
            .read_exact(&mut state)
            .map_err(|e| format!("Cannot read the state: {}", e))?;
        let depth = read_i32(&mut reader)?;
        let stamp = read_u64(&mut reader)?;
        let child_count = read_u32(&mut reader)?;

        cache.dir_states.insert(path.clone(), state[0]);
        cache.dir_depths.insert(path.clone(), depth);
        cache.dir_stamps.insert(path.clone(), stamp);
        cache
            .dir_child_counts
            .insert(path.clone(), child_count as usize);
    }

    // The reported directories, as an index into the table above.
    let result_count = read_u32(&mut reader)?;
    for _ in 0..result_count {
        let index = read_u32(&mut reader)? as usize;
        let status = read_i32(&mut reader)?;
        let path = paths
            .get(index)
            .ok_or_else(|| format!("The result index {} is out of range", index))?;
        cache.all_dirs.insert(path.clone(), status);
    }

    Ok(cache)
}

/// Writes a cache to a writer.
pub fn write<W: Write>(writer: W, cache: &BinaryCache) -> Result<(), String> {
    let mut writer = BufWriter::new(writer);
    let paths = cache.paths();
    let mut index_of: std::collections::HashMap<&str, u32> = std::collections::HashMap::new();
    for (index, path) in paths.iter().enumerate() {
        index_of.insert(path.as_str(), index as u32);
    }

    writer
        .write_all(MAGIC)
        .map_err(|e| format!("Cannot write the magic bytes: {}", e))?;
    writer
        .write_all(&[VERSION, 0])
        .map_err(|e| format!("Cannot write the header: {}", e))?;
    write_string(&mut writer, &cache.root)?;
    writer
        .write_all(&cache.timestamp.to_le_bytes())
        .map_err(|e| format!("Cannot write the timestamp: {}", e))?;
    writer
        .write_all(&cache.settings_hash.to_le_bytes())
        .map_err(|e| format!("Cannot write the settings hash: {}", e))?;

    // The path table.
    writer
        .write_all(&(paths.len() as u32).to_le_bytes())
        .map_err(|e| format!("Cannot write the path count: {}", e))?;
    for path in &paths {
        write_string(&mut writer, path)?;
    }

    // The metadata of every directory.
    writer
        .write_all(&(paths.len() as u32).to_le_bytes())
        .map_err(|e| format!("Cannot write the directory count: {}", e))?;
    for path in &paths {
        let state = cache.dir_states.get(*path).copied().unwrap_or(0);
        let depth = cache.dir_depths.get(*path).copied().unwrap_or(0);
        let stamp = cache.dir_stamps.get(*path).copied().unwrap_or(0);
        let count = cache.dir_child_counts.get(*path).copied().unwrap_or(0);

        writer
            .write_all(&[state])
            .map_err(|e| format!("Cannot write the state: {}", e))?;
        writer
            .write_all(&depth.to_le_bytes())
            .map_err(|e| format!("Cannot write the depth: {}", e))?;
        writer
            .write_all(&stamp.to_le_bytes())
            .map_err(|e| format!("Cannot write the stamp: {}", e))?;
        writer
            .write_all(&(count as u32).to_le_bytes())
            .map_err(|e| format!("Cannot write the child count: {}", e))?;
    }

    // The reported directories.
    writer
        .write_all(&(cache.all_dirs.len() as u32).to_le_bytes())
        .map_err(|e| format!("Cannot write the result count: {}", e))?;

    for (path, status) in &cache.all_dirs {
        let index = index_of
            .get(path.as_str())
            .copied()
            .ok_or_else(|| format!("The directory {} is not in the table", path))?;
        writer
            .write_all(&index.to_le_bytes())
            .map_err(|e| format!("Cannot write the result index: {}", e))?;
        writer
            .write_all(&status.to_le_bytes())
            .map_err(|e| format!("Cannot write the status: {}", e))?;
    }

    writer
        .flush()
        .map_err(|e| format!("Cannot flush the cache: {}", e))
}

/// Reads a cache from a file.
pub fn read_file(path: &Path) -> Result<BinaryCache, String> {
    let file = fs::File::open(path).map_err(|e| format!("Cannot open the cache: {}", e))?;
    read(file)
}

/// Writes a cache to a file.
pub fn write_file(path: &Path, cache: &BinaryCache) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let file = fs::File::create(path).map_err(|e| format!("Cannot create the cache: {}", e))?;
    write(file, cache)
}

/// Reads a length prefixed UTF-8 string.
fn read_string<R: Read>(reader: &mut R) -> Result<String, String> {
    let mut len = [0u8; 2];
    reader
        .read_exact(&mut len)
        .map_err(|e| format!("Cannot read the string length: {}", e))?;
    let len = u16::from_le_bytes(len) as usize;

    let mut buf = vec![0u8; len];
    reader
        .read_exact(&mut buf)
        .map_err(|e| format!("Cannot read the string: {}", e))?;
    String::from_utf8(buf).map_err(|e| format!("The string is not valid UTF-8: {}", e))
}

/// Writes a length prefixed UTF-8 string.
fn write_string<W: Write>(writer: &mut W, value: &str) -> Result<(), String> {
    let bytes = value.as_bytes();
    let len = u16::try_from(bytes.len()).map_err(|_| "The path is too long".to_string())?;
    writer
        .write_all(&len.to_le_bytes())
        .map_err(|e| format!("Cannot write the string length: {}", e))?;
    writer
        .write_all(bytes)
        .map_err(|e| format!("Cannot write the string: {}", e))
}

/// Reads a little endian u64.
fn read_u64<R: Read>(reader: &mut R) -> Result<u64, String> {
    let mut buf = [0u8; 8];
    reader
        .read_exact(&mut buf)
        .map_err(|e| format!("Cannot read a number: {}", e))?;
    Ok(u64::from_le_bytes(buf))
}

/// Reads a little endian u32.
fn read_u32<R: Read>(reader: &mut R) -> Result<u32, String> {
    let mut buf = [0u8; 4];
    reader
        .read_exact(&mut buf)
        .map_err(|e| format!("Cannot read a number: {}", e))?;
    Ok(u32::from_le_bytes(buf))
}

/// Reads a little endian i32.
fn read_i32<R: Read>(reader: &mut R) -> Result<i32, String> {
    Ok(read_u32(reader)? as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> BinaryCache {
        let mut cache = BinaryCache {
            root: "c:\\temp".to_string(),
            timestamp: 42,
            settings_hash: 7,
            ..Default::default()
        };
        cache.dir_states.insert("c:\\temp\\a".to_string(), 1);
        cache.dir_depths.insert("c:\\temp\\a".to_string(), 1);
        cache.dir_stamps.insert("c:\\temp\\a".to_string(), 12345);
        cache.dir_child_counts.insert("c:\\temp\\a".to_string(), 2);
        cache.all_dirs.insert("c:\\temp\\a".to_string(), 1);
        cache
    }

    #[test]
    fn roundtrip_keeps_everything() {
        let original = sample();
        let mut buffer = Vec::new();
        write(&mut buffer, &original).unwrap();

        let restored = read(&buffer[..]).unwrap();
        assert_eq!(restored.timestamp, original.timestamp);
        assert_eq!(restored.settings_hash, original.settings_hash);
        assert_eq!(restored.dir_states, original.dir_states);
        assert_eq!(restored.dir_depths, original.dir_depths);
        assert_eq!(restored.dir_stamps, original.dir_stamps);
        assert_eq!(restored.dir_child_counts, original.dir_child_counts);
        assert_eq!(restored.all_dirs, original.all_dirs);
    }

    #[test]
    fn a_foreign_file_is_rejected() {
        assert!(read(&b"not a cache"[..]).is_err());
    }

    #[test]
    fn a_truncated_file_is_rejected() {
        let original = sample();
        let mut buffer = Vec::new();
        write(&mut buffer, &original).unwrap();
        buffer.truncate(buffer.len() - 3);
        assert!(read(&buffer[..]).is_err());
    }

    #[test]
    fn a_result_without_a_state_is_still_stored() {
        let mut cache = BinaryCache::default();
        cache.all_dirs.insert("c:\\only".to_string(), 1);

        let mut buffer = Vec::new();
        write(&mut buffer, &cache).unwrap();
        let restored = read(&buffer[..]).unwrap();
        assert_eq!(restored.all_dirs.len(), 1);
    }
}
