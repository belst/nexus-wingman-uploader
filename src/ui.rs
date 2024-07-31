use std::ffi::OsStr;

use nexus::imgui::{StyleColor, TableToken, Ui, Window};

pub trait UiExt {
    fn link<L: AsRef<str>, U: AsRef<OsStr>>(&self, label: L, url: Option<U>) -> anyhow::Result<()>;
    fn popover<F: FnOnce()>(&self, f: F);
    // _t is unused, but required to make sure we only call this inside an active table
    fn table_row_hovered(&self, _t: &TableToken) -> bool;
}

impl UiExt for Ui<'_> {
    fn link<L: AsRef<str>, U: AsRef<OsStr>>(&self, label: L, url: Option<U>) -> anyhow::Result<()> {
        let blue = self.push_style_color(StyleColor::Text, [0.0, 0.0, 1.0, 1.0]);
        self.text(label);
        blue.pop();
        let mut min = self.item_rect_min();
        let max = self.item_rect_max();
        min[1] = max[1];
        self.get_window_draw_list()
            .add_line(min, max, [0.0, 0.0, 1.0, 1.0])
            .build();
        if url.is_some() && self.is_item_hovered() {
            if self.is_item_clicked() {
                open::that_detached(url.unwrap())?;
            }
            self.tooltip_text("Open in Browser");
        }
        Ok(())
    }

    fn popover<F: FnOnce()>(&self, f: F) {
        let [x_min, y_min] = self.item_rect_min();
        let [x_max, _] = self.item_rect_max();
        let mid = x_min + (x_max - x_min) / 2.0;

        Window::new("Popover")
            .position([mid, y_min], nexus::imgui::Condition::Always)
            .position_pivot([0.5, 1.0])
            .no_nav()
            .no_decoration()
            .build(self, f);
    }

    fn table_row_hovered(&self, _t: &TableToken) -> bool {
        // use start to get the y position of the row
        let start = self.cursor_pos();
        let style = self.clone_style();
        let window_padding = style.window_padding;
        let frame_padding = style.frame_padding;
        let mut wmin = self.window_pos();
        let wsize = self.window_size();
        wmin[0] += window_padding[0];
        wmin[1] += start[1] - frame_padding[1];
        let frame_height = self.frame_height();
        let wmax = [
            wmin[0] + wsize[0] - window_padding[0] * 2.0,
            wmin[1] + frame_height,
        ];
        if self.is_current_mouse_pos_valid()
            && self.is_rect_visible(wmin, wmax)
            && self.is_mouse_hovering_rect(wmin, wmax)
        {
            true
        } else {
            false
        }
    }
}
