use gpui_kit::TestAppContext;

use crate::test_support::TestWorld;
use crate::test_support::fixtures_list::{ListFixture, listed_corpus};

#[gpui_kit::test]
fn smoke_loads_the_listed_corpus(cx: &mut TestAppContext) {
    let world = TestWorld::new();
    listed_corpus(&world);
    let f = ListFixture::open_loaded(cx, &world, &[]);
    assert_eq!(f.paths(cx).len(), 8);
    let titles = f.titles(cx);
    eprintln!("{titles:#?}");
    assert_eq!(f.read(cx, |l, _| l.search_status().text), "1/8");
}
