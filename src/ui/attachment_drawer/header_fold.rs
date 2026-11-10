//! The header's size slider folds away in a narrow reader (#624): the
//! header is the reader's widest row, and a reader squeezed below it was
//! drawn past the window's edge.

use adw::prelude::*;
use relm4::prelude::*;

use super::{AttachmentDrawer, AttachmentDrawerInput, SLIDER_ROOM};

/// The breakpoint that hides the slider, on the header's own bin. Its
/// width is set by `fit_header` once the header has been measured.
pub(super) fn header_fold(
    bin: &adw::BreakpointBin,
    sender: &ComponentSender<AttachmentDrawer>,
) -> adw::Breakpoint {
    let fold = adw::Breakpoint::new(max_width(0));
    let s = sender.input_sender().clone();
    fold.connect_apply(move |_| s.emit(AttachmentDrawerInput::SetNarrow(true)));
    let s = sender.input_sender().clone();
    fold.connect_unapply(move |_| s.emit(AttachmentDrawerInput::SetNarrow(false)));
    bin.add_breakpoint(fold.clone());
    fold
}

fn max_width(px: i32) -> adw::BreakpointCondition {
    adw::BreakpointCondition::new_length(
        adw::BreakpointConditionLengthType::MaxWidth,
        f64::from(px),
        adw::LengthUnit::Px,
    )
}

impl AttachmentDrawer {
    /// Whether the thumbnail size slider is in the header.
    pub(super) fn slider_shown(&self) -> bool {
        !self.collapsed && !self.list_view && !self.narrow
    }

    /// Size the header's bin from the header as it now is: at least the
    /// header without the slider, and the slider folded below the width it
    /// needs with it. The count's label changes that width a little.
    pub(super) fn fit_header(&self, bin: &adw::BreakpointBin, header: &gtk::Box) {
        let Some(fold) = &self.header_fold else {
            return;
        };
        let width = header.measure(gtk::Orientation::Horizontal, -1).0;
        let height = header.measure(gtk::Orientation::Vertical, -1).0;
        let slider_room = if self.slider_shown() { SLIDER_ROOM } else { 0 };
        let floor = width - slider_room;
        let with_slider = floor + SLIDER_ROOM;
        if bin.width_request() != floor || bin.height_request() != height {
            bin.set_size_request(floor, height);
        }
        let condition = max_width(with_slider - 1);
        if fold.condition().map(|c| c.to_string()) != Some(condition.to_string()) {
            fold.set_condition(Some(&condition));
        }
    }
}
