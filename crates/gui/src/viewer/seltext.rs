//! `SelText`: a `StyledText` element registered with gpui-base text
//! selection that can report the y of a byte index after paint (DESIGN §9.5). WP2.
//!
//! Ported from the viewer spike (`spikes/viewer/src/seltext.rs`): it is
//! gpui-base's `SelectableText` with styled runs (highlights + mono font
//! overrides). One [`TextSelectionHandle`] per rendered segment (from the
//! handle cache, `lru.rs`); window-wide drag selection and
//! `TextSelection::selected_text` then work across segments and items.
//!
//! Extras for the viewer:
//! * `reveal(byte, cell)`: after paint the window position of `byte` is
//!   written to `cell` (exact scroll-to-match, §9.8; test probes);
//! * `links(ranges, on_click)`: a click (no drag) on a link range calls
//!   `on_click(url)` ("Open link?", AD13); hovering a link reports its URL
//!   through `on_hover` and shows the pointing-hand cursor (the URL appears
//!   in the status bar).

use std::cell::Cell;
use std::ops::Range;
use std::rc::Rc;

use gpui_kit::base::{
    TextSelection, TextSelectionHandle, TextSelectionRegistration, TextSelectionRun,
};
use gpui_kit::{
    App, BorderStyle, Bounds, Corners, CursorStyle, DispatchPhase, Edges, Element, ElementId,
    GlobalElementId, HighlightStyle, Hitbox, HitboxBehavior, Hsla, InspectorElementId, IntoElement,
    LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PaintQuad, Pixels, Point,
    SharedString, StyledText, Window, transparent_black,
};

/// Receives the window position of a byte after paint.
pub type PositionCell = Rc<Cell<Option<Point<Pixels>>>>;

/// Called with a link URL (click or hover).
pub type LinkHandler = Rc<dyn Fn(Option<SharedString>, &mut Window, &mut App)>;

/// A selectable, styled text run (see the module documentation).
pub struct SelText {
    id: ElementId,
    handle: TextSelectionHandle,
    text: SharedString,
    styled: StyledText,
    order: u64,
    selection_color: Hsla,
    select_all: bool,
    reveal: Vec<(usize, PositionCell)>,
    links: Rc<Vec<(Range<usize>, SharedString)>>,
    on_link_click: Option<LinkHandler>,
    on_link_hover: Option<LinkHandler>,
}

impl SelText {
    pub fn new(
        id: impl Into<ElementId>,
        handle: TextSelectionHandle,
        text: SharedString,
        highlights: Vec<(Range<usize>, HighlightStyle)>,
        mono_ranges: Vec<Range<usize>>,
        mono_family: SharedString,
    ) -> Self {
        let mut styled = StyledText::new(text.clone()).with_highlights(highlights);
        if !mono_ranges.is_empty() {
            styled = styled.with_font_family_overrides(
                mono_ranges
                    .into_iter()
                    .map(|range| (range, mono_family.clone())),
            );
        }
        Self {
            id: id.into(),
            handle,
            text,
            styled,
            order: 0,
            selection_color: transparent_black(),
            select_all: false,
            reveal: Vec::new(),
            links: Rc::new(Vec::new()),
            on_link_click: None,
            on_link_hover: None,
        }
    }

    /// Reading order among the segments of the window's selection.
    pub fn order(mut self, order: u64) -> Self {
        self.order = order;
        self
    }

    pub fn selection_color(mut self, color: Hsla) -> Self {
        self.selection_color = color;
        self
    }

    /// Paint the whole text as selected (viewer Select All, V-088).
    pub fn select_all(mut self, on: bool) -> Self {
        self.select_all = on;
        self
    }

    /// After paint, write the window position of byte `index` into `cell`.
    pub fn reveal(mut self, index: usize, cell: PositionCell) -> Self {
        self.reveal.push((index, cell));
        self
    }

    /// Link ranges (byte ranges of the text) with their URLs.
    pub fn links(
        mut self,
        links: Vec<(Range<usize>, SharedString)>,
        on_click: LinkHandler,
        on_hover: LinkHandler,
    ) -> Self {
        if !links.is_empty() {
            self.links = Rc::new(links);
            self.on_link_click = Some(on_click);
            self.on_link_hover = Some(on_hover);
        }
        self
    }
}

impl IntoElement for SelText {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

fn link_at(
    links: &[(Range<usize>, SharedString)],
    layout: &gpui_kit::TextLayout,
    position: Point<Pixels>,
) -> Option<SharedString> {
    let index = layout.index_for_position(position).ok()?;
    links
        .iter()
        .find(|(range, _)| range.start <= index && index < range.end)
        .map(|(_, url)| url.clone())
}

impl Element for SelText {
    type RequestLayoutState = ();
    type PrepaintState = Hitbox;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        global_id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        self.styled
            .request_layout(global_id, inspector_id, window, cx)
    }

    fn prepaint(
        &mut self,
        global_id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Hitbox {
        self.styled
            .prepaint(global_id, inspector_id, bounds, &mut (), window, cx);
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        let registration = TextSelectionRegistration::new(hitbox.clone(), bounds)
            .with_document_order(self.order)
            .with_text_bounds(vec![bounds])
            .with_rendered_element(&self.handle, window, cx);
        self.handle.register(registration, window, cx);
        hitbox
    }

    fn paint(
        &mut self,
        global_id: Option<&GlobalElementId>,
        inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        hitbox: &mut Hitbox,
        window: &mut Window,
        cx: &mut App,
    ) {
        let layout = self.styled.layout().clone();
        let before = TextSelection::selected_text(window, cx);
        let projection = self.handle.update_runs(
            &[
                TextSelectionRun::new(self.text.clone(), layout.clone(), bounds)
                    .with_document_order(self.order),
            ],
            cx,
        );
        if before != TextSelection::selected_text(window, cx) {
            window.refresh();
        }
        if self.select_all {
            paint_selection(&layout, 0..self.text.len(), self.selection_color, window);
        }
        for range in projection.ranges().iter().flatten().cloned() {
            paint_selection(&layout, range, self.selection_color, window);
        }
        self.styled.paint(
            global_id,
            inspector_id,
            bounds,
            &mut (),
            &mut (),
            window,
            cx,
        );
        for (index, cell) in self.reveal.drain(..) {
            if let Some(position) = layout.position_for_index(index) {
                cell.set(Some(position));
            }
        }
        if let (Some(on_click), Some(on_hover)) =
            (self.on_link_click.clone(), self.on_link_hover.clone())
        {
            let over_link = hitbox.is_hovered(window)
                && link_at(&self.links, &layout, window.mouse_position()).is_some();
            if over_link {
                window.set_cursor_style(CursorStyle::PointingHand, hitbox);
            }
            let links = self.links.clone();
            let click_layout = layout.clone();
            let click_hitbox = hitbox.clone();
            // Only a left press that started on the link, released on it, is
            // a click (right/middle clicks and scrollbar drags are not).
            let pressed_on_link = Rc::new(Cell::new(false));
            {
                let links = self.links.clone();
                let layout = layout.clone();
                let hitbox = click_hitbox.clone();
                let pressed = pressed_on_link.clone();
                window.on_mouse_event(move |event: &MouseDownEvent, phase, window, _| {
                    if phase == DispatchPhase::Bubble {
                        pressed.set(
                            event.button == MouseButton::Left
                                && hitbox.is_hovered(window)
                                && link_at(&links, &layout, event.position).is_some(),
                        );
                    }
                });
            }
            window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble || !click_hitbox.is_hovered(window) {
                    return;
                }
                if event.button != MouseButton::Left || !pressed_on_link.replace(false) {
                    return;
                }
                // A drag selected text: not a click.
                if TextSelection::has_selection(window, cx) {
                    return;
                }
                if let Some(url) = link_at(&links, &click_layout, event.position) {
                    on_click(Some(url), window, cx);
                }
            });
            let links = self.links.clone();
            let hover_hitbox = hitbox.clone();
            let was_over = Rc::new(Cell::new(over_link));
            window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble {
                    return;
                }
                let url = if hover_hitbox.is_hovered(window) {
                    link_at(&links, &layout, event.position)
                } else {
                    None
                };
                let over = url.is_some();
                if over != was_over.get() {
                    was_over.set(over);
                    on_hover(url, window, cx);
                    window.refresh();
                }
            });
        }
    }
}

fn paint_selection(
    layout: &gpui_kit::TextLayout,
    range: Range<usize>,
    color: Hsla,
    window: &mut Window,
) {
    let (Some(start), Some(end)) = (
        layout.position_for_index(range.start),
        layout.position_for_index(range.end),
    ) else {
        return;
    };
    let bounds = layout.bounds();
    let line_height = layout.line_height();
    let mut quads: Vec<Bounds<Pixels>> = Vec::with_capacity(3);
    if start.y == end.y {
        quads.push(Bounds::from_corners(
            start,
            Point::new(end.x, end.y + line_height),
        ));
    } else {
        quads.push(Bounds::from_corners(
            start,
            Point::new(bounds.right(), start.y + line_height),
        ));
        if end.y > start.y + line_height {
            quads.push(Bounds::from_corners(
                Point::new(bounds.left(), start.y + line_height),
                Point::new(bounds.right(), end.y),
            ));
        }
        quads.push(Bounds::from_corners(
            Point::new(bounds.left(), end.y),
            Point::new(end.x, end.y + line_height),
        ));
    }
    for quad in quads {
        window.paint_quad(PaintQuad {
            bounds: quad,
            background: color.into(),
            corner_radii: Corners::default(),
            border_widths: Edges::default(),
            border_color: transparent_black(),
            border_style: BorderStyle::default(),
        });
    }
}
