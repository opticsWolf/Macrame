//! Rows out of a diagnostic connection (pulled forward from P4.6).
//!
//! # Why this is here at P4.1
//!
//! P4.1's acceptance is "writes, and reads back". Without a read path there is
//! no way to distinguish a write that landed from a method that returned a
//! plausible count and did nothing — and every later phase's tests need the
//! same thing. So the diagnostic read arrives with the first writes rather than
//! at P4.6, where the plan put it.
//!
//! # It is a query, not a connection
//!
//! §7 is explicit that `diagnostic_conn()` is exposed as *methods that run a
//! query and return rows*, never as a connection object. The capability T5.1
//! wanted — a caller's own read-only handle, an OS-level boundary rather than a
//! reversible `PRAGMA` — is preserved. The object that would let a caller keep
//! it and do something else with it is not.
//!
//! Each call opens its own `SQLITE_OPEN_READ_ONLY` connection and drops it.
//! That is the semantic (`diagnostic_conn` hands out a caller's own connection)
//! and it is also the R15-safe shape: the fault counts *concurrent* opens, and
//! 500 sequential opens in one process measured 0 faults in 10.

use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyTuple};

use crate::errors::to_py;

/// One libSQL value as a Python object.
///
/// `Null` becomes `None`, integers and reals their obvious counterparts, `Text`
/// a `str`, `Blob` `bytes`. No interpretation beyond that — a timestamp column
/// comes back as the canonical string it is stored as, because this is the
/// *diagnostic* path and its job is to show what is actually there.
fn value_to_py<'py>(py: Python<'py>, v: libsql::Value) -> PyResult<Bound<'py, PyAny>> {
    Ok(match v {
        libsql::Value::Null => py.None().into_bound(py),
        libsql::Value::Integer(i) => i.into_pyobject(py)?.into_any(),
        libsql::Value::Real(f) => f.into_pyobject(py)?.into_any(),
        libsql::Value::Text(s) => s.into_pyobject(py)?.into_any(),
        libsql::Value::Blob(b) => PyBytes::new(py, &b).into_any(),
    })
}

/// Rows collected off the connection while the GIL is released.
///
/// The two halves have to be separate: reading rows is async and must not hold
/// the GIL, while building Python objects requires it. So the query runs to
/// completion into `Vec<Vec<Value>>` first, and only then does
/// [`rows_to_py`] convert. Interleaving them would mean re-acquiring the GIL
/// per cell.
pub(crate) type RawRows = Vec<Vec<libsql::Value>>;

pub(crate) async fn collect(
    conn: &libsql::Connection,
    sql: &str,
    params: Vec<libsql::Value>,
) -> Result<RawRows, macrame::DbError> {
    let mut rows = conn.query(sql, libsql::params_from_iter(params)).await?;
    let mut out: RawRows = Vec::new();
    while let Some(row) = rows.next().await? {
        let mut cells = Vec::new();
        // `column_count` is on the rows handle, not the row; asking per row
        // keeps this correct for a statement whose shape we never inspected.
        let n = rows.column_count();
        for i in 0..n {
            cells.push(row.get_value(i)?);
        }
        out.push(cells);
    }
    Ok(out)
}

pub(crate) fn rows_to_py<'py>(
    py: Python<'py>,
    rows: RawRows,
) -> PyResult<Vec<Bound<'py, PyTuple>>> {
    rows.into_iter()
        .map(|cells| {
            let values = cells
                .into_iter()
                .map(|v| value_to_py(py, v))
                .collect::<PyResult<Vec<_>>>()?;
            PyTuple::new(py, values)
        })
        .collect()
}

/// Python value → `libsql::Value`, for bound parameters.
///
/// Deliberately narrow. Anything not on this list is refused rather than
/// stringified: a diagnostic query that silently coerced a `datetime` to
/// `str(dt)` would compare a Python repr against a canonical timestamp and
/// return no rows, which reads as "the data is not there".
pub(crate) fn py_to_value(obj: &Bound<'_, PyAny>) -> PyResult<libsql::Value> {
    if obj.is_none() {
        return Ok(libsql::Value::Null);
    }
    if let Ok(b) = obj.extract::<bool>() {
        // Before the integer branch: `bool` is a subclass of `int` in Python and
        // would otherwise arrive as 0/1 with no complaint, which is right here
        // but worth doing on purpose rather than by accident.
        return Ok(libsql::Value::Integer(b as i64));
    }
    if let Ok(i) = obj.extract::<i64>() {
        return Ok(libsql::Value::Integer(i));
    }
    if let Ok(f) = obj.extract::<f64>() {
        return Ok(libsql::Value::Real(f));
    }
    if let Ok(s) = obj.extract::<String>() {
        return Ok(libsql::Value::Text(s));
    }
    if let Ok(b) = obj.extract::<Vec<u8>>() {
        return Ok(libsql::Value::Blob(b));
    }
    Err(pyo3::exceptions::PyTypeError::new_err(format!(
        "cannot bind {} as a SQL parameter; pass None, int, float, str or bytes. \
         A datetime must be passed as its canonical string — see macrame.OPEN \
         for the stored form",
        obj.get_type()
            .name()
            .map(|n| n.to_string())
            .unwrap_or_else(|_| "?".to_string())
    )))
}

/// Convenience for the crate's own error type at call sites that already have a
/// `DbError`.
pub(crate) fn map_err<T>(r: Result<T, macrame::DbError>) -> PyResult<T> {
    r.map_err(to_py)
}
