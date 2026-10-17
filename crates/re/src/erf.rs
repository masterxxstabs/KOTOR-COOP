//! BioWare ERF/MOD/SAV archive reader (+ writer for fixtures).
//!
//! Header (160 bytes): type[4], version[4], language count, localized string
//! size, entry count, localized string offset, key list offset, resource list
//! offset, build year, build day, description strref, 116 reserved bytes.
//! Key entries are 24 bytes (resref[16], id, type u16, unused u16); resource
//! entries are 8 bytes (offset, size).

#[derive(Debug, thiserror::Error)]
pub enum ErfError {
    #[error("not an ERF archive")]
    BadHeader,
    #[error("corrupt ERF ({0})")]
    Corrupt(&'static str),
}

pub const TYPE_IFO: u16 = 2014;
pub const TYPE_SAV: u16 = 2057;

#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub res_type: u16,
    offset: usize,
    size: usize,
}

pub struct Erf<'a> {
    data: &'a [u8],
    pub entries: Vec<Entry>,
}

fn u32_at(d: &[u8], o: usize) -> Result<u32, ErfError> {
    d.get(o..o + 4)
        .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
        .ok_or(ErfError::Corrupt("header"))
}

impl<'a> Erf<'a> {
    pub fn parse(data: &'a [u8]) -> Result<Erf<'a>, ErfError> {
        if data.len() < 160 || &data[4..8] != b"V1.0" {
            return Err(ErfError::BadHeader);
        }
        let count = u32_at(data, 16)? as usize;
        let keys = u32_at(data, 24)? as usize;
        let res = u32_at(data, 28)? as usize;
        if count > 1_000_000 {
            return Err(ErfError::Corrupt("entry count"));
        }
        let mut entries = Vec::with_capacity(count);
        for i in 0..count {
            let k = keys + i * 24;
            let raw = data.get(k..k + 16).ok_or(ErfError::Corrupt("key list"))?;
            let end = raw.iter().position(|&b| b == 0).unwrap_or(16);
            let res_type = data
                .get(k + 20..k + 22)
                .map(|b| u16::from_le_bytes(b.try_into().unwrap()))
                .ok_or(ErfError::Corrupt("key list"))?;
            let offset = u32_at(data, res + i * 8)? as usize;
            let size = u32_at(data, res + i * 8 + 4)? as usize;
            if offset.checked_add(size).is_none_or(|e| e > data.len()) {
                return Err(ErfError::Corrupt("resource out of range"));
            }
            entries.push(Entry {
                name: String::from_utf8_lossy(&raw[..end]).into_owned(),
                res_type,
                offset,
                size,
            });
        }
        Ok(Erf { data, entries })
    }

    pub fn get(&self, name: &str, res_type: u16) -> Option<&'a [u8]> {
        self.entries
            .iter()
            .find(|e| e.res_type == res_type && e.name.eq_ignore_ascii_case(name))
            .map(|e| &self.data[e.offset..e.offset + e.size])
    }
}

/// Builds an archive. Used for test fixtures.
pub fn write(file_type: &str, entries: &[(&str, u16, Vec<u8>)]) -> Vec<u8> {
    let n = entries.len();
    let keys = 160;
    let res = keys + n * 24;
    let mut data_off = res + n * 8;
    let mut out = Vec::new();
    let mut ft = file_type.as_bytes().to_vec();
    ft.resize(4, b' ');
    out.extend_from_slice(&ft);
    out.extend_from_slice(b"V1.0");
    for v in [
        0u32,
        0,
        n as u32,
        160,
        keys as u32,
        res as u32,
        0,
        0,
        u32::MAX,
    ] {
        out.extend_from_slice(&v.to_le_bytes());
    }
    out.resize(160, 0);
    for (i, (name, ty, _)) in entries.iter().enumerate() {
        let mut r = name.as_bytes().to_vec();
        r.resize(16, 0);
        out.extend_from_slice(&r);
        out.extend_from_slice(&(i as u32).to_le_bytes());
        out.extend_from_slice(&ty.to_le_bytes());
        out.extend_from_slice(&[0, 0]);
    }
    for (_, _, body) in entries {
        out.extend_from_slice(&(data_off as u32).to_le_bytes());
        out.extend_from_slice(&(body.len() as u32).to_le_bytes());
        data_off += body.len();
    }
    for (_, _, body) in entries {
        out.extend_from_slice(body);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_case_insensitive_lookup() {
        let bytes = write(
            "SAV",
            &[
                ("Module", TYPE_IFO, b"abc".to_vec()),
                ("end_m01aa", TYPE_SAV, b"xy".to_vec()),
            ],
        );
        let erf = Erf::parse(&bytes).unwrap();
        assert_eq!(erf.get("module", TYPE_IFO), Some(&b"abc"[..]));
        assert_eq!(erf.get("END_M01AA", TYPE_SAV), Some(&b"xy"[..]));
        assert!(erf.get("Module", TYPE_SAV).is_none());
        for cut in [0, 100, 170, bytes.len() - 1] {
            let _ = Erf::parse(&bytes[..cut]);
        }
    }
}
