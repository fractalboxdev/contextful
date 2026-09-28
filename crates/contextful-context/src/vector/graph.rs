//! An HNSW graph over unit-length vectors: the seeded builder, the on-disk layout, and a
//! search that reads the layout from any byte slice, memory-mapped or decrypted.
//!
//! Layout, little-endian throughout:
//!
//! ```text
//! header      magic[8] dim count m max_level entry reserved      (u32 each, 32 B)
//! vectors     count * dim f32, unit length
//! levels      count u8, padded to 4 B
//! layer 0     count * (1 + 2m) u32: a neighbour count, then the slots
//! layer l>0   n_l u32, n_l node ids (ascending), n_l * (1 + m) u32 slots
//! ids         (count + 1) u64 offsets into a UTF-8 blob, then the blob
//! ```

use std::cmp::{Ordering, Reverse};
use std::collections::BinaryHeap;

/// The format's magic and version.
pub const MAGIC: &[u8; 8] = b"CFHNSW\x00\x01";

const HEADER_LEN: usize = 32;

/// Layers a node may reach. At `m = 2` the expected top of 2^16 nodes is layer 16.
const MAX_LEVEL: usize = 16;

/// A node and its distance to the point being placed or searched for.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Scored {
    dist: f32,
    node: u32,
}

impl Eq for Scored {}

impl Ord for Scored {
    fn cmp(&self, o: &Self) -> Ordering {
        self.dist.total_cmp(&o.dist).then(self.node.cmp(&o.node))
    }
}

impl PartialOrd for Scored {
    fn partial_cmp(&self, o: &Self) -> Option<Ordering> {
        Some(self.cmp(o))
    }
}

/// SplitMix64: a seeded generator, so one row set and seed build one graph.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// A uniform draw in (0, 1).
    fn unit(&mut self) -> f64 {
        ((self.next() >> 11) as f64 + 0.5) / (1u64 << 53) as f64
    }
}

/// Nodes seen during one layer search, cleared in constant time by an epoch.
struct Visited {
    marks: Vec<u32>,
    epoch: u32,
}

impl Visited {
    fn new(n: usize) -> Visited {
        Visited { marks: vec![0; n], epoch: 0 }
    }

    fn clear(&mut self) {
        self.epoch = self.epoch.wrapping_add(1);
        if self.epoch == 0 {
            self.marks.iter_mut().for_each(|m| *m = 0);
            self.epoch = 1;
        }
    }

    /// Mark `n`, reporting whether it was unmarked.
    fn insert(&mut self, n: u32) -> bool {
        let m = &mut self.marks[n as usize];
        if *m == self.epoch {
            false
        } else {
            *m = self.epoch;
            true
        }
    }
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

/// The `ef` nodes nearest the target reachable from `entry` on one layer, nearest first.
fn search_layer(
    entry: &[Scored],
    ef: usize,
    layer: usize,
    visited: &mut Visited,
    neighbours: &impl Fn(u32, usize, &mut Vec<u32>),
    distance: &impl Fn(u32) -> f32,
) -> Vec<Scored> {
    visited.clear();
    let mut candidates: BinaryHeap<Reverse<Scored>> = BinaryHeap::new();
    let mut found: BinaryHeap<Scored> = BinaryHeap::new();
    for e in entry {
        if visited.insert(e.node) {
            candidates.push(Reverse(*e));
            found.push(*e);
        }
    }
    while found.len() > ef {
        found.pop();
    }
    let mut buf = Vec::new();
    while let Some(Reverse(c)) = candidates.pop() {
        if found.len() >= ef && found.peek().is_some_and(|w| c.dist > w.dist) {
            break;
        }
        buf.clear();
        neighbours(c.node, layer, &mut buf);
        for &n in &buf {
            if !visited.insert(n) {
                continue;
            }
            let d = distance(n);
            if found.len() < ef || found.peek().is_some_and(|w| d < w.dist) {
                let s = Scored { dist: d, node: n };
                candidates.push(Reverse(s));
                found.push(s);
                if found.len() > ef {
                    found.pop();
                }
            }
        }
    }
    let mut out = found.into_vec();
    out.sort();
    out
}

/// A graph under construction over `count` unit vectors of `dim`, held flat.
struct Builder<'a> {
    vectors: &'a [f32],
    dim: usize,
    m: usize,
    ef_construction: usize,
    level_scale: f64,
    levels: Vec<u8>,
    links: Vec<Vec<Vec<u32>>>,
    entry: Option<u32>,
    max_level: usize,
    visited: Visited,
    rng: Rng,
}

impl Builder<'_> {
    fn vector(&self, n: u32) -> &[f32] {
        let i = n as usize * self.dim;
        &self.vectors[i..i + self.dim]
    }

    fn distance(&self, a: u32, b: u32) -> f32 {
        1.0 - dot(self.vector(a), self.vector(b))
    }

    fn cap(&self, layer: usize) -> usize {
        if layer == 0 {
            self.m * 2
        } else {
            self.m
        }
    }

    /// The neighbour-selection heuristic: keep a candidate at least as near the point as to
    /// every neighbour already kept, then fill the remaining slots with the nearest pruned
    /// ones. A tie keeps the candidate, so a point orthogonal to a whole cluster keeps its
    /// links into it and stays reachable.
    fn select(&self, candidates: &[Scored], cap: usize) -> Vec<u32> {
        let mut kept: Vec<Scored> = Vec::with_capacity(cap);
        let mut pruned = Vec::new();
        for c in candidates {
            if kept.len() >= cap {
                break;
            }
            if kept.iter().all(|k| self.distance(c.node, k.node) >= c.dist) {
                kept.push(*c);
            } else {
                pruned.push(*c);
            }
        }
        for p in pruned {
            if kept.len() >= cap {
                break;
            }
            kept.push(p);
        }
        kept.into_iter().map(|s| s.node).collect()
    }

    /// Give every base-layer node an incoming link. Pruning can drop a node from every list
    /// that held it, and a node nothing links to is never found; its nearest neighbour then
    /// links to it, in place of that neighbour's farthest link whose target keeps another.
    fn reconnect(&mut self) {
        let count = self.levels.len();
        let mut incoming = vec![0u32; count];
        for list in &self.links {
            for &o in list.first().into_iter().flatten() {
                incoming[o as usize] += 1;
            }
        }
        let cap = self.cap(0);
        for x in 0..count as u32 {
            if incoming[x as usize] > 0 || Some(x) == self.entry {
                continue;
            }
            let Some(&n) = self.links[x as usize][0].first() else { continue };
            let list = &self.links[n as usize][0];
            if list.len() < cap {
                self.links[n as usize][0].push(x);
                incoming[x as usize] += 1;
                continue;
            }
            let farthest = list
                .iter()
                .enumerate()
                .filter(|(_, &o)| incoming[o as usize] > 1)
                .max_by(|a, b| self.distance(n, *a.1).total_cmp(&self.distance(n, *b.1)))
                .map(|(i, &o)| (i, o));
            if let Some((i, o)) = farthest {
                self.links[n as usize][0][i] = x;
                incoming[o as usize] -= 1;
                incoming[x as usize] += 1;
            }
        }
    }

    fn insert(&mut self, q: u32) {
        let level = ((-self.rng.unit().ln() * self.level_scale).floor() as usize).min(MAX_LEVEL);
        self.levels[q as usize] = level as u8;
        self.links[q as usize] = vec![Vec::new(); level + 1];
        let Some(entry) = self.entry else {
            self.entry = Some(q);
            self.max_level = level;
            return;
        };
        let target = q;
        let mut ep = vec![Scored { dist: self.distance(target, entry), node: entry }];
        let mut visited = std::mem::replace(&mut self.visited, Visited::new(0));
        {
            let links = &self.links;
            let neighbours = |n: u32, l: usize, out: &mut Vec<u32>| {
                if let Some(ls) = links[n as usize].get(l) {
                    out.extend_from_slice(ls);
                }
            };
            let distance = |n: u32| self.distance(target, n);
            for layer in (level + 1..=self.max_level).rev() {
                ep = search_layer(&ep, 1, layer, &mut visited, &neighbours, &distance);
            }
        }
        for layer in (0..=level.min(self.max_level)).rev() {
            let found = {
                let links = &self.links;
                let neighbours = |n: u32, l: usize, out: &mut Vec<u32>| {
                    if let Some(ls) = links[n as usize].get(l) {
                        out.extend_from_slice(ls);
                    }
                };
                let distance = |n: u32| self.distance(target, n);
                search_layer(&ep, self.ef_construction, layer, &mut visited, &neighbours, &distance)
            };
            let cap = self.cap(layer);
            let chosen = self.select(&found, cap);
            for &n in &chosen {
                self.links[n as usize][layer].push(q);
                if self.links[n as usize][layer].len() > cap {
                    let mut around: Vec<Scored> =
                        self.links[n as usize][layer].iter().map(|&o| Scored { dist: self.distance(n, o), node: o }).collect();
                    around.sort();
                    self.links[n as usize][layer] = self.select(&around, cap);
                }
            }
            self.links[q as usize][layer] = chosen;
            ep = found;
        }
        self.visited = visited;
        if level > self.max_level {
            self.max_level = level;
            self.entry = Some(q);
        }
    }
}

/// Build the graph over `vectors` (`count * dim` unit-length floats, row-major) and lay it
/// out with `ids`, one per vector.
pub fn build(vectors: &[f32], dim: usize, ids: &[String], m: usize, ef_construction: usize, seed: u64) -> Vec<u8> {
    let count = ids.len();
    debug_assert_eq!(vectors.len(), count * dim);
    let mut b = Builder {
        vectors,
        dim,
        m,
        ef_construction,
        level_scale: 1.0 / (m as f64).ln(),
        levels: vec![0; count],
        links: vec![Vec::new(); count],
        entry: None,
        max_level: 0,
        visited: Visited::new(count),
        rng: Rng(seed),
    };
    for q in 0..count as u32 {
        b.insert(q);
    }
    b.reconnect();
    lay_out(&b, ids)
}

fn put(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_le_bytes());
}

fn lay_out(b: &Builder<'_>, ids: &[String]) -> Vec<u8> {
    let count = ids.len();
    let mut out = Vec::with_capacity(HEADER_LEN + b.vectors.len() * 4 + count * (1 + 2 * b.m) * 4);
    out.extend_from_slice(MAGIC);
    for v in [b.dim as u32, count as u32, b.m as u32, b.max_level as u32, b.entry.unwrap_or(u32::MAX), 0] {
        put(&mut out, v);
    }
    for f in b.vectors {
        out.extend_from_slice(&f.to_le_bytes());
    }
    out.extend_from_slice(&b.levels);
    out.resize(out.len().next_multiple_of(4), 0);
    for layer in 0..=b.max_level {
        let cap = b.cap(layer);
        let nodes: Vec<u32> = if layer == 0 {
            (0..count as u32).collect()
        } else {
            let on: Vec<u32> = (0..count as u32).filter(|&n| b.levels[n as usize] as usize >= layer).collect();
            put(&mut out, on.len() as u32);
            on.iter().for_each(|&n| put(&mut out, n));
            on
        };
        for n in nodes {
            let ls = b.links[n as usize].get(layer).map(Vec::as_slice).unwrap_or_default();
            put(&mut out, ls.len() as u32);
            for i in 0..cap {
                put(&mut out, ls.get(i).copied().unwrap_or(u32::MAX));
            }
        }
    }
    let mut offset = 0u64;
    out.extend_from_slice(&offset.to_le_bytes());
    for id in ids {
        offset += id.len() as u64;
        out.extend_from_slice(&offset.to_le_bytes());
    }
    for id in ids {
        out.extend_from_slice(id.as_bytes());
    }
    out
}

/// The offsets of one laid-out graph, checked once against its byte length.
#[derive(Debug, Clone)]
pub struct Layout {
    pub dim: usize,
    pub count: usize,
    m: usize,
    max_level: usize,
    entry: u32,
    vectors: usize,
    layer0: usize,
    /// Per upper layer: node count, node-id offset, slot offset.
    upper: Vec<(usize, usize, usize)>,
    id_offsets: usize,
    blob: usize,
}

fn u32_at(bytes: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?))
}

fn u64_at(bytes: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(bytes.get(at..at + 8)?.try_into().ok()?))
}

impl Layout {
    /// Parse the header and check every section fits; `None` for bytes no builder laid out.
    pub fn parse(bytes: &[u8]) -> Option<Layout> {
        if bytes.get(..8)? != MAGIC {
            return None;
        }
        let field = |i: usize| u32_at(bytes, 8 + 4 * i).map(|v| v as usize);
        let (dim, count, m, max_level, entry) = (field(0)?, field(1)?, field(2)?, field(3)?, u32_at(bytes, 24)?);
        if dim == 0 || m < 2 || max_level > MAX_LEVEL || (count > 0 && entry as usize >= count) {
            return None;
        }
        let vectors = HEADER_LEN;
        let levels = vectors.checked_add(count.checked_mul(dim)?.checked_mul(4)?)?;
        let layer0 = levels.checked_add(count)?.next_multiple_of(4);
        let mut at = layer0.checked_add(count.checked_mul(1 + 2 * m)?.checked_mul(4)?)?;
        let mut upper = Vec::with_capacity(max_level);
        for _ in 1..=max_level {
            let n = u32_at(bytes, at)? as usize;
            let ids = at + 4;
            let slots = ids.checked_add(n.checked_mul(4)?)?;
            at = slots.checked_add(n.checked_mul(1 + m)?.checked_mul(4)?)?;
            upper.push((n, ids, slots));
        }
        let id_offsets = at;
        let blob = id_offsets.checked_add(count.checked_add(1)?.checked_mul(8)?)?;
        let blob_len = u64_at(bytes, id_offsets.checked_add(count * 8)?)? as usize;
        if blob.checked_add(blob_len)? != bytes.len() {
            return None;
        }
        Some(Layout { dim, count, m, max_level, entry, vectors, layer0, upper, id_offsets, blob })
    }

    pub fn m(&self) -> usize {
        self.m
    }

    fn distance(&self, bytes: &[u8], query: &[f32], n: u32) -> f32 {
        let at = self.vectors + n as usize * self.dim * 4;
        let row = &bytes[at..at + self.dim * 4];
        1.0 - row.chunks_exact(4).zip(query).map(|(c, q)| f32::from_le_bytes([c[0], c[1], c[2], c[3]]) * q).sum::<f32>()
    }

    fn neighbours(&self, bytes: &[u8], n: u32, layer: usize, out: &mut Vec<u32>) {
        let (slots, cap) = if layer == 0 {
            (self.layer0 + n as usize * (1 + 2 * self.m) * 4, 2 * self.m)
        } else {
            let Some(&(count, ids, slots)) = self.upper.get(layer - 1) else { return };
            let (mut lo, mut hi) = (0usize, count);
            let mut found = None;
            while lo < hi {
                let mid = (lo + hi) / 2;
                match u32_at(bytes, ids + mid * 4).map(|v| v.cmp(&n)) {
                    Some(Ordering::Less) => lo = mid + 1,
                    Some(Ordering::Greater) => hi = mid,
                    Some(Ordering::Equal) => {
                        found = Some(mid);
                        break;
                    }
                    None => return,
                }
            }
            let Some(i) = found else { return };
            (slots + i * (1 + self.m) * 4, self.m)
        };
        let len = u32_at(bytes, slots).map_or(0, |l| (l as usize).min(cap));
        for i in 0..len {
            match u32_at(bytes, slots + 4 + i * 4) {
                Some(v) if (v as usize) < self.count => out.push(v),
                _ => {}
            }
        }
    }

    /// The identifier of node `n`, or `None` for a slice that is not UTF-8.
    pub fn id<'b>(&self, bytes: &'b [u8], n: u32) -> Option<&'b str> {
        let from = u64_at(bytes, self.id_offsets + n as usize * 8)? as usize;
        let to = u64_at(bytes, self.id_offsets + (n as usize + 1) * 8)? as usize;
        std::str::from_utf8(bytes.get(self.blob.checked_add(from)?..self.blob.checked_add(to)?)?).ok()
    }

    /// The `k` nodes nearest `query` (unit length, `dim` wide) with their cosine
    /// similarity, nearest first, searching the base layer `ef` wide.
    pub fn search(&self, bytes: &[u8], query: &[f32], k: usize, ef: usize) -> Vec<(u32, f32)> {
        if self.count == 0 || k == 0 || query.len() != self.dim {
            return Vec::new();
        }
        let mut visited = Visited::new(self.count);
        let neighbours = |n: u32, l: usize, out: &mut Vec<u32>| self.neighbours(bytes, n, l, out);
        let distance = |n: u32| self.distance(bytes, query, n);
        let mut ep = vec![Scored { dist: distance(self.entry), node: self.entry }];
        for layer in (1..=self.max_level).rev() {
            ep = search_layer(&ep, 1, layer, &mut visited, &neighbours, &distance);
        }
        let found = search_layer(&ep, ef.max(k), 0, &mut visited, &neighbours, &distance);
        found.into_iter().take(k).map(|s| (s.node, 1.0 - s.dist)).collect()
    }
}
