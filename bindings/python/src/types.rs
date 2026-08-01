//! Value types (P3).
//!
//! The Rust API builds these with consuming builders —
//! `EdgeAssertion::new(a, b, t).valid_from(x).weight(0.5)`. Chaining that in
//! Python is possible and nobody would write it, so these take keyword
//! arguments and are immutable afterwards.
//!
//! # Validation happens in the constructor, and the plan said otherwise
//!
//! §5 specified that `normalized()` runs "at the point of use, so validation
//! errors surface from the method that would have written the row". That is the
//! right instinct in Rust and the wrong one here, for a reason specific to how
//! these are used: a Python caller builds a **list** and hands it to
//! `write_bulk_atomic`. Validating there reports *"invalid edge type 'bad'"*
//! with no indication which of ten thousand edges it was, and a traceback
//! pointing at the write.
//!
//! Validating in the constructor puts the error on the line that built the bad
//! object, which is the line that has to change. It also makes these types mean
//! something: an `EdgeAssertion` that exists is one the ledger will accept, so
//! the getters can return canonical values rather than whatever was passed.
//!
//! # What `normalized()` does and does not check
//!
//! It validates ids, edge type, and timestamp *shape* — it does not check that
//! a date is a real calendar date, which `timestamp::parse` does and the write
//! path reaches later. So `2026-02-30T…` still constructs. That is the crate's
//! own boundary, left where it is rather than tightened here, because a binding
//! that refuses more than the library it binds is its own kind of surprise.

use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyString};

use macrame::graph::AttributeMode;
use macrame::prelude::{Annotation, ConceptUpsert, EdgeAssertion, Interval};

use crate::errors::to_py;
use crate::timestamps::{from_canonical, to_canonical};

/// Which text a temporal traversal should return (T3.2, D-085).
///
/// Leaving this unset on a traversal that also sets `as_of` raises
/// `AttributeModeUnstatedError` rather than quietly returning live text for a
/// historical topology. `None` is *unstated*, not `CURRENT`, and the difference
/// is the whole mechanism.
#[pyclass(
    name = "AttributeMode",
    module = "macrame",
    eq,
    eq_int,
    frozen,
    from_py_object
)]
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum PyAttributeMode {
    /// Live attributes from `concepts`. Fast, and wrong for historical text.
    #[pyo3(name = "CURRENT")]
    Current,
    /// Attributes as believed at the traversal's instant, from the log.
    #[pyo3(name = "AT_TIME")]
    AtTime,
    /// Topology only; no attributes are hydrated.
    #[pyo3(name = "OMIT")]
    Omit,
}

impl From<PyAttributeMode> for AttributeMode {
    fn from(m: PyAttributeMode) -> Self {
        match m {
            PyAttributeMode::Current => AttributeMode::Current,
            PyAttributeMode::AtTime => AttributeMode::AtTime,
            PyAttributeMode::Omit => AttributeMode::Omit,
        }
    }
}

impl From<AttributeMode> for PyAttributeMode {
    fn from(m: AttributeMode) -> Self {
        match m {
            AttributeMode::Current => PyAttributeMode::Current,
            AttributeMode::AtTime => PyAttributeMode::AtTime,
            AttributeMode::Omit => PyAttributeMode::Omit,
        }
    }
}

/// A concept assertion: the payload of an upsert.
#[pyclass(name = "ConceptUpsert", module = "macrame", frozen, eq, from_py_object)]
#[derive(Clone, PartialEq)]
pub(crate) struct PyConceptUpsert {
    pub(crate) inner: ConceptUpsert,
}

#[pymethods]
impl PyConceptUpsert {
    /// `valid_from` is required because the ledger requires it.
    ///
    /// Rust's builder leaves it empty and `normalized()` rejects that, so there
    /// is no default to inherit. Defaulting it to "now" here would be inventing
    /// a semantic the library does not have — and valid time is a claim about
    /// the world, not about when the row was written, which is `recorded_at`
    /// and is the clock's business.
    #[new]
    #[pyo3(signature = (
        id, title, *, valid_from, content = String::new(),
        embedding_model = None, valid_to = None, retired = false
    ))]
    fn new(
        id: String,
        title: String,
        valid_from: &Bound<'_, PyAny>,
        content: String,
        embedding_model: Option<String>,
        valid_to: Option<&Bound<'_, PyAny>>,
        retired: bool,
    ) -> PyResult<Self> {
        let mut c = ConceptUpsert::new(id, title)
            .content(content)
            .valid_from(to_canonical(Some(valid_from))?)
            .valid_to(to_canonical(valid_to)?)
            .retired(retired);
        if let Some(model) = embedding_model {
            c = c.embedding_model(model);
        }
        Ok(Self {
            inner: c.normalized().map_err(to_py)?,
        })
    }

    #[getter]
    fn id(&self) -> &str {
        &self.inner.id
    }
    #[getter]
    fn title(&self) -> &str {
        &self.inner.title
    }
    #[getter]
    fn content(&self) -> &str {
        &self.inner.content
    }
    #[getter]
    fn embedding_model(&self) -> Option<&str> {
        self.inner.embedding_model.as_deref()
    }
    #[getter]
    fn retired(&self) -> bool {
        self.inner.retired
    }
    #[getter]
    fn valid_from<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        from_canonical(py, &self.inner.valid_from)
    }
    #[getter]
    fn valid_to<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        from_canonical(py, &self.inner.valid_to)
    }

    fn __repr__(&self) -> String {
        format!(
            "ConceptUpsert(id={:?}, title={:?}, valid_from={:?}, valid_to={:?}, retired={})",
            self.inner.id,
            self.inner.title,
            self.inner.valid_from,
            self.inner.valid_to,
            if self.inner.retired { "True" } else { "False" }
        )
    }
}

/// An edge assertion: the payload of assert / retire / re-assert.
#[pyclass(name = "EdgeAssertion", module = "macrame", frozen, eq, from_py_object)]
#[derive(Clone, PartialEq)]
pub(crate) struct PyEdgeAssertion {
    pub(crate) inner: EdgeAssertion,
}

#[pymethods]
impl PyEdgeAssertion {
    /// `properties` is a JSON **string**, matching the column.
    ///
    /// Not a dict. The crate documents the payload as opaque, and accepting a
    /// dict here would mean this binding decides the encoding — key order,
    /// what happens to a `Decimal`, what happens to a `datetime` — for data it
    /// never reads. `json.dumps(...)` at the call site is one line and leaves
    /// those choices with the caller, who is the only one who can make them.
    #[new]
    #[pyo3(signature = (
        source, target, edge_type, *, valid_from, valid_to = None,
        weight = 1.0, properties = "{}".to_string()
    ))]
    fn new(
        source: String,
        target: String,
        edge_type: String,
        valid_from: &Bound<'_, PyAny>,
        valid_to: Option<&Bound<'_, PyAny>>,
        weight: f64,
        properties: String,
    ) -> PyResult<Self> {
        let e = EdgeAssertion::new(source, target, edge_type)
            .valid_from(to_canonical(Some(valid_from))?)
            .valid_to(to_canonical(valid_to)?)
            .weight(weight)
            .properties(properties);
        Ok(Self {
            inner: e.normalized().map_err(to_py)?,
        })
    }

    #[getter]
    fn source(&self) -> &str {
        &self.inner.source
    }
    #[getter]
    fn target(&self) -> &str {
        &self.inner.target
    }
    #[getter]
    fn edge_type(&self) -> &str {
        &self.inner.edge_type
    }
    #[getter]
    fn weight(&self) -> f64 {
        self.inner.weight
    }
    #[getter]
    fn properties(&self) -> &str {
        &self.inner.properties
    }
    #[getter]
    fn valid_from<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        from_canonical(py, &self.inner.valid_from)
    }
    #[getter]
    fn valid_to<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        from_canonical(py, &self.inner.valid_to)
    }

    /// The interval this assertion claims.
    fn interval(&self) -> PyInterval {
        PyInterval {
            inner: Interval::new(self.inner.valid_from.clone(), self.inner.valid_to.clone()),
        }
    }

    fn __repr__(&self) -> String {
        format!(
            "EdgeAssertion({:?} -> {:?}, {:?}, valid_from={:?}, valid_to={:?}, weight={})",
            self.inner.source,
            self.inner.target,
            self.inner.edge_type,
            self.inner.valid_from,
            self.inner.valid_to,
            self.inner.weight
        )
    }
}

/// One derived analytics result for one concept (§5.4, D-041).
///
/// **Not a `ConceptUpsert`,** and the distinction is the whole of D-041: an
/// upsert is a statement about the world and belongs in the ledger; an
/// annotation is a function of an algorithm applied to a graph and belongs in
/// `analytics_annotations`, which carries no log trigger. Writing one as the
/// other overwrote the concept's content with the label and recorded every
/// analytics rerun as a fresh version of the world.
#[pyclass(name = "Annotation", module = "macrame", frozen, eq, from_py_object)]
#[derive(Clone, PartialEq)]
pub(crate) struct PyAnnotation {
    pub(crate) inner: Annotation,
}

#[pymethods]
impl PyAnnotation {
    #[new]
    fn new(concept_id: String, label: String, value: String) -> Self {
        Self {
            inner: Annotation::new(concept_id, label, value),
        }
    }

    #[getter]
    fn concept_id(&self) -> &str {
        &self.inner.concept_id
    }
    /// Namespaced by convention, e.g. `louvain.community`, `kcore.shell`.
    #[getter]
    fn label(&self) -> &str {
        &self.inner.label
    }
    /// JSON-encoded payload, opaque to the ledger.
    #[getter]
    fn value(&self) -> &str {
        &self.inner.value
    }

    fn __repr__(&self) -> String {
        format!(
            "Annotation(concept_id={:?}, label={:?}, value={:?})",
            self.inner.concept_id, self.inner.label, self.inner.value
        )
    }
}

/// A half-open valid-time interval `[valid_from, valid_to)`.
#[pyclass(name = "Interval", module = "macrame", frozen, eq, from_py_object)]
#[derive(Clone, PartialEq)]
pub(crate) struct PyInterval {
    pub(crate) inner: Interval,
}

#[pymethods]
impl PyInterval {
    #[new]
    #[pyo3(signature = (valid_from, valid_to = None))]
    fn new(valid_from: &Bound<'_, PyAny>, valid_to: Option<&Bound<'_, PyAny>>) -> PyResult<Self> {
        Ok(Self {
            inner: Interval::new(to_canonical(Some(valid_from))?, to_canonical(valid_to)?),
        })
    }

    #[getter]
    fn valid_from<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        from_canonical(py, &self.inner.valid_from)
    }
    #[getter]
    fn valid_to<'py>(&self, py: Python<'py>) -> PyResult<Bound<'py, PyAny>> {
        from_canonical(py, &self.inner.valid_to)
    }

    /// Whether the interval runs to the open sentinel, i.e. `valid_to is None`.
    fn is_open(&self) -> bool {
        self.inner.is_open()
    }

    /// Half-open containment: `valid_from <= ts < valid_to`.
    fn contains(&self, ts: &Bound<'_, PyAny>) -> PyResult<bool> {
        Ok(self.inner.contains(&to_canonical(Some(ts))?))
    }

    /// Whether two intervals share any instant.
    fn overlaps(&self, other: &PyInterval) -> bool {
        self.inner.overlaps(&other.inner)
    }

    fn __repr__(&self) -> String {
        format!(
            "Interval(valid_from={:?}, valid_to={:?})",
            self.inner.valid_from, self.inner.valid_to
        )
    }
}

/// Python value → `Vec<f32>`.
///
/// # abi3 took the buffer protocol away, and D-094 did not anticipate that
///
/// The plan specified `PyBuffer<f32>` so a numpy `float32` array would cross as
/// a memory view rather than as 768 individually unboxed Python floats.
/// **`pyo3::buffer` is compiled out under `Py_LIMITED_API`** — the buffer
/// protocol is not in the stable ABI — so with `abi3-py310` that path does not
/// exist. This was found by compiling, not by reading, and it is the second
/// thing abi3 cost (see `timestamps.rs` for the first).
///
/// What is left:
///
/// - **`bytes` / `bytearray`**: decoded as tightly packed little-endian
///   `float32`. This is the fast path, and it is explicit rather than magic —
///   `arr.astype("<f4").tobytes()` is what a numpy caller writes, and it says
///   plainly which dtype and which byte order are being committed to. Refusing
///   to guess is the same rule the timestamps follow.
/// - **any sequence**: `list`, `tuple`, `array.array`, a numpy array of any
///   dtype. Correct everywhere, element by element.
///
/// Re-examine if P4.4 measures the sequence path as a real cost; the exit is to
/// drop abi3, which is a wheel-matrix decision and not a local one.
pub(crate) fn coerce_embedding(obj: &Bound<'_, PyAny>) -> PyResult<Vec<f32>> {
    // A `str` is a sequence of length-1 strings and would otherwise fail much
    // deeper in, with a confusing message.
    if obj.cast::<PyString>().is_ok() {
        return Err(pyo3::exceptions::PyTypeError::new_err(
            "expected a sequence of floats or packed little-endian float32 bytes, got str",
        ));
    }

    // **`bytes` exactly, and nothing that merely converts to it.** An earlier
    // draft accepted anything extracting as `Vec<u8>` so `bytearray` and
    // `memoryview` would take the fast path too — which also swallows a
    // `tuple` of small ints and reinterprets it as packed floats. A silent
    // wrong answer, in the one place where the caller has no way to notice:
    // the result is a valid embedding of a quarter the length, and the
    // dimension check then blames the model. Callers holding a `bytearray` or
    // `memoryview` pass `bytes(x)`.
    if let Ok(raw) = obj.cast::<PyBytes>() {
        return decode_f32_le(raw.as_bytes());
    }

    obj.extract::<Vec<f32>>().map_err(|_| {
        pyo3::exceptions::PyTypeError::new_err(format!(
            "expected a sequence of floats or packed little-endian float32 bytes, got {}",
            obj.get_type()
                .name()
                .map(|n| n.to_string())
                .unwrap_or_else(|_| "?".to_string())
        ))
    })
}

/// Tightly packed little-endian `f32`. A trailing partial value is refused
/// rather than truncated: it means the caller's dtype is not what they think,
/// and silently dropping bytes would produce an embedding of the wrong length
/// that the dimension check would then blame on the model.
fn decode_f32_le(bytes: &[u8]) -> PyResult<Vec<f32>> {
    if !bytes.len().is_multiple_of(4) {
        return Err(pyo3::exceptions::PyValueError::new_err(format!(
            "packed float32 buffer has {} bytes, which is not a multiple of 4 —              check the dtype (expected little-endian float32, e.g.              arr.astype(\"<f4\").tobytes())",
            bytes.len()
        )));
    }
    Ok(bytes
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect())
}

/// `_coerce_embedding(value)` — exposed so P3 can test the coercion before
/// P4.4 lands anything that calls it.
#[pyfunction]
pub(crate) fn _coerce_embedding(obj: &Bound<'_, PyAny>) -> PyResult<Vec<f32>> {
    coerce_embedding(obj)
}

pub(crate) fn register(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<PyAttributeMode>()?;
    m.add_class::<PyConceptUpsert>()?;
    m.add_class::<PyEdgeAssertion>()?;
    m.add_class::<PyAnnotation>()?;
    m.add_class::<PyInterval>()?;
    m.add("OPEN", crate::timestamps::OPEN)?;
    m.add_function(wrap_pyfunction!(_coerce_embedding, m)?)?;
    Ok(())
}
