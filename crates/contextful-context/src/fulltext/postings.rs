//! The full-text sidecar's one file, `postings.bin` (`store.index.postings`): a header, a
//! document table, the identifiers, a term dictionary sorted by term bytes, the terms, and
//! per term its postings. Every integer is little-endian; a reader binary-searches the
//! dictionary in place, over mapped or decrypted bytes, and decodes only the postings of
//! the terms it asks for.
//!
//! ```text
//! header    b"CFPOST01", doc_count u32, term_count u32, total_terms u64,
//!           docs, ids, dict, terms, postings, end: u64 offsets
//! docs      doc_count x (id_start u64, id_len u32, length u32)       id_start from `ids`
//! dict      term_count x (term_start u64, term_len u32, df u32,
//!                         postings_start u64, postings_len u64)     starts from their blobs
//! postings  per term, df x (doc u32, tf u32, tf x position u32), docs ascending
//! ```

use std::collections::BTreeMap;

const MAGIC: &[u8; 8] = b"CFPOST01";
const HEADER: usize = 8 + 4 + 4 + 8 + 6 * 8;
const DOC: usize = 16;
const TERM: usize = 32;

/// Lay out the postings of `docs`, each an identifier and its terms in position order. One
/// document list lays out one byte sequence.
pub fn build(docs: &[(String, Vec<String>)]) -> Vec<u8> {
    let mut terms: BTreeMap<&str, Vec<(u32, Vec<u32>)>> = BTreeMap::new();
    let mut total: u64 = 0;
    for (d, (_, doc_terms)) in docs.iter().enumerate() {
        total += doc_terms.len() as u64;
        for (p, t) in doc_terms.iter().enumerate() {
            let list = terms.entry(t.as_str()).or_default();
            match list.last_mut() {
                Some((doc, positions)) if *doc == d as u32 => positions.push(p as u32),
                _ => list.push((d as u32, vec![p as u32])),
            }
        }
    }

    let mut doc_table = Vec::with_capacity(docs.len() * DOC);
    let mut ids: Vec<u8> = Vec::new();
    for (id, doc_terms) in docs {
        doc_table.extend((ids.len() as u64).to_le_bytes());
        doc_table.extend((id.len() as u32).to_le_bytes());
        doc_table.extend((doc_terms.len() as u32).to_le_bytes());
        ids.extend(id.as_bytes());
    }
    let mut dict = Vec::with_capacity(terms.len() * TERM);
    let mut term_bytes: Vec<u8> = Vec::new();
    let mut postings: Vec<u8> = Vec::new();
    for (term, list) in &terms {
        let start = postings.len() as u64;
        for (doc, positions) in list {
            postings.extend(doc.to_le_bytes());
            postings.extend((positions.len() as u32).to_le_bytes());
            for p in positions {
                postings.extend(p.to_le_bytes());
            }
        }
        dict.extend((term_bytes.len() as u64).to_le_bytes());
        dict.extend((term.len() as u32).to_le_bytes());
        dict.extend((list.len() as u32).to_le_bytes());
        dict.extend(start.to_le_bytes());
        dict.extend((postings.len() as u64 - start).to_le_bytes());
        term_bytes.extend(term.as_bytes());
    }

    let docs_at = HEADER as u64;
    let ids_at = docs_at + doc_table.len() as u64;
    let dict_at = ids_at + ids.len() as u64;
    let terms_at = dict_at + dict.len() as u64;
    let postings_at = terms_at + term_bytes.len() as u64;
    let end = postings_at + postings.len() as u64;
    let mut out = Vec::with_capacity(end as usize);
    out.extend(MAGIC);
    out.extend((docs.len() as u32).to_le_bytes());
    out.extend((terms.len() as u32).to_le_bytes());
    out.extend(total.to_le_bytes());
    for at in [docs_at, ids_at, dict_at, terms_at, postings_at, end] {
        out.extend(at.to_le_bytes());
    }
    out.extend(doc_table);
    out.extend(ids);
    out.extend(dict);
    out.extend(term_bytes);
    out.extend(postings);
    out
}

fn u32_at(b: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(b.get(at..at.checked_add(4)?)?.try_into().ok()?))
}

fn u64_at(b: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(b.get(at..at.checked_add(8)?)?.try_into().ok()?))
}

/// Where each region of a laid-out file sits. Parsing checks the regions tile the file; a
/// read past a region's end within it returns `None`, never panics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub doc_count: usize,
    pub term_count: usize,
    pub total_terms: u64,
    docs: usize,
    ids: usize,
    dict: usize,
    terms: usize,
    postings: usize,
    end: usize,
}

/// A dictionary entry that does not read: an offset or length past its region.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Malformed;

/// One dictionary entry: how many documents hold the term, and where its postings sit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TermEntry {
    pub df: u32,
    start: usize,
    len: usize,
}

/// One document's occurrences of a term.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Posting {
    pub doc: u32,
    pub positions: Vec<u32>,
}

impl Layout {
    pub fn parse(b: &[u8]) -> Option<Layout> {
        if b.get(..8)? != MAGIC {
            return None;
        }
        let doc_count = u32_at(b, 8)? as usize;
        let term_count = u32_at(b, 12)? as usize;
        let total_terms = u64_at(b, 16)?;
        let at = |i: usize| u64_at(b, 24 + 8 * i).and_then(|v| usize::try_from(v).ok());
        let l = Layout {
            doc_count,
            term_count,
            total_terms,
            docs: at(0)?,
            ids: at(1)?,
            dict: at(2)?,
            terms: at(3)?,
            postings: at(4)?,
            end: at(5)?,
        };
        let tiles = l.docs == HEADER
            && l.ids == l.docs.checked_add(doc_count.checked_mul(DOC)?)?
            && l.ids <= l.dict
            && l.terms == l.dict.checked_add(term_count.checked_mul(TERM)?)?
            && l.terms <= l.postings
            && l.postings <= l.end
            && l.end == b.len();
        tiles.then_some(l)
    }

    /// Document `d`'s identifier.
    pub fn id<'b>(&self, b: &'b [u8], d: u32) -> Option<&'b str> {
        let row = self.docs + d as usize * DOC;
        let start = self.ids.checked_add(usize::try_from(u64_at(b, row)?).ok()?)?;
        let len = u32_at(b, row + 8)? as usize;
        let end = start.checked_add(len)?;
        (end <= self.dict).then_some(())?;
        std::str::from_utf8(b.get(start..end)?).ok()
    }

    /// Document `d`'s length in terms.
    pub fn length(&self, b: &[u8], d: u32) -> Option<u32> {
        ((d as usize) < self.doc_count).then_some(())?;
        u32_at(b, self.docs + d as usize * DOC + 12)
    }

    fn term_at<'b>(&self, b: &'b [u8], i: usize) -> Option<&'b [u8]> {
        let row = self.dict + i * TERM;
        let start = self.terms.checked_add(usize::try_from(u64_at(b, row)?).ok()?)?;
        let end = start.checked_add(u32_at(b, row + 8)? as usize)?;
        (end <= self.postings).then_some(())?;
        b.get(start..end)
    }

    /// The dictionary entry of `term`, found by binary search; `Ok(None)` when absent.
    pub fn lookup(&self, b: &[u8], term: &str) -> Result<Option<TermEntry>, Malformed> {
        let (mut lo, mut hi) = (0usize, self.term_count);
        while lo < hi {
            let mid = lo + (hi - lo) / 2;
            match self.term_at(b, mid).ok_or(Malformed)?.cmp(term.as_bytes()) {
                std::cmp::Ordering::Less => lo = mid + 1,
                std::cmp::Ordering::Greater => hi = mid,
                std::cmp::Ordering::Equal => {
                    let row = self.dict + mid * TERM;
                    let df = u32_at(b, row + 12).ok_or(Malformed)?;
                    let start = usize::try_from(u64_at(b, row + 16).ok_or(Malformed)?).map_err(|_| Malformed)?;
                    let len = usize::try_from(u64_at(b, row + 24).ok_or(Malformed)?).map_err(|_| Malformed)?;
                    let start = self.postings.checked_add(start).ok_or(Malformed)?;
                    (start.checked_add(len).ok_or(Malformed)? <= self.end).then_some(()).ok_or(Malformed)?;
                    return Ok(Some(TermEntry { df, start, len }));
                }
            }
        }
        Ok(None)
    }

    /// Decode a term's postings, documents ascending.
    pub fn postings(&self, b: &[u8], e: TermEntry) -> Option<Vec<Posting>> {
        let bytes = b.get(e.start..e.start + e.len)?;
        let mut out = Vec::with_capacity(e.df as usize);
        let mut at = 0;
        for _ in 0..e.df {
            let doc = u32_at(bytes, at)?;
            let tf = u32_at(bytes, at + 4)? as usize;
            at += 8;
            let positions = (0..tf).map(|i| u32_at(bytes, at + 4 * i)).collect::<Option<Vec<u32>>>()?;
            at += 4 * tf;
            if doc as usize >= self.doc_count {
                return None;
            }
            out.push(Posting { doc, positions });
        }
        (at == bytes.len()).then_some(out)
    }
}
