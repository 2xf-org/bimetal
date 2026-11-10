//! One generated JavaScript module with its type declarations, and the
//! ownership warning that starts every generated file.

use std::path::Path;

use anyhow::Result;

use crate::output;

/// One generated JavaScript module and its type declarations.
#[derive(Debug, Default)]
pub(super) struct Module {
    /// The JavaScript that runs.
    pub(super) javascript: String,
    /// The declarations that TypeScript reads.
    pub(super) declarations: String,
}

impl Module {
    /// Write `<stem>.js` and `<stem>.d.ts` into `directory`.
    pub(super) fn write(&self, directory: &Path, stem: &str) -> Result<()> {
        output::write(
            &directory.join(format!("{stem}.js")),
            &generated_typescript(&self.javascript),
        )?;
        output::write(
            &directory.join(format!("{stem}.d.ts")),
            &generated_typescript(&self.declarations),
        )
    }
}

/// Prefix generated JavaScript or declarations with the ownership warning.
#[must_use]
pub(super) fn generated_typescript(source: &str) -> String {
    format!("{}\n{source}", output::banner("//", ""))
}
