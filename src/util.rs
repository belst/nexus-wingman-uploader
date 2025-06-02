use nexus::{
    imgui::{StyleColor, Ui},
    localization::translate,
};
use std::path::Path;

use windows::Win32::UI::Shell::{ILCreateFromPathW, ILFree, SHOpenFolderAndSelectItems};
use windows::core::{HSTRING, Result};

pub fn e(s: &str) -> String {
    translate(s).unwrap_or_else(|| s.to_string())
}

pub fn open_with_selected(path: impl AsRef<Path>) -> Result<()> {
    let path: &Path = path.as_ref();

    unsafe {
        let pidl = ILCreateFromPathW(&HSTRING::from(path.as_os_str()));
        if pidl.is_null() {
            return Err(windows::core::Error::from_win32());
        }
        SHOpenFolderAndSelectItems(pidl, None, 0)?;
        ILFree(Some(pidl));
    }

    Ok(())
}

pub trait UiExt {
    fn help_marker<F: FnOnce()>(&self, f: F) -> bool;
    fn attention_marker<F: FnOnce()>(&self, f: F) -> bool;
    fn link(&self, label: impl AsRef<str>, url: impl AsRef<str>);
}
impl UiExt for Ui<'_> {
    fn help_marker<F: FnOnce()>(&self, f: F) -> bool {
        let mut clicked = false;
        self.same_line();
        self.text_disabled("(?)");
        if self.is_item_hovered() && self.is_item_clicked() {
            clicked = true;
        }
        if self.is_item_hovered() {
            f();
        }
        clicked
    }
    fn attention_marker<F: FnOnce()>(&self, f: F) -> bool {
        let mut clicked = false;
        self.same_line();
        self.text_disabled("(!)");
        if self.is_item_hovered() && self.is_item_clicked() {
            clicked = true;
        }
        if self.is_item_hovered() {
            f();
        }
        clicked
    }
    fn link(&self, label: impl AsRef<str>, url: impl AsRef<str>) {
        let blue = self.push_style_color(StyleColor::Text, [0.0, 0.0, 1.0, 1.0]);
        self.text(label);
        blue.pop();
        let mut min = self.item_rect_min();
        let max = self.item_rect_max();
        min[1] = max[1];
        self.get_window_draw_list()
            .add_line(min, max, [0.0, 0.0, 1.0, 1.0])
            .build();
        if self.is_item_hovered() {
            if self.is_item_clicked() {
                if let Err(e) = open::that_detached(url.as_ref()) {
                    log::error!("Failed to open {}: {e}", url.as_ref());
                }
            }
            self.tooltip_text(e("Open ") + url.as_ref());
        }
    }
}
