use crate::util::timestamp::OPEN_SENTINEL;

/// Half-open valid time interval [valid_from, valid_to).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[non_exhaustive]
pub struct Interval {
    pub valid_from: String,
    pub valid_to: String,
}

impl Interval {
    pub fn new(valid_from: impl Into<String>, valid_to: impl Into<String>) -> Self {
        Self {
            valid_from: valid_from.into(),
            valid_to: valid_to.into(),
        }
    }

    /// Check if interval represents an open interval (`OPEN_SENTINEL`).
    pub fn is_open(&self) -> bool {
        self.valid_to == OPEN_SENTINEL
    }

    /// Check if timestamp falls within half-open interval [valid_from, valid_to).
    pub fn contains(&self, ts: &str) -> bool {
        self.valid_from.as_str() <= ts && ts < self.valid_to.as_str()
    }

    /// Check if two half-open intervals overlap: max(start1, start2) < min(end1, end2).
    pub fn overlaps(&self, other: &Interval) -> bool {
        let max_start = std::cmp::max(self.valid_from.as_str(), other.valid_from.as_str());
        let min_end = std::cmp::min(self.valid_to.as_str(), other.valid_to.as_str());
        max_start < min_end
    }
}
