//! Without the `arrow` feature of `bimetal`, the compiler says to enable it.

use bimetal::arrow::RecordBatch;

#[bimetal::export]
#[must_use]
pub fn rows(#[bimetal(arrow)] batch: &RecordBatch) -> u64 {
    batch.num_rows() as u64
}

fn main() {}
