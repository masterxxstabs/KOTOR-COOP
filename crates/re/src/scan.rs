//! Value and pointer scanning over a process's memory (Cheat Engine style).

use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Region {
    pub base: usize,
    pub size: usize,
}

/// Read access to a (usually foreign) process.
pub trait Memory {
    /// Committed, readable regions worth scanning (heap + main image).
    fn regions(&self) -> &[Region];
    fn read(&self, addr: usize, buf: &mut [u8]) -> bool;
    /// Base and size of the main executable image; pointer paths start here.
    fn image(&self) -> (usize, usize);

    fn read_u32(&self, addr: usize) -> Option<u32> {
        let mut b = [0u8; 4];
        self.read(addr, &mut b).then(|| u32::from_le_bytes(b))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scalar {
    I16,
    U16,
    I32,
    F32,
}

impl Scalar {
    pub fn size(self) -> usize {
        match self {
            Scalar::I16 | Scalar::U16 => 2,
            Scalar::I32 | Scalar::F32 => 4,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Scalar::I16 => "i16",
            Scalar::U16 => "u16",
            Scalar::I32 => "i32",
            Scalar::F32 => "f32",
        }
    }

    pub fn decode(self, b: &[u8]) -> f64 {
        match self {
            Scalar::I16 => i16::from_le_bytes([b[0], b[1]]) as f64,
            Scalar::U16 => u16::from_le_bytes([b[0], b[1]]) as f64,
            Scalar::I32 => i32::from_le_bytes(b[..4].try_into().unwrap()) as f64,
            Scalar::F32 => f32::from_le_bytes(b[..4].try_into().unwrap()) as f64,
        }
    }

    pub fn read(self, mem: &dyn Memory, addr: usize) -> Option<f64> {
        let mut b = [0u8; 4];
        mem.read(addr, &mut b[..self.size()])
            .then(|| self.decode(&b))
    }
}

const CHUNK: usize = 4 << 20;
/// Chunks overlap so a pattern straddling a chunk boundary is still found.
const OVERLAP: usize = 64;

/// Calls `f(chunk_base, bytes)` over all regions. Bytes in the overlap may be visited twice.
fn for_each_chunk(mem: &dyn Memory, mut f: impl FnMut(usize, &[u8])) {
    let mut buf = vec![0u8; CHUNK + OVERLAP];
    for r in mem.regions() {
        let mut off = 0;
        while off < r.size {
            let len = (r.size - off).min(CHUNK + OVERLAP);
            if mem.read(r.base + off, &mut buf[..len]) {
                f(r.base + off, &buf[..len]);
            }
            off += CHUNK;
        }
    }
}

fn dedup(mut v: Vec<usize>) -> Vec<usize> {
    v.sort_unstable();
    v.dedup();
    v
}

/// Every address where `needle` occurs.
pub fn find_bytes(mem: &dyn Memory, needle: &[u8], limit: usize) -> Vec<usize> {
    let mut hits = Vec::new();
    if needle.is_empty() {
        return hits;
    }
    for_each_chunk(mem, |base, bytes| {
        if hits.len() >= limit {
            return;
        }
        let first = needle[0];
        let mut i = 0;
        while let Some(p) = bytes[i..].iter().position(|&b| b == first) {
            let at = i + p;
            if bytes[at..].starts_with(needle) {
                hits.push(base + at);
            }
            i = at + 1;
            if i + needle.len() > bytes.len() {
                break;
            }
        }
    });
    let mut hits = dedup(hits);
    hits.truncate(limit);
    hits
}

/// 4-byte aligned addresses holding three consecutive floats within `eps` of `v`.
pub fn find_f32x3(mem: &dyn Memory, v: [f32; 3], eps: f32, limit: usize) -> Vec<usize> {
    let mut hits = Vec::new();
    let close = |b: &[u8], i: usize, want: f32| {
        let x = f32::from_le_bytes(b[i..i + 4].try_into().unwrap());
        (x - want).abs() <= eps
    };
    for_each_chunk(mem, |base, bytes| {
        let start = (4 - base % 4) % 4;
        let mut i = start;
        while i + 12 <= bytes.len() && hits.len() < limit {
            if close(bytes, i, v[0]) && close(bytes, i + 4, v[1]) && close(bytes, i + 8, v[2]) {
                hits.push(base + i);
            }
            i += 4;
        }
    });
    let mut hits = dedup(hits);
    hits.truncate(limit);
    hits
}

/// Offsets `d` (|d| <= radius, aligned to the scalar size) such that the
/// value at `center + d` matches `want` for at least one center.
pub fn find_near(
    mem: &dyn Memory,
    centers: &[usize],
    radius: usize,
    ty: Scalar,
    want: f64,
    eps: f64,
) -> Vec<i32> {
    let mut deltas = HashSet::new();
    let step = ty.size();
    for &c in centers.iter().take(256) {
        // Clamp the window to the region holding the center.
        let Some(r) = mem
            .regions()
            .iter()
            .find(|r| c >= r.base && c < r.base + r.size)
        else {
            continue;
        };
        let start = c.saturating_sub(radius).max(r.base);
        let end = (c + radius + 4).min(r.base + r.size);
        let mut buf = vec![0u8; end - start];
        if !mem.read(start, &mut buf) {
            continue;
        }
        let mut i = (step - start % step) % step;
        while i + step <= buf.len() {
            if (ty.decode(&buf[i..]) - want).abs() <= eps {
                deltas.insert((start + i) as i64 - c as i64);
            }
            i += step;
        }
    }
    let mut out: Vec<i32> = deltas.into_iter().map(|d| d as i32).collect();
    out.sort_by_key(|d| (d.unsigned_abs(), *d));
    out
}

/// A pointer path: start at `image + base_rva`; for each offset, read a
/// pointer and add the offset. The result is the value's address.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Chain {
    pub base_rva: u32,
    pub offsets: Vec<u32>,
}

impl Chain {
    pub fn resolve(&self, mem: &dyn Memory) -> Option<usize> {
        let mut addr = mem.image().0.wrapping_add(self.base_rva as usize);
        for off in &self.offsets {
            addr = (mem.read_u32(addr)? as usize).wrapping_add(*off as usize);
        }
        Some(addr)
    }

    /// Same chain, ending `delta` bytes further into the final object.
    pub fn shifted(&self, delta: i32) -> Chain {
        let mut c = self.clone();
        match c.offsets.last_mut() {
            Some(last) => *last = last.wrapping_add(delta as u32),
            None => c.base_rva = c.base_rva.wrapping_add(delta as u32),
        }
        c
    }

    /// Cheat Engine notation, as used by offsets.toml.
    pub fn to_path(&self, module: &str) -> String {
        let mut s = format!("{module}+0x{:X}", self.base_rva);
        for o in &self.offsets {
            s.push_str(&format!(", 0x{o:X}"));
        }
        s
    }

    /// Lower is better: shallow chains with small offsets survive game updates best.
    pub fn cost(&self) -> (usize, u64) {
        (
            self.offsets.len(),
            self.offsets.iter().map(|&o| o as u64).sum(),
        )
    }
}

#[derive(Debug, Clone, Copy)]
pub struct PointerScanParams {
    pub max_depth: usize,
    pub max_offset: u32,
    /// Nodes kept per level; the rest (largest offsets first) are dropped.
    pub max_nodes: usize,
    pub max_results: usize,
}

impl Default for PointerScanParams {
    fn default() -> Self {
        Self {
            max_depth: 4,
            max_offset: 0x2000,
            max_nodes: 200_000,
            max_results: 20_000,
        }
    }
}

/// Finds static pointer paths (rooted in the main image) to any of `targets`.
pub fn pointer_scan(mem: &dyn Memory, targets: &[usize], p: PointerScanParams) -> Vec<Chain> {
    let (img, img_size) = mem.image();
    let in_image = |a: usize| a >= img && a < img + img_size;
    // parent[a] = (child, offset): *(a) + offset == child
    let mut parent: HashMap<usize, (usize, u32)> = HashMap::new();
    let mut visited: HashSet<usize> = targets.iter().copied().collect();
    let mut roots: Vec<usize> = Vec::new();
    let mut current = dedup(targets.to_vec());

    for depth in 0..=p.max_depth {
        for &a in &current {
            if in_image(a) {
                roots.push(a);
            }
        }
        if depth == p.max_depth || roots.len() >= p.max_results {
            break;
        }
        // Static addresses are roots; don't expand them further.
        current.retain(|&a| !in_image(a));
        if current.is_empty() {
            break;
        }
        let mut next: Vec<(usize, usize, u32)> = Vec::new();
        for_each_chunk(mem, |base, bytes| {
            let start = (4 - base % 4) % 4;
            let mut i = start;
            while i + 4 <= bytes.len() {
                let v = u32::from_le_bytes(bytes[i..i + 4].try_into().unwrap()) as usize;
                let k = current.partition_point(|&t| t < v);
                if let Some(&t) = current.get(k) {
                    let off = (t - v) as u32;
                    if off <= p.max_offset {
                        next.push((base + i, t, off));
                    }
                }
                i += 4;
            }
        });
        next.sort_by_key(|&(a, _, off)| (off, a));
        let mut level = Vec::new();
        for (a, child, off) in next {
            if level.len() >= p.max_nodes {
                break;
            }
            if visited.insert(a) {
                parent.insert(a, (child, off));
                level.push(a);
            }
        }
        current = dedup(level);
    }

    let mut chains: Vec<Chain> = roots
        .into_iter()
        .map(|root| {
            let mut offsets = Vec::new();
            let mut a = root;
            while let Some(&(child, off)) = parent.get(&a) {
                offsets.push(off);
                a = child;
            }
            Chain {
                base_rva: (root - img) as u32,
                offsets,
            }
        })
        .collect();
    chains.sort_by_key(|c| (c.cost(), c.base_rva));
    chains.dedup();
    chains.truncate(p.max_results);
    chains
}

/// In-memory fake process for tests.
#[cfg(any(test, feature = "testing"))]
pub mod fake {
    use super::*;

    #[derive(Default)]
    pub struct FakeMemory {
        pub blocks: Vec<(usize, Vec<u8>)>,
        pub image: (usize, usize),
        regions: Vec<Region>,
    }

    impl FakeMemory {
        pub fn new(image_base: usize, image_size: usize) -> Self {
            let mut m = Self {
                image: (image_base, image_size),
                ..Default::default()
            };
            m.add(image_base, vec![0; image_size]);
            m
        }
        pub fn add(&mut self, base: usize, bytes: Vec<u8>) {
            self.regions.push(Region {
                base,
                size: bytes.len(),
            });
            self.blocks.push((base, bytes));
        }
        pub fn write(&mut self, addr: usize, bytes: &[u8]) {
            for (b, data) in &mut self.blocks {
                if addr >= *b && addr + bytes.len() <= *b + data.len() {
                    data[addr - *b..addr - *b + bytes.len()].copy_from_slice(bytes);
                    return;
                }
            }
            panic!("write to unmapped {addr:#x}");
        }
        pub fn write_u32(&mut self, addr: usize, v: u32) {
            self.write(addr, &v.to_le_bytes());
        }
    }

    impl Memory for FakeMemory {
        fn regions(&self) -> &[Region] {
            &self.regions
        }
        fn read(&self, addr: usize, buf: &mut [u8]) -> bool {
            for (b, data) in &self.blocks {
                if addr >= *b && addr + buf.len() <= *b + data.len() {
                    buf.copy_from_slice(&data[addr - *b..addr - *b + buf.len()]);
                    return true;
                }
            }
            false
        }
        fn image(&self) -> (usize, usize) {
            self.image
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fake::FakeMemory;
    use super::*;

    #[test]
    fn finds_two_level_pointer_path() {
        let mut m = FakeMemory::new(0x400000, 0x1000);
        m.add(0x1000_0000, vec![0; 0x2000]); // heap A
        m.add(0x2000_0000, vec![0; 0x2000]); // heap B
                                             // exe+0x200 -> A ; A+0x10 -> B ; B+0x90 = position
        m.write_u32(0x400200, 0x1000_0000);
        m.write_u32(0x1000_0010, 0x2000_0000);
        let pos: Vec<u8> = [1.5f32, -2.0, 3.25]
            .iter()
            .flat_map(|f| f.to_le_bytes())
            .collect();
        m.write(0x2000_0090, &pos);

        let hits = find_f32x3(&m, [1.5, -2.0, 3.25], 0.001, 100);
        assert_eq!(hits, vec![0x2000_0090]);
        let chains = pointer_scan(&m, &hits, PointerScanParams::default());
        let want = Chain {
            base_rva: 0x200,
            offsets: vec![0x10, 0x90],
        };
        assert!(chains.contains(&want), "{chains:x?}");
        assert_eq!(want.resolve(&m), Some(0x2000_0090));
        assert_eq!(want.to_path("swkotor.exe"), "swkotor.exe+0x200, 0x10, 0x90");
        assert_eq!(want.shifted(-0x10).offsets, vec![0x10, 0x80]);
    }

    #[test]
    fn finds_strings_and_nearby_values() {
        let mut m = FakeMemory::new(0x400000, 0x100);
        m.add(0x3000_0000, vec![0; CHUNK + 100]);
        // Straddles the chunk boundary on purpose.
        m.write(0x3000_0000 + CHUNK - 3, b"end_m01aa\0");
        assert_eq!(
            find_bytes(&m, b"end_m01aa\0", 10),
            vec![0x3000_0000 + CHUNK - 3]
        );
        m.write(0x3000_0100, &37i16.to_le_bytes());
        let d = find_near(&m, &[0x3000_0200], 0x200, Scalar::I16, 37.0, 0.0);
        assert_eq!(d, vec![-0x100]);
    }
}

#[cfg(test)]
mod perf {
    use super::fake::FakeMemory;
    use super::*;

    /// `cargo test -p kotor-re --release -- --ignored --nocapture`
    #[test]
    #[ignore]
    fn pointer_scan_on_game_sized_memory() {
        let heap = 0x1000_0000usize;
        let size = 256 << 20;
        let mut bytes = vec![0u8; size];
        // ~25% of words look like pointers into the heap, like a real game heap.
        let mut x: u32 = 12345;
        for w in bytes.chunks_mut(4) {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            if x % 4 == 0 {
                w.copy_from_slice(&((heap + (x as usize % size)) as u32 & !3).to_le_bytes());
            }
        }
        let mut m = FakeMemory::new(0x400000, 0x10_0000);
        m.add(heap, bytes);
        let target = heap + 0x0123_4560;
        m.write_u32(0x400200, (target - 0x90) as u32);
        let t = std::time::Instant::now();
        let chains = pointer_scan(&m, &[target], PointerScanParams::default());
        println!("{} chains in {:.1}s", chains.len(), t.elapsed().as_secs_f32());
        assert!(chains.contains(&Chain { base_rva: 0x200, offsets: vec![0x90] }));
        assert!(t.elapsed().as_secs() < 120);
    }
}
