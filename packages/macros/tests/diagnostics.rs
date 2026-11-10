//! The errors that an application sees when Bimetal cannot export its code.
//!
//! Each file in `tests/diagnostics` is a small application that must fail to
//! compile with the output beside it. Run with `TRYBUILD=overwrite` to
//! rewrite the expected output after a deliberate change.

#[test]
fn unsupported_exports_fail_with_their_diagnostics() {
    trybuild::TestCases::new().compile_fail("tests/diagnostics/*.rs");
}
