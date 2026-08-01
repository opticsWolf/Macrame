//! Timestamp coercion (P3, D-096 as amended).
//!
//! Every timestamp in the ledger is a canonical RFC3339 microsecond UTC string,
//! `YYYY-MM-DDTHH:MM:SS.ffffffZ`, and `normalize` refuses anything else rather
//! than guessing — an offset, a missing `Z`, millisecond precision. That is
//! right, and it is also the first thing a Python caller will trip over, because
//! they will pass a `datetime`.
//!
//! So: **`str` or aware `datetime` in, `datetime` out.**
//!
//! # Naive datetimes are refused, not assumed to be UTC
//!
//! The same rule §4.1 applies to timestamp strings. A naive datetime carries no
//! information about which instant it names; treating it as UTC is a silent
//! repair, and a silent repair here is a wrong answer in a temporal query later
//! — shifted by the caller's offset, in a direction nothing records.
//!
//! # The open sentinel crosses as `None`, and this is not what the plan said
//!
//! `9999-12-31T23:59:59.999999Z` is exactly `datetime.max`, so the plan proposed
//! exposing it as a module-level `datetime` constant. **Probe P3-a refuted
//! that.** Measured on CPython 3.13:
//!
//! ```text
//! aware = datetime(9999,12,31,23,59,59,999999, tzinfo=utc)
//!   aware.astimezone(timezone(timedelta(hours=1)))  -> OverflowError
//!   aware.astimezone()          # local zone        -> OSError
//!   aware + timedelta(microseconds=1)               -> OverflowError
//! ```
//!
//! `astimezone()` — rendering an instant in local time — is among the most
//! ordinary things anyone does with an aware datetime, and it raises for every
//! zone east of UTC. In a bitemporal ledger the open interval is *current
//! belief*, so those are not edge-case rows; they are the common ones. A
//! landmine in the common path is worse than a slightly less convenient type.
//!
//! `None` says what is true: an open interval has no end. It is symmetric —
//! `valid_to=None` on the way in means open too — and it cannot be ambiguous
//! with "missing", because every interval has a `valid_to`.
//!
//! The cost is real and stated rather than hidden: `sorted(rows, key=...)` over
//! a `valid_to` needs `key=lambda r: (r.valid_to is None, r.valid_to)`, because
//! `None` does not compare with `datetime`. [`OPEN`] is exported for callers who
//! need to name the sentinel in its stored form.
//!
//! # abi3 costs a C-level accessor, and this was not anticipated
//!
//! D-094 chose `abi3-py310` and asserted the price was "the limited C API, which
//! these bindings do not touch". **They do.** `PyDateAccess` and `PyTimeAccess`
//! — pyo3's `get_year()` / `get_hour()` and friends — are compiled out under
//! `Py_LIMITED_API`, because the CPython datetime struct is not part of it.
//!
//! So the fields are read with `getattr`, seven Python attribute lookups per
//! timestamp instead of seven struct reads. That is the whole cost: it is on
//! the coercion path only, a write is dominated by SQLite, and `isoformat()` —
//! the tempting one-call alternative — omits the microseconds when they are
//! zero, which would silently produce a non-canonical string for every
//! timestamp landing exactly on a second.

use pyo3::prelude::*;
use pyo3::types::{PyDate, PyDateTime, PyString, PyTzInfo, PyTzInfoAccess};

use macrame::util::timestamp::{normalize, OPEN_SENTINEL};
use macrame::DbError;

use crate::errors::to_py;

/// The stored form of an open interval's end, for callers who need to name it.
///
/// Exposed as `macrame.OPEN`. Reading it back from a ledger gives `None`; this
/// is what is actually in the column.
pub(crate) const OPEN: &str = OPEN_SENTINEL;

fn invalid(value: String, reason: &str) -> PyErr {
    to_py(DbError::InvalidTimestamp {
        value,
        reason: reason.to_string(),
    })
}

/// Python value → canonical string. `None` means the open sentinel.
pub(crate) fn to_canonical(obj: Option<&Bound<'_, PyAny>>) -> PyResult<String> {
    let Some(obj) = obj else {
        return Ok(OPEN_SENTINEL.to_string());
    };
    if obj.is_none() {
        return Ok(OPEN_SENTINEL.to_string());
    }

    // `str` first: it is not a datetime, and it is the form the ledger already
    // speaks, so it goes straight to the crate's own validator.
    if let Ok(s) = obj.cast::<PyString>() {
        let text = s.to_cow()?;
        return normalize(&text).map_err(to_py);
    }

    if let Ok(dt) = obj.cast::<PyDateTime>() {
        return datetime_to_canonical(dt);
    }

    // A bare `date` is refused rather than widened to midnight. Which midnight,
    // in which zone, is precisely the question a `date` does not answer, and
    // picking one is the silent repair this module exists to avoid.
    if obj.cast::<PyDate>().is_ok() {
        return Err(invalid(
            obj.str()?.to_string_lossy().into_owned(),
            "a date names a day, not an instant. Pass an aware datetime, or the \
             canonical string YYYY-MM-DDTHH:MM:SS.ffffffZ",
        ));
    }

    Err(pyo3::exceptions::PyTypeError::new_err(format!(
        "expected a canonical timestamp string, an aware datetime, or None for \
         an open interval; got {}",
        obj.get_type().name()?
    )))
}

fn datetime_to_canonical(dt: &Bound<'_, PyDateTime>) -> PyResult<String> {
    // `tzinfo` present is not enough — `utcoffset()` may still be None, which is
    // what "naive" actually means.
    let naive = match dt.get_tzinfo() {
        None => true,
        Some(_) => dt.call_method0("utcoffset")?.is_none(),
    };
    if naive {
        return Err(invalid(
            dt.str()?.to_string_lossy().into_owned(),
            "naive datetime: it carries no offset, so which instant it names is \
             unknown. Attach a timezone (datetime.timezone.utc, or your own) \
             rather than letting one be assumed",
        ));
    }

    // Convert through Python rather than by arithmetic on the offset: `fold`,
    // and any tzinfo with its own rules, are that object's business and not
    // something to reimplement here.
    let py = dt.py();
    let shifted = dt.call_method1("astimezone", (PyTzInfo::utc(py)?,))?;

    // `getattr` rather than pyo3's struct accessors: those are compiled out
    // under abi3 (see the module docs). Seven lookups, on the coercion path
    // only.
    let f = |name: &str| -> PyResult<u32> { shifted.getattr(name)?.extract::<u32>() };
    Ok(format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:06}Z",
        f("year")?,
        f("month")?,
        f("day")?,
        f("hour")?,
        f("minute")?,
        f("second")?,
        f("microsecond")?,
    ))
}

/// Canonical string → aware UTC `datetime`, or `None` for the open sentinel.
pub(crate) fn from_canonical<'py>(py: Python<'py>, s: &str) -> PyResult<Bound<'py, PyAny>> {
    if s == OPEN_SENTINEL {
        return Ok(py.None().into_bound(py));
    }

    // Fixed-width by construction: anything reaching here came out of the
    // ledger, where `normalize` is the only way in. A stored value that is not
    // canonical is a fault to report, not something to parse leniently.
    if s.len() != 27 {
        return Err(invalid(
            s.to_string(),
            "stored timestamp is not canonical (wrong length)",
        ));
    }
    let num = |lo: usize, hi: usize| -> PyResult<i32> {
        s[lo..hi]
            .parse::<i32>()
            .map_err(|_| invalid(s.to_string(), "stored timestamp is not canonical"))
    };

    let utc = PyTzInfo::utc(py)?;
    let dt = PyDateTime::new(
        py,
        num(0, 4)?,
        num(5, 7)? as u8,
        num(8, 10)? as u8,
        num(11, 13)? as u8,
        num(14, 16)? as u8,
        num(17, 19)? as u8,
        num(20, 26)? as u32,
        Some(&utc),
    )?;
    Ok(dt.into_any())
}

/// `_coerce_timestamp(value)` — the inbound half, exposed for the test suite.
///
/// P3 lands the coercion before P4.x lands anything that calls it, so without
/// this the layer would ship untested. Returns the canonical string so a test
/// can assert on exactly what the ledger would store.
#[pyfunction]
#[pyo3(signature = (value = None))]
pub(crate) fn _coerce_timestamp(value: Option<&Bound<'_, PyAny>>) -> PyResult<String> {
    to_canonical(value)
}

/// `_render_timestamp(canonical)` — the outbound half, likewise.
#[pyfunction]
pub(crate) fn _render_timestamp<'py>(
    py: Python<'py>,
    canonical: &str,
) -> PyResult<Bound<'py, PyAny>> {
    from_canonical(py, canonical)
}
