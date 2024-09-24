use nexus::{imgui::Ui, localization::translate};

pub fn e(s: &str) -> String {
    translate(s).unwrap_or_else(|| s.to_string())
}

pub trait UiExt {
    fn help_marker<F: FnOnce()>(&self, f: F);
    fn attention_marker<F: FnOnce()>(&self, f: F);
}
impl UiExt for Ui<'_> {
    fn help_marker<F: FnOnce()>(&self, f: F) {
        self.same_line();
        self.text_disabled("(?)");
        if self.is_item_hovered() {
            f();
        }
    }
    fn attention_marker<F: FnOnce()>(&self, f: F) {
        self.same_line();
        self.text_disabled("(!)");
        if self.is_item_hovered() {
            f();
        }
    }
}
