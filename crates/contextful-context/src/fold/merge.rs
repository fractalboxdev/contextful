//! The streaming merge a pass runs over parts already sorted by the table's sort columns:
//! it holds one record batch per input and one output batch, so its memory does not grow
//! with the rows the inputs hold (`store.fold.bounded-memory`).

use crate::error::{ContextError, Result};
use crate::parquet_io::StreamWriter;
use arrow_array::RecordBatch;
use arrow_ord::sort::SortOptions;
use arrow_row::{RowConverter, Rows, SortField};
use arrow_schema::Schema as ArrowSchema;
use arrow_select::interleave::interleave_record_batch;
use std::sync::Arc;

/// Rows one merged output batch holds.
const OUT_BATCH: usize = 8192;

/// One input's position: its remaining batches, the batch in hand with its sort rows, and
/// the next row of that batch.
struct Cursor<I> {
    batches: I,
    batch: Option<(RecordBatch, Rows)>,
    at: usize,
}

fn invalid(e: arrow_schema::ArrowError) -> ContextError {
    ContextError::Invalid(format!("merging parts: {e}"))
}

impl<I: Iterator<Item = Result<RecordBatch>>> Cursor<I> {
    /// Advance to the next non-empty batch, or to none once the input ends.
    fn load(&mut self, converter: &RowConverter, sort: &[String]) -> Result<()> {
        self.at = 0;
        self.batch = None;
        for batch in self.batches.by_ref() {
            let batch = batch?;
            if batch.num_rows() == 0 {
                continue;
            }
            let columns = sort
                .iter()
                .map(|c| batch.column_by_name(c).cloned().ok_or_else(|| ContextError::Invalid(format!("merging parts: no column `{c}`"))))
                .collect::<Result<Vec<_>>>()?;
            let rows = converter.convert_columns(&columns).map_err(invalid)?;
            self.batch = Some((batch, rows));
            break;
        }
        Ok(())
    }
}

/// Merge `inputs`, each a stream of batches conformed to `target` and sorted ascending by
/// `sort` with nulls last, into `out` in that order; with no sort columns the inputs follow
/// one another. Returns the rows written.
pub(crate) fn merge_into<I>(inputs: Vec<I>, target: &Arc<ArrowSchema>, sort: &[String], out: &mut StreamWriter) -> Result<u64>
where
    I: Iterator<Item = Result<RecordBatch>>,
{
    let mut written = 0u64;
    if sort.is_empty() {
        for input in inputs {
            for batch in input {
                let batch = batch?;
                written += batch.num_rows() as u64;
                out.write(&batch)?;
            }
        }
        return Ok(written);
    }
    let options = SortOptions { descending: false, nulls_first: false };
    let fields = sort
        .iter()
        .map(|c| {
            let f = target.field_with_name(c).map_err(invalid)?;
            Ok(SortField::new_with_options(f.data_type().clone(), options))
        })
        .collect::<Result<Vec<_>>>()?;
    let converter = RowConverter::new(fields).map_err(invalid)?;
    let mut cursors: Vec<Cursor<I>> = inputs.into_iter().map(|batches| Cursor { batches, batch: None, at: 0 }).collect();
    for c in &mut cursors {
        c.load(&converter, sort)?;
    }
    let mut pending: Vec<(usize, usize)> = Vec::with_capacity(OUT_BATCH);
    // Every batch a pending index names is still in hand: the merge flushes before any
    // cursor replaces its batch.
    let flush = |cursors: &[Cursor<I>], pending: &mut Vec<(usize, usize)>, out: &mut StreamWriter| -> Result<u64> {
        if pending.is_empty() {
            return Ok(0);
        }
        let mut slot = vec![usize::MAX; cursors.len()];
        let mut refs: Vec<&RecordBatch> = Vec::new();
        for (i, c) in cursors.iter().enumerate() {
            if let Some((b, _)) = &c.batch {
                slot[i] = refs.len();
                refs.push(b);
            }
        }
        let indices: Vec<(usize, usize)> = pending.iter().map(|(c, r)| (slot[*c], *r)).collect();
        let batch = interleave_record_batch(&refs, &indices).map_err(invalid)?;
        out.write(&batch)?;
        let n = pending.len() as u64;
        pending.clear();
        Ok(n)
    };
    loop {
        let mut best: Option<usize> = None;
        for (i, c) in cursors.iter().enumerate() {
            let Some((_, rows)) = &c.batch else { continue };
            best = match best {
                Some(b) => {
                    let (_, best_rows) = cursors[b].batch.as_ref().expect("the best cursor holds a batch");
                    if rows.row(c.at) < best_rows.row(cursors[b].at) { Some(i) } else { Some(b) }
                }
                None => Some(i),
            };
        }
        let Some(i) = best else { break };
        pending.push((i, cursors[i].at));
        cursors[i].at += 1;
        let exhausted = cursors[i].batch.as_ref().is_some_and(|(b, _)| cursors[i].at >= b.num_rows());
        if exhausted {
            written += flush(&cursors, &mut pending, out)?;
            cursors[i].load(&converter, sort)?;
        } else if pending.len() >= OUT_BATCH {
            written += flush(&cursors, &mut pending, out)?;
        }
    }
    written += flush(&cursors, &mut pending, out)?;
    Ok(written)
}
