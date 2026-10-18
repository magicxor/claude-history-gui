//! QA element ids (DESIGN §15.3). WP0 → WP7.
//!
//! `element.qa_id("row:3f2a19c0")` registers the element's bounds each frame
//! for the QA runner's `click` steps. With feature `qa` it appends an
//! absolutely positioned canvas child that records the parent's window
//! bounds in a qa-only registry ([`registry`]); without the feature it is the
//! identity, so release builds carry no trace of it.

use gpui_kit::{ParentElement, SharedString, Styled};

/// Name an element for QA scenarios.
pub trait QaExt: Sized {
    /// Register this element under `id` (no-op without feature `qa`).
    fn qa_id(self, id: impl Into<SharedString>) -> Self;
}

#[cfg(not(feature = "qa"))]
impl<T: ParentElement + Styled> QaExt for T {
    fn qa_id(self, id: impl Into<SharedString>) -> Self {
        let _id: SharedString = id.into();
        self
    }
}

#[cfg(feature = "qa")]
impl<T: ParentElement + Styled> QaExt for T {
    fn qa_id(self, id: impl Into<SharedString>) -> Self {
        use gpui_kit::canvas;
        let id: SharedString = id.into();
        self.child(
            canvas(
                move |bounds, _, _| registry::record(&id, bounds),
                |_, (), _, _| {},
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full(),
        )
    }
}

/// The qa-only registry of element bounds (feature `qa`).
#[cfg(feature = "qa")]
pub mod registry {
    use std::collections::HashMap;
    use std::sync::Mutex;
    use std::time::{Duration, Instant};

    use gpui_kit::{Bounds, Pixels, SharedString};

    /// An element's last recorded bounds and when they were recorded.
    type Recorded = (Bounds<Pixels>, Instant);

    static BOUNDS: Mutex<Option<HashMap<SharedString, Recorded>>> = Mutex::new(None);

    /// Remember the bounds an element had in the frame being painted.
    pub fn record(id: &SharedString, bounds: Bounds<Pixels>) {
        let mut map = BOUNDS.lock().unwrap_or_else(|e| e.into_inner());
        map.get_or_insert_with(HashMap::new)
            .insert(id.clone(), (bounds, Instant::now()));
    }

    /// The bounds recorded for `id` no longer than `max_age` ago.
    pub fn lookup(id: &str, max_age: Duration) -> Option<Bounds<Pixels>> {
        let map = BOUNDS.lock().unwrap_or_else(|e| e.into_inner());
        map.as_ref()?
            .get(id)
            .filter(|(_, at)| at.elapsed() <= max_age)
            .map(|(bounds, _)| *bounds)
    }

    /// Every id recorded no longer than `max_age` ago, sorted (diagnostics).
    pub fn ids(max_age: Duration) -> Vec<String> {
        let map = BOUNDS.lock().unwrap_or_else(|e| e.into_inner());
        let mut ids: Vec<String> = map
            .iter()
            .flat_map(|map| map.iter())
            .filter(|(_, (_, at))| at.elapsed() <= max_age)
            .map(|(id, _)| id.to_string())
            .collect();
        ids.sort();
        ids
    }
}
