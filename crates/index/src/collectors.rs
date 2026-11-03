//! Custom collectors reading FAST columns directly (no doc-store access).

use std::collections::HashMap;

use tantivy::collector::{Collector, SegmentCollector};
use tantivy::columnar::{Column, StrColumn};
use tantivy::{DocId, Score, SegmentOrdinal, SegmentReader};

fn u64_col(reader: &SegmentReader, name: &str) -> tantivy::Result<Option<Column<u64>>> {
    reader.fast_fields().column_opt::<u64>(name)
}

/// Resolves `(term ordinal, count)` pairs of a string column to `value → count`, in one
/// sorted pass over the dictionary (per-ordinal lookups re-decode a block each time).
fn ord_strings(col: &StrColumn, counts: &mut [(u64, u64)]) -> HashMap<String, u64> {
    counts.sort_unstable_by_key(|&(ord, _)| ord);
    let ords: Vec<u64> = counts.iter().map(|&(ord, _)| ord).collect();
    let mut out = HashMap::with_capacity(counts.len());
    let mut i = 0;
    let _ = col.dictionary().sorted_ords_to_term_cb(&ords, |term| {
        if let (Some(&(_, c)), Ok(s)) = (counts.get(i), std::str::from_utf8(term)) {
            *out.entry(s.to_string()).or_insert(0) += c;
        }
        i += 1;
    });
    out
}

// ---------------------------------------------------------------------------------------------

/// Total hit count plus sender / year / source distributions of the matched set.
pub(crate) struct FacetCollector;

/// Distinct senders resolved to strings per segment. Exact whenever a segment's matches
/// span at most this many senders; beyond that only the per-segment leaders compete for
/// the global top 20 (the usual shard-size approximation of terms aggregations).
const SENDERS_PER_SEGMENT: usize = 256;

#[derive(Default)]
pub(crate) struct Facets {
    pub total: u64,
    pub from: HashMap<String, u64>,
    pub year: HashMap<u64, u64>,
    pub source: HashMap<u64, u64>,
}

/// Per-value counts for a u64 column: a dense array over `[min, max]` when that span is
/// small (the normal case for years and source ids), a hash map otherwise.
struct Counter {
    col: Option<Column<u64>>,
    min: u64,
    dense: Vec<u64>,
    sparse: HashMap<u64, u64>,
}

impl Counter {
    fn new(col: Option<Column<u64>>) -> Self {
        let (min, span) = col.as_ref().map_or((0, 0), |c| (c.min_value(), c.max_value().saturating_sub(c.min_value())));
        let dense = if col.is_some() && span < 1 << 16 { vec![0; span as usize + 1] } else { Vec::new() };
        Counter { col, min, dense, sparse: HashMap::new() }
    }

    #[inline]
    fn add(&mut self, doc: DocId) {
        let Some(v) = self.col.as_ref().and_then(|c| c.first(doc)) else { return };
        match self.dense.get_mut(v.wrapping_sub(self.min) as usize) {
            Some(slot) => *slot += 1,
            None => *self.sparse.entry(v).or_insert(0) += 1,
        }
    }

    fn into_map(self) -> HashMap<u64, u64> {
        let mut out = self.sparse;
        for (i, &c) in self.dense.iter().enumerate() {
            if c > 0 {
                *out.entry(self.min + i as u64).or_insert(0) += c;
            }
        }
        out
    }
}

pub(crate) struct FacetSegment {
    total: u64,
    source: Counter,
    year: Counter,
    from_col: Option<StrColumn>,
    from_counts: Vec<u64>,
}

impl Collector for FacetCollector {
    type Fruit = Facets;
    type Child = FacetSegment;

    fn for_segment(&self, _ord: SegmentOrdinal, reader: &SegmentReader) -> tantivy::Result<FacetSegment> {
        let from_col = reader.fast_fields().str("from_addr")?;
        let from_counts = vec![0; from_col.as_ref().map_or(0, |c| c.num_terms())];
        Ok(FacetSegment {
            total: 0,
            source: Counter::new(u64_col(reader, "source")?),
            year: Counter::new(u64_col(reader, "year")?),
            from_col,
            from_counts,
        })
    }

    fn requires_scoring(&self) -> bool {
        false
    }

    fn merge_fruits(&self, fruits: Vec<Facets>) -> tantivy::Result<Facets> {
        let mut out = Facets::default();
        for f in fruits {
            out.total += f.total;
            for (k, v) in f.from {
                *out.from.entry(k).or_insert(0) += v;
            }
            for (k, v) in f.year {
                *out.year.entry(k).or_insert(0) += v;
            }
            for (k, v) in f.source {
                *out.source.entry(k).or_insert(0) += v;
            }
        }
        Ok(out)
    }
}

impl SegmentCollector for FacetSegment {
    type Fruit = Facets;

    fn collect(&mut self, doc: DocId, _score: Score) {
        self.total += 1;
        self.source.add(doc);
        self.year.add(doc);
        if let Some(ord) = self.from_col.as_ref().and_then(|c| c.ords().first(doc)) {
            self.from_counts[ord as usize] += 1;
        }
    }

    fn harvest(self) -> Facets {
        let from = match &self.from_col {
            Some(col) => {
                let mut nonzero: Vec<(u64, u64)> = self
                    .from_counts
                    .iter()
                    .enumerate()
                    .filter(|(_, &c)| c > 0)
                    .map(|(ord, &c)| (ord as u64, c))
                    .collect();
                if nonzero.len() > SENDERS_PER_SEGMENT {
                    nonzero.select_nth_unstable_by(SENDERS_PER_SEGMENT, |a, b| b.1.cmp(&a.1));
                    nonzero.truncate(SENDERS_PER_SEGMENT);
                }
                ord_strings(col, &mut nonzero)
            }
            None => HashMap::new(),
        };
        Facets { total: self.total, from, year: self.year.into_map(), source: self.source.into_map() }
    }
}

// ---------------------------------------------------------------------------------------------

/// Distinct values of a FAST string field over the matched documents (first value per doc).
pub(crate) struct StrValues(pub &'static str);

pub(crate) struct StrValuesSegment {
    col: Option<StrColumn>,
    ords: Vec<u64>,
}

impl Collector for StrValues {
    type Fruit = Vec<String>;
    type Child = StrValuesSegment;

    fn for_segment(&self, _ord: SegmentOrdinal, reader: &SegmentReader) -> tantivy::Result<StrValuesSegment> {
        Ok(StrValuesSegment { col: reader.fast_fields().str(self.0)?, ords: Vec::new() })
    }

    fn requires_scoring(&self) -> bool {
        false
    }

    fn merge_fruits(&self, fruits: Vec<Vec<String>>) -> tantivy::Result<Vec<String>> {
        Ok(fruits.into_iter().flatten().collect())
    }
}

impl SegmentCollector for StrValuesSegment {
    type Fruit = Vec<String>;

    fn collect(&mut self, doc: DocId, _score: Score) {
        if let Some(ord) = self.col.as_ref().and_then(|c| c.ords().first(doc)) {
            self.ords.push(ord);
        }
    }

    fn harvest(mut self) -> Vec<String> {
        let Some(col) = &self.col else { return Vec::new() };
        // One sorted dictionary pass instead of a block decode per document.
        self.ords.sort_unstable();
        self.ords.dedup();
        let mut out = Vec::with_capacity(self.ords.len());
        let _ = col.dictionary().sorted_ords_to_term_cb(&self.ords, |term| {
            out.push(String::from_utf8_lossy(term).into_owned());
        });
        out
    }
}

// ---------------------------------------------------------------------------------------------

/// Matched-document count per value of a FAST string field (first value per doc), for
/// fields with few distinct values.
pub(crate) struct StrCounts(pub &'static str);

pub(crate) struct StrCountsSegment {
    col: Option<StrColumn>,
    /// Indexed by term ordinal.
    counts: Vec<u64>,
}

impl Collector for StrCounts {
    type Fruit = HashMap<String, u64>;
    type Child = StrCountsSegment;

    fn for_segment(&self, _ord: SegmentOrdinal, reader: &SegmentReader) -> tantivy::Result<StrCountsSegment> {
        let col = reader.fast_fields().str(self.0)?;
        let counts = vec![0; col.as_ref().map_or(0, |c| c.num_terms())];
        Ok(StrCountsSegment { col, counts })
    }

    fn requires_scoring(&self) -> bool {
        false
    }

    fn merge_fruits(&self, fruits: Vec<HashMap<String, u64>>) -> tantivy::Result<HashMap<String, u64>> {
        let mut out = HashMap::new();
        for (k, v) in fruits.into_iter().flatten() {
            *out.entry(k).or_insert(0) += v;
        }
        Ok(out)
    }
}

impl SegmentCollector for StrCountsSegment {
    type Fruit = HashMap<String, u64>;

    fn collect(&mut self, doc: DocId, _score: Score) {
        if let Some(ord) = self.col.as_ref().and_then(|c| c.ords().first(doc)) {
            self.counts[ord as usize] += 1;
        }
    }

    fn harvest(self) -> HashMap<String, u64> {
        let Some(col) = &self.col else { return HashMap::new() };
        let mut nonzero: Vec<(u64, u64)> =
            self.counts.iter().enumerate().filter(|(_, &c)| c > 0).map(|(ord, &c)| (ord as u64, c)).collect();
        ord_strings(col, &mut nonzero)
    }
}
