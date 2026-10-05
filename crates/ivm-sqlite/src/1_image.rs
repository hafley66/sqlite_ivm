//! Installed-image cache for shard schemas. The shard DDL of an install is a pure function of the
//! plan's shape (node tables carry no relation names, literals or rows), so the same text always
//! yields the same empty schemas. The first install of a text runs it and keeps
//! `sqlite3_serialize` of every shard; later installs `sqlite3_deserialize` those bytes instead
//! of running thousands of CREATEs.
//!
//! Two layers: a process map, always on, and a directory named by `IVM_SQLITE_IMAGE_CACHE`
//! (one file per key, written through a rename). Every miss and every disk failure logs at
//! `ivm_sqlite::image` and falls to running the DDL; nothing is skipped silently.

use crate::meter::Meter;
use sqlite_ext::rusqlite::{self, Connection};
#[cfg(feature = "image")]
use sqlite_ext::rusqlite::ffi;
#[cfg(feature = "image")]
use std::{
    collections::HashMap,
    hash::{Hash, Hasher},
    sync::{Arc, Mutex, OnceLock},
};

#[cfg(feature = "image")]
const TARGET: &str = "ivm_sqlite::image";
#[cfg(feature = "image")]
const MAGIC: &[u8; 8] = b"ivmimg01";

#[cfg(feature = "image")]
/// The DDL text the images were built from, kept to compare on every hit, and one image per shard.
struct Image {
    ddl: String,
    shards: Vec<Vec<u8>>,
}

#[cfg(feature = "image")]
fn memory() -> &'static Mutex<HashMap<u64, Arc<Image>>> {
    static MEMORY: OnceLock<Mutex<HashMap<u64, Arc<Image>>>> = OnceLock::new();
    MEMORY.get_or_init(Default::default)
}

#[cfg(feature = "image")]
fn key(shards: usize, ddl: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    (rusqlite::version(), shards, ddl).hash(&mut hasher);
    hasher.finish()
}

#[cfg(feature = "image")]
fn path(key: u64) -> Option<std::path::PathBuf> {
    std::env::var_os("IVM_SQLITE_IMAGE_CACHE").map(|dir| std::path::Path::new(&dir).join(format!("{key:016x}.img")))
}

#[cfg(feature = "image")]
fn encode(image: &Image) -> Vec<u8> {
    let mut out = MAGIC.to_vec();
    out.extend((image.ddl.len() as u64).to_le_bytes());
    out.extend(image.ddl.as_bytes());
    out.extend((image.shards.len() as u64).to_le_bytes());
    for shard in &image.shards {
        out.extend((shard.len() as u64).to_le_bytes());
        out.extend(shard);
    }
    out
}

#[cfg(feature = "image")]
fn decode(bytes: &[u8]) -> Option<Image> {
    fn take<'a>(bytes: &'a [u8], at: &mut usize, n: usize) -> Option<&'a [u8]> {
        let part = bytes.get(*at..*at + n)?;
        *at += n;
        Some(part)
    }
    fn len(bytes: &[u8], at: &mut usize) -> Option<usize> {
        Some(u64::from_le_bytes(take(bytes, at, 8)?.try_into().ok()?) as usize)
    }
    let mut at = 0;
    if take(bytes, &mut at, MAGIC.len())? != MAGIC { return None; }
    let ddl_len = len(bytes, &mut at)?;
    let ddl = String::from_utf8(take(bytes, &mut at, ddl_len)?.to_vec()).ok()?;
    let count = len(bytes, &mut at)?;
    let mut shards = Vec::with_capacity(count);
    for _ in 0..count {
        let n = len(bytes, &mut at)?;
        shards.push(take(bytes, &mut at, n)?.to_vec());
    }
    Some(Image { ddl, shards })
}

#[cfg(feature = "image")]
fn from_disk(key: u64, ddl: &str, shards: usize) -> Option<Image> {
    let path = path(key)?;
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            tracing::warn!(target: TARGET, path = %path.display(), error = %e, "image cache read failed");
            return None;
        }
    };
    match decode(&bytes) {
        Some(image) if image.ddl == ddl && image.shards.len() == shards => Some(image),
        _ => {
            tracing::warn!(target: TARGET, path = %path.display(), "image cache file does not match its key");
            None
        }
    }
}

#[cfg(feature = "image")]
fn to_disk(key: u64, image: &Image) {
    let Some(path) = path(key) else { return };
    let temp = path.with_extension(format!("img.{}", std::process::id()));
    let written = path.parent().map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::write(&temp, encode(image)))
        .and_then(|()| std::fs::rename(&temp, &path));
    if let Err(e) = written {
        let _ = std::fs::remove_file(&temp);
        tracing::warn!(target: TARGET, path = %path.display(), error = %e, "image cache write failed");
    }
}

#[cfg(feature = "image")]
fn serialize(conn: &Connection, schema: &str) -> rusqlite::Result<Vec<u8>> {
    let name = std::ffi::CString::new(schema).map_err(rusqlite::Error::NulError)?;
    let mut size: ffi::sqlite3_int64 = 0;
    let data = unsafe { ffi::sqlite3_serialize(conn.handle(), name.as_ptr(), &mut size, 0) };
    if data.is_null() {
        return Err(rusqlite::Error::SqliteFailure(ffi::Error::new(ffi::SQLITE_NOMEM), Some(format!("serialize {schema}"))));
    }
    let bytes = unsafe { std::slice::from_raw_parts(data, size as usize) }.to_vec();
    unsafe { ffi::sqlite3_free(data.cast()) };
    Ok(bytes)
}

#[cfg(feature = "image")]
fn deserialize(conn: &Connection, schema: &str, bytes: &[u8]) -> rusqlite::Result<()> {
    let name = std::ffi::CString::new(schema).map_err(rusqlite::Error::NulError)?;
    let data = unsafe { ffi::sqlite3_malloc64(bytes.len() as u64) }.cast::<u8>();
    if data.is_null() {
        return Err(rusqlite::Error::SqliteFailure(ffi::Error::new(ffi::SQLITE_NOMEM), Some(format!("deserialize {schema}"))));
    }
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), data, bytes.len()) };
    let size = bytes.len() as ffi::sqlite3_int64;
    let flags = (ffi::SQLITE_DESERIALIZE_FREEONCLOSE | ffi::SQLITE_DESERIALIZE_RESIZEABLE) as std::ffi::c_uint;
    let rc = unsafe { ffi::sqlite3_deserialize(conn.handle(), name.as_ptr(), data, size, size, flags) };
    if rc != ffi::SQLITE_OK {
        return Err(rusqlite::Error::SqliteFailure(ffi::Error::new(rc), Some(format!("deserialize {schema}"))));
    }
    Ok(())
}

/// Without the `image` feature every install runs its shard DDL.
#[cfg(not(feature = "image"))]
pub(crate) fn install_shards(conn: &Connection, _shards: usize, ddl: &str, meter: &mut Meter) -> rusqlite::Result<()> {
    meter.exec_text(conn, "install", "shards", ddl)
}

#[cfg(not(feature = "image"))]
pub fn clear_memory() {}

/// Fills the attached shard schemas `ivm_s0..ivm_s{shards}` with the objects `ddl` creates.
#[cfg(feature = "image")]
pub(crate) fn install_shards(conn: &Connection, shards: usize, ddl: &str, meter: &mut Meter) -> rusqlite::Result<()> {
    let key = key(shards, ddl);
    let cached = memory().lock().unwrap().get(&key).filter(|image| image.ddl == ddl).cloned();
    let (cached, layer) = match cached {
        Some(image) => (Some(image), "memory"),
        None => match from_disk(key, ddl, shards) {
            Some(image) => {
                let image = Arc::new(image);
                memory().lock().unwrap().insert(key, image.clone());
                (Some(image), "disk")
            }
            None => (None, "none"),
        },
    };
    if let Some(image) = cached {
        for (shard, bytes) in image.shards.iter().enumerate() {
            deserialize(conn, &crate::catalog::shard_schema(shard), bytes)?;
        }
        tracing::debug!(target: TARGET, key = format_args!("{key:016x}"), layer, bytes = image.shards.iter().map(Vec::len).sum::<usize>(), "image cache hit");
        return Ok(());
    }
    tracing::info!(target: TARGET, key = format_args!("{key:016x}"), ddl_bytes = ddl.len(), disk = path(key).is_some(), "image cache miss");
    meter.exec_text(conn, "install", "shards", ddl)?;
    let image = Image {
        ddl: ddl.to_owned(),
        shards: (0..shards).map(|shard| serialize(conn, &crate::catalog::shard_schema(shard))).collect::<rusqlite::Result<_>>()?,
    };
    to_disk(key, &image);
    memory().lock().unwrap().insert(key, Arc::new(image));
    Ok(())
}

/// Drops the process layer; the next install of each text reads the directory or runs its DDL.
#[cfg(feature = "image")]
pub fn clear_memory() {
    memory().lock().unwrap().clear();
}
