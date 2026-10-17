//! BioWare GFF V3.2 reader (and a writer, used to build test fixtures).
//!
//! Layout: a 56-byte header (type, version, then six offset/count pairs for
//! structs, fields, labels, field data, field indices and list indices),
//! followed by those sections. Structs and fields are 12-byte records; a
//! struct with one field stores the field index directly, otherwise an offset
//! into the field-indices array. Lists are `[count, struct indices...]` in the
//! list-indices array.

use std::collections::BTreeMap;

#[derive(Debug, thiserror::Error)]
pub enum GffError {
    #[error("not a GFF V3.x file")]
    BadHeader,
    #[error("truncated or corrupt GFF ({0})")]
    Corrupt(&'static str),
    #[error("unknown field type {0}")]
    UnknownType(u32),
}

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Byte(u8),
    Char(i8),
    Word(u16),
    Short(i16),
    Dword(u32),
    Int(i32),
    Dword64(u64),
    Int64(i64),
    Float(f32),
    Double(f64),
    String(String),
    ResRef(String),
    /// (string ref, [(language id, text)])
    LocString(u32, Vec<(u32, String)>),
    Void(Vec<u8>),
    Struct(Struct),
    List(Vec<Struct>),
    Orientation([f32; 4]),
    Vector([f32; 3]),
    StrRef(u32),
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Struct {
    pub id: u32,
    pub fields: BTreeMap<String, Value>,
}

impl Value {
    pub fn as_i64(&self) -> Option<i64> {
        Some(match *self {
            Value::Byte(v) => v as i64,
            Value::Char(v) => v as i64,
            Value::Word(v) => v as i64,
            Value::Short(v) => v as i64,
            Value::Dword(v) => v as i64,
            Value::Int(v) => v as i64,
            Value::Dword64(v) => v as i64,
            Value::Int64(v) => v,
            _ => return None,
        })
    }

    pub fn as_f32(&self) -> Option<f32> {
        match *self {
            Value::Float(v) => Some(v),
            Value::Double(v) => Some(v as f32),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            Value::String(s) | Value::ResRef(s) => Some(s),
            _ => None,
        }
    }
}

impl Struct {
    pub fn get(&self, label: &str) -> Option<&Value> {
        self.fields.get(label)
    }
    pub fn int(&self, label: &str) -> Option<i64> {
        self.get(label)?.as_i64()
    }
    pub fn float(&self, label: &str) -> Option<f32> {
        self.get(label)?.as_f32()
    }
    pub fn string(&self, label: &str) -> Option<&str> {
        self.get(label)?.as_str()
    }
    pub fn list(&self, label: &str) -> Option<&[Struct]> {
        match self.get(label)? {
            Value::List(l) => Some(l),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Gff {
    pub file_type: String,
    pub root: Struct,
}

struct Reader<'a> {
    d: &'a [u8],
    h: [u32; 12],
}

const MAX_DEPTH: u32 = 64;

impl<'a> Reader<'a> {
    fn u32_at(&self, off: usize) -> Result<u32, GffError> {
        self.d
            .get(off..off + 4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
            .ok_or(GffError::Corrupt("offset out of range"))
    }

    fn bytes(&self, off: usize, len: usize) -> Result<&'a [u8], GffError> {
        self.d
            .get(off..off.checked_add(len).ok_or(GffError::Corrupt("overflow"))?)
            .ok_or(GffError::Corrupt("data out of range"))
    }

    fn section(&self, which: usize) -> usize {
        self.h[which * 2] as usize
    }

    fn field_data(&self, off: u32, len: usize) -> Result<&'a [u8], GffError> {
        if off as usize + len > self.h[7] as usize {
            return Err(GffError::Corrupt("field data out of range"));
        }
        self.bytes(self.section(3) + off as usize, len)
    }

    fn read_struct(&self, index: u32, depth: u32) -> Result<Struct, GffError> {
        if depth > MAX_DEPTH || index >= self.h[1] {
            return Err(GffError::Corrupt("bad struct index"));
        }
        let base = self.section(0) + index as usize * 12;
        let id = self.u32_at(base)?;
        let data = self.u32_at(base + 4)?;
        let count = self.u32_at(base + 8)?;
        let indices: Vec<u32> = match count {
            0 => vec![],
            1 => vec![data],
            n => {
                if data as usize + n as usize * 4 > self.h[9] as usize {
                    return Err(GffError::Corrupt("field indices out of range"));
                }
                (0..n as usize)
                    .map(|i| self.u32_at(self.section(4) + data as usize + i * 4))
                    .collect::<Result<_, _>>()?
            }
        };
        let mut fields = BTreeMap::new();
        for fi in indices {
            let (label, value) = self.read_field(fi, depth)?;
            fields.insert(label, value);
        }
        Ok(Struct { id, fields })
    }

    fn read_field(&self, index: u32, depth: u32) -> Result<(String, Value), GffError> {
        if index >= self.h[3] {
            return Err(GffError::Corrupt("bad field index"));
        }
        let base = self.section(1) + index as usize * 12;
        let ty = self.u32_at(base)?;
        let label_idx = self.u32_at(base + 4)?;
        let data = self.u32_at(base + 8)?;
        if label_idx >= self.h[5] {
            return Err(GffError::Corrupt("bad label index"));
        }
        let raw = self.bytes(self.section(2) + label_idx as usize * 16, 16)?;
        let end = raw.iter().position(|&b| b == 0).unwrap_or(16);
        let label = String::from_utf8_lossy(&raw[..end]).into_owned();

        let fd = |len: usize| self.field_data(data, len);
        let f32s = |n: usize| -> Result<Vec<f32>, GffError> {
            Ok(fd(n * 4)?
                .chunks(4)
                .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
                .collect())
        };
        let lenpref = || -> Result<&'a [u8], GffError> {
            let n = u32::from_le_bytes(fd(4)?.try_into().unwrap()) as usize;
            Ok(&self.field_data(data, 4 + n)?[4..])
        };
        let value = match ty {
            0 => Value::Byte(data as u8),
            1 => Value::Char(data as u8 as i8),
            2 => Value::Word(data as u16),
            3 => Value::Short(data as u16 as i16),
            4 => Value::Dword(data),
            5 => Value::Int(data as i32),
            6 => Value::Dword64(u64::from_le_bytes(fd(8)?.try_into().unwrap())),
            7 => Value::Int64(i64::from_le_bytes(fd(8)?.try_into().unwrap())),
            8 => Value::Float(f32::from_bits(data)),
            9 => Value::Double(f64::from_le_bytes(fd(8)?.try_into().unwrap())),
            10 => Value::String(String::from_utf8_lossy(lenpref()?).into_owned()),
            11 => {
                let n = fd(1)?[0] as usize;
                Value::ResRef(String::from_utf8_lossy(&fd(1 + n)?[1..]).into_owned())
            }
            12 => {
                let body = lenpref()?;
                let rd = |o: usize| -> Result<u32, GffError> {
                    body.get(o..o + 4)
                        .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
                        .ok_or(GffError::Corrupt("locstring"))
                };
                let strref = rd(0)?;
                let count = rd(4)?;
                let mut pos = 8;
                let mut subs = Vec::new();
                for _ in 0..count.min(64) {
                    let id = rd(pos)?;
                    let len = rd(pos + 4)? as usize;
                    let text = body
                        .get(pos + 8..pos + 8 + len)
                        .ok_or(GffError::Corrupt("locstring text"))?;
                    subs.push((id, String::from_utf8_lossy(text).into_owned()));
                    pos += 8 + len;
                }
                Value::LocString(strref, subs)
            }
            13 => Value::Void(lenpref()?.to_vec()),
            14 => Value::Struct(self.read_struct(data, depth + 1)?),
            15 => {
                let off = self.section(5) + data as usize;
                if data as usize + 4 > self.h[11] as usize {
                    return Err(GffError::Corrupt("list out of range"));
                }
                let n = self.u32_at(off)? as usize;
                if data as usize + 4 + n * 4 > self.h[11] as usize {
                    return Err(GffError::Corrupt("list out of range"));
                }
                let mut items = Vec::with_capacity(n);
                for i in 0..n {
                    items.push(self.read_struct(self.u32_at(off + 4 + i * 4)?, depth + 1)?);
                }
                Value::List(items)
            }
            16 => Value::Orientation(f32s(4)?.try_into().unwrap()),
            17 => Value::Vector(f32s(3)?.try_into().unwrap()),
            18 => Value::StrRef(u32::from_le_bytes(fd(8)?[4..8].try_into().unwrap())),
            other => return Err(GffError::UnknownType(other)),
        };
        Ok((label, value))
    }
}

pub fn parse(data: &[u8]) -> Result<Gff, GffError> {
    if data.len() < 56 || &data[4..7] != b"V3." {
        return Err(GffError::BadHeader);
    }
    let mut h = [0u32; 12];
    for (i, v) in h.iter_mut().enumerate() {
        *v = u32::from_le_bytes(data[8 + i * 4..12 + i * 4].try_into().unwrap());
    }
    let r = Reader { d: data, h };
    if h[1] == 0 {
        return Err(GffError::Corrupt("no structs"));
    }
    Ok(Gff {
        file_type: String::from_utf8_lossy(&data[..4]).trim_end().to_owned(),
        root: r.read_struct(0, 0)?,
    })
}

/// Serializes a GFF. Only used to produce fixtures for tests and tools.
pub fn write(file_type: &str, root: &Struct) -> Vec<u8> {
    #[derive(Default)]
    struct W {
        structs: Vec<[u32; 3]>,
        fields: Vec<[u32; 3]>,
        labels: Vec<String>,
        data: Vec<u8>,
        field_idx: Vec<u8>,
        list_idx: Vec<u8>,
    }
    impl W {
        fn label(&mut self, l: &str) -> u32 {
            match self.labels.iter().position(|x| x == l) {
                Some(i) => i as u32,
                None => {
                    self.labels.push(l.to_owned());
                    self.labels.len() as u32 - 1
                }
            }
        }
        fn put(&mut self, bytes: &[u8]) -> u32 {
            let off = self.data.len() as u32;
            self.data.extend_from_slice(bytes);
            off
        }
        fn strukt(&mut self, s: &Struct) -> u32 {
            let idx = self.structs.len();
            self.structs.push([s.id, 0, s.fields.len() as u32]);
            let fis: Vec<u32> = s.fields.iter().map(|(l, v)| self.field(l, v)).collect();
            let data = if fis.len() == 1 {
                fis[0]
            } else {
                let off = self.field_idx.len() as u32;
                for f in &fis {
                    self.field_idx.extend_from_slice(&f.to_le_bytes());
                }
                off
            };
            self.structs[idx][1] = data;
            idx as u32
        }
        fn field(&mut self, label: &str, v: &Value) -> u32 {
            let li = self.label(label);
            let idx = self.fields.len();
            self.fields.push([0, li, 0]);
            let lp = |w: &mut W, b: &[u8]| {
                let mut out = (b.len() as u32).to_le_bytes().to_vec();
                out.extend_from_slice(b);
                w.put(&out)
            };
            let floats = |w: &mut W, f: &[f32]| {
                let b: Vec<u8> = f.iter().flat_map(|x| x.to_le_bytes()).collect();
                w.put(&b)
            };
            let (ty, data) = match v {
                Value::Byte(x) => (0, *x as u32),
                Value::Char(x) => (1, *x as u8 as u32),
                Value::Word(x) => (2, *x as u32),
                Value::Short(x) => (3, *x as u16 as u32),
                Value::Dword(x) => (4, *x),
                Value::Int(x) => (5, *x as u32),
                Value::Dword64(x) => (6, self.put(&x.to_le_bytes())),
                Value::Int64(x) => (7, self.put(&x.to_le_bytes())),
                Value::Float(x) => (8, x.to_bits()),
                Value::Double(x) => (9, self.put(&x.to_le_bytes())),
                Value::String(s) => (10, lp(self, s.as_bytes())),
                Value::ResRef(s) => {
                    let mut b = vec![s.len() as u8];
                    b.extend_from_slice(s.as_bytes());
                    (11, self.put(&b))
                }
                Value::LocString(strref, subs) => {
                    let mut b = strref.to_le_bytes().to_vec();
                    b.extend_from_slice(&(subs.len() as u32).to_le_bytes());
                    for (id, t) in subs {
                        b.extend_from_slice(&id.to_le_bytes());
                        b.extend_from_slice(&(t.len() as u32).to_le_bytes());
                        b.extend_from_slice(t.as_bytes());
                    }
                    (12, lp(self, &b))
                }
                Value::Void(b) => (13, lp(self, b)),
                Value::Struct(s) => (14, self.strukt(s)),
                Value::List(items) => {
                    let idxs: Vec<u32> = items.iter().map(|s| self.strukt(s)).collect();
                    let off = self.list_idx.len() as u32;
                    self.list_idx
                        .extend_from_slice(&(idxs.len() as u32).to_le_bytes());
                    for i in idxs {
                        self.list_idx.extend_from_slice(&i.to_le_bytes());
                    }
                    (15, off)
                }
                Value::Orientation(f) => (16, floats(self, f)),
                Value::Vector(f) => (17, floats(self, f)),
                Value::StrRef(x) => {
                    let mut b = 4u32.to_le_bytes().to_vec();
                    b.extend_from_slice(&x.to_le_bytes());
                    (18, self.put(&b))
                }
            };
            self.fields[idx] = [ty, li, data];
            idx as u32
        }
    }

    let mut w = W::default();
    w.strukt(root);
    let mut out = Vec::new();
    let mut ft = file_type.as_bytes().to_vec();
    ft.resize(4, b' ');
    out.extend_from_slice(&ft);
    out.extend_from_slice(b"V3.2");
    let structs: Vec<u8> = w
        .structs
        .iter()
        .flatten()
        .flat_map(|x| x.to_le_bytes())
        .collect();
    let fields: Vec<u8> = w
        .fields
        .iter()
        .flatten()
        .flat_map(|x| x.to_le_bytes())
        .collect();
    let labels: Vec<u8> = w
        .labels
        .iter()
        .flat_map(|l| {
            let mut b = l.as_bytes().to_vec();
            b.resize(16, 0);
            b
        })
        .collect();
    let sections = [
        (&structs, w.structs.len()),
        (&fields, w.fields.len()),
        (&labels, w.labels.len()),
        (&w.data, w.data.len()),
        (&w.field_idx, w.field_idx.len()),
        (&w.list_idx, w.list_idx.len()),
    ];
    let mut off = 56u32;
    let mut header = Vec::new();
    for (bytes, count) in sections {
        header.extend_from_slice(&off.to_le_bytes());
        header.extend_from_slice(&(count as u32).to_le_bytes());
        off += bytes.len() as u32;
    }
    out.extend_from_slice(&header);
    for (bytes, _) in sections {
        out.extend_from_slice(bytes);
    }
    out
}

/// Readable dump, one field per line, for `dump-gff`.
pub fn dump(s: &Struct, indent: usize, out: &mut String) {
    use std::fmt::Write;
    for (label, v) in &s.fields {
        let pad = " ".repeat(indent);
        match v {
            Value::Struct(inner) => {
                let _ = writeln!(out, "{pad}{label}: struct {}", inner.id);
                dump(inner, indent + 2, out);
            }
            Value::List(items) => {
                let _ = writeln!(out, "{pad}{label}: list[{}]", items.len());
                for (i, item) in items.iter().enumerate() {
                    let _ = writeln!(out, "{pad}  [{i}] struct {}", item.id);
                    dump(item, indent + 4, out);
                }
            }
            Value::Void(b) => {
                let _ = writeln!(out, "{pad}{label}: void[{}]", b.len());
            }
            other => {
                let _ = writeln!(out, "{pad}{label}: {other:?}");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn sample() -> Struct {
        let mut pc = Struct {
            id: 0,
            ..Default::default()
        };
        pc.fields.insert("XPosition".into(), Value::Float(12.5));
        pc.fields
            .insert("CurrentHitPoints".into(), Value::Short(37));
        pc.fields
            .insert("Tag".into(), Value::String("PLAYER".into()));
        let mut root = Struct {
            id: u32::MAX,
            ..Default::default()
        };
        root.fields
            .insert("Mod_PlayerList".into(), Value::List(vec![pc]));
        root.fields
            .insert("Mod_Entry_Area".into(), Value::ResRef("end_m01aa".into()));
        root.fields
            .insert("Pos".into(), Value::Vector([1.0, 2.0, 3.0]));
        root.fields
            .insert("Blob".into(), Value::Void(vec![1, 2, 3]));
        root.fields.insert(
            "Name".into(),
            Value::LocString(u32::MAX, vec![(0, "Trask".into())]),
        );
        root.fields.insert("Big".into(), Value::Int64(-5));
        root.fields.insert(
            "Only".into(),
            Value::Struct(Struct {
                id: 7,
                fields: [("One".to_string(), Value::Byte(1))].into(),
            }),
        );
        root
    }

    #[test]
    fn roundtrip() {
        let root = sample();
        let bytes = write("IFO", &root);
        let gff = parse(&bytes).unwrap();
        assert_eq!(gff.file_type, "IFO");
        assert_eq!(gff.root, root);
        let pc = &gff.root.list("Mod_PlayerList").unwrap()[0];
        assert_eq!(pc.float("XPosition"), Some(12.5));
        assert_eq!(pc.int("CurrentHitPoints"), Some(37));
    }

    #[test]
    fn corrupt_input_is_an_error_not_a_panic() {
        let bytes = write("IFO", &sample());
        for cut in [0, 10, 56, 80, bytes.len() / 2, bytes.len() - 1] {
            let _ = parse(&bytes[..cut]);
        }
        let mut evil = bytes.clone();
        evil[8 + 4] = 0xFF; // struct count huge
        let _ = parse(&evil);
    }
}
