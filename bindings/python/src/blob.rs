//! The blob store's row type (0.19.0, P4, D-281).
//!
//! Lands with the Rust half, for the reason `branch.rs` records: a binding gap
//! opened in the release that created the feature never becomes a convention.
//! The three methods are on `Database`; this is what `blob_stat` returns.
//!
//! # Why the address is a `str` and not a class
//!
//! It is the 64 lowercase hex characters `hashlib.sha256(data).hexdigest()`
//! prints, and that equality is the whole interoperability argument for
//! SHA-256 (D-281 amendment 3). A wrapper type would make a caller convert a
//! digest they already hold into one they can pass, and would validate it at
//! construction rather than at the call — the same instant, one more name.

use pyo3::prelude::*;

use crate::timestamps::from_canonical;

/// What the store knows about one blob, without its bytes.
///
/// `frozen`: a snapshot of a row, and a mutable copy would invite the belief
/// that changing it changes anything.
#[pyclass(name = "BlobStat", module = "macrame", frozen)]
pub(crate) struct PyBlobStat {
    pub(crate) inner: macrame::BlobStat,
}

#[pymethods]
impl PyBlobStat {
    /// The address: 64 lowercase hex characters.
    #[getter]
    fn sha256(&self) -> &str {
        &self.inner.sha256
    }

    /// Length of the bytes.
    #[getter]
    fn size(&self) -> u64 {
        self.inner.size
    }

    /// When the blob was last put — or, for one the archive copied back, when
    /// that session ran. The archive's age guard reads this.
    #[getter]
    fn put_at<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        from_canonical(py, &self.inner.put_at)
    }

    /// `"hot"` or `"cold"`: which file answered.
    ///
    /// A string rather than an enum class, on the reasoning `Branch.parent`
    /// uses for a lineage name: two values a caller compares against and never
    /// constructs.
    #[getter]
    fn location(&self) -> &'static str {
        location_str(self.inner.location)
    }

    fn __repr__(&self) -> String {
        format!(
            "<macrame.BlobStat sha256={} size={} location={}>",
            self.inner.sha256,
            self.inner.size,
            location_str(self.inner.location)
        )
    }
}

fn location_str(location: macrame::BlobLocation) -> &'static str {
    match location {
        macrame::BlobLocation::Hot => "hot",
        macrame::BlobLocation::Cold => "cold",
        // `#[non_exhaustive]` on the Rust side. A third location would be a
        // release that changed this file in the same commit.
        _ => "unknown",
    }
}
