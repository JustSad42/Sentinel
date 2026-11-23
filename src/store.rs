//! Compact on-disk format for the history files: a 4-byte tag, then LZ4-compressed bincode.
//! Reads fall back to the old JSON file once (and the JSON copy is removed after the next save).

use serde::Serialize;
use serde::de::DeserializeOwned;

const TAG: &[u8; 4] = b"SNB1";

fn paths(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = crate::config::data_dir();
    (dir.join(format!("{name}.bin")), dir.join(format!("{name}.json")))
}

pub fn read<T: DeserializeOwned>(name: &str) -> Option<T> {
    let (bin, json) = paths(name);
    if let Ok(b) = std::fs::read(&bin) {
        if b.len() > 4 && &b[..4] == TAG {
            if let Ok(raw) = lz4_flex::decompress_size_prepended(&b[4..]) {
                if let Ok(v) = bincode::deserialize(&raw) {
                    return Some(v);
                }
            }
        }
    }
    std::fs::read(&json).ok().and_then(|b| serde_json::from_slice(&b).ok())
}

pub fn write<T: Serialize + ?Sized>(name: &str, v: &T) {
    let Ok(raw) = bincode::serialize(v) else { return };
    let mut out = Vec::with_capacity(raw.len() / 3 + 8);
    out.extend_from_slice(TAG);
    out.extend_from_slice(&lz4_flex::compress_prepend_size(&raw));
    let (bin, json) = paths(name);
    let tmp = bin.with_extension("bin.tmp");
    if std::fs::write(&tmp, &out).is_ok() && std::fs::rename(&tmp, &bin).is_ok() {
        let _ = std::fs::remove_file(json);
    }
}
