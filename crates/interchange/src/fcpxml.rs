//! (in progress)

use filmcraft_project::{ItemId, Project};

use crate::{Error, ExportOptions, ImportOptions, Imported, Report, Result};

pub(crate) fn import(_text: &str, _opts: &ImportOptions, _report: &mut Report) -> Result<Imported> {
    Err(Error::Other("not implemented".into()))
}

pub(crate) fn export(_p: &Project, _seq: ItemId, _opts: &ExportOptions, _report: &mut Report) -> Result<String> {
    Err(Error::Other("not implemented".into()))
}
