use std::ffi::OsStr;

use arcdps_imgui::{StyleColor, Ui};

pub trait UiExt {
    fn link<L: AsRef<str>, U: AsRef<OsStr>>(&self, label: L, url: Option<U>) -> anyhow::Result<()>;
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
}
