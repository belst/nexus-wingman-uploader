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
    #[allow(unused)]
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
            if self.is_item_clicked()
                && let Err(e) = open::that_detached(url.as_ref())
            {
                log::error!("Failed to open {}: {e}", url.as_ref());
            }
            self.tooltip_text(e("Open ") + url.as_ref());
        }
    }
}

/// Builder for a progress bar widget.
///
/// # Examples
///
/// ```no_run
/// # use arcdps_imgui::*;
/// # let mut imgui = Context::create();
/// # let ui = imgui.frame();
/// ProgressBarIndeterminate::new()
///     .size([100.0, 12.0])
///     .overlay_text("Progress!")
///     .build(&ui);
/// ```
#[derive(Copy, Clone, Debug)]
#[must_use]
pub struct ProgressBarIndeterminate<T = &'static str> {
    size: [f32; 2],
    overlay_text: Option<T>,
}

impl ProgressBarIndeterminate {
    /// Creates a progress bar with indeterminate progress.
    ///
    /// The progress bar will be automatically sized to fill the entire width of the window if no
    /// custom size is specified.
    #[inline]
    #[doc(alias = "ProgressBar")]
    pub fn new() -> Self {
        ProgressBarIndeterminate {
            size: [-1.0, 0.0],
            overlay_text: None,
        }
    }
}

impl<T: AsRef<str>> ProgressBarIndeterminate<T> {
    /// Sets an optional text that will be drawn over the progress bar.
    pub fn overlay_text<T2: AsRef<str>>(self, overlay_text: T2) -> ProgressBarIndeterminate<T2> {
        ProgressBarIndeterminate {
            size: self.size,
            overlay_text: Some(overlay_text),
        }
    }

    /// Sets the size of the progress bar.
    ///
    /// Negative values will automatically align to the end of the axis, zero will let the progress
    /// bar choose a size, and positive values will use the given size.
    #[inline]
    pub fn size(mut self, size: [f32; 2]) -> Self {
        self.size = size;
        self
    }

    /// Builds the progress bar
    // Backported from C++ Dear ImGui::ProgressBar with negative fraction (master branch)
    pub fn build(self, ui: &Ui<'_>) {
        let style = ui.clone_style();
        let avail = ui.content_region_avail();
        let size = [
            calc_item_size(self.size[0], ui.calc_item_width(), avail[0]),
            calc_item_size(
                self.size[1],
                ui.current_font_size() + style.frame_padding[1] * 2.0,
                avail[1],
            ),
        ];
        let min = ui.cursor_screen_pos();
        let max = [min[0] + size[0], min[1] + size[1]];
        ui.dummy(size);
        if !ui.is_item_visible() {
            return;
        }

        // The animation is what makes the bar indeterminate: the filled part
        // walks its own width in from the left edge and off the right one.
        const FILL_WIDTH_N: f32 = 0.2;
        let phase = (ui.time() as f32).rem_euclid(1.0);
        let fill_n0 = phase * (1.0 + FILL_WIDTH_N) - FILL_WIDTH_N;
        let fill_n1 = (fill_n0 + FILL_WIDTH_N).clamp(0.0, 1.0);
        let fill_n0 = fill_n0.clamp(0.0, 1.0);

        let draw_list = ui.get_window_draw_list();
        draw_list
            .add_rect(min, max, style[StyleColor::FrameBg])
            .filled(true)
            .rounding(style.frame_rounding)
            .build();

        let border = style.frame_border_size;
        let min = [min[0] + border, min[1] + border];
        let max = [max[0] - border, max[1] - border];
        let fill_x0 = lerp(min[0], max[0], fill_n0);
        let fill_x1 = lerp(min[0], max[0], fill_n1);
        if fill_x0 < fill_x1 {
            // Clipping the full rounded rect keeps the rounding on the ends the
            // fill actually reaches, which is what RenderRectFilledInRangeH does.
            draw_list.with_clip_rect_intersect([fill_x0, min[1]], [fill_x1, max[1]], || {
                draw_list
                    .add_rect(min, max, style[StyleColor::PlotHistogram])
                    .filled(true)
                    .rounding(style.frame_rounding)
                    .build();
            });
        }

        // No percentage to fall back on, so text is drawn only when asked for.
        if let Some(overlay) = &self.overlay_text {
            let overlay = overlay.as_ref();
            let text_size = ui.calc_text_size(overlay);
            if text_size[0] > 0.0 {
                let x = ((min[0] + max[0] - text_size[0]) * 0.5)
                    .clamp(min[0], max[0] - text_size[0] - style.item_inner_spacing[0]);
                let y = min[1] + (size[1] - text_size[1]) * 0.5;
                draw_list.with_clip_rect_intersect(min, max, || {
                    draw_list.add_text([x, y], style[StyleColor::Text], overlay);
                });
            }
        }
    }
}

/// size < 0: align to the end of the axis, 0: default, > 0: as given.
fn calc_item_size(size: f32, default: f32, avail: f32) -> f32 {
    if size > 0.0 {
        size
    } else if size < 0.0 {
        (avail + size).max(4.0)
    } else {
        default
    }
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}
