//! Tools tests (G, DESIGN §17.9). WP5. One module per tool; every test names
//! the FEATURES row it covers.

mod about_updates;
mod debug_log;
mod delete_by_id;
mod delete_empty;
mod misc;
mod reports;
mod review_fixes;
mod semantic_cache;
mod startup_tools;

use gpui_kit::{AnyWindowHandle, TestAppContext};

use crate::test_support::TestWorld;
use crate::test_support::fixtures_list::ListFixture;

/// Open the main window on `world` and wait for its load.
pub(super) fn main_window(
    cx: &mut TestAppContext,
    world: &TestWorld,
    args: &[&str],
) -> (ListFixture, AnyWindowHandle) {
    let fixture = ListFixture::open_loaded(cx, world, args);
    let handle = fixture.handle;
    (fixture, handle)
}
