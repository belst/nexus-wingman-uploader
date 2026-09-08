use std::sync::Mutex;
use std::time::{Duration, Instant};

use nexus::imgui::{Condition, Ui, Window};

use crate::settings::Settings;
use crate::util::e;
use crate::wvwsession;

const SNOOZE: Duration = Duration::from_mins(60 * 3);

const MISSED_LIMIT: usize = 50;

const REQUEUE_WINDOW: Duration = Duration::from_secs(120);

#[derive(Default)]
struct State {
    due: Option<Instant>,
    snoozed_until: Option<Instant>,
    missed: Vec<usize>,
    requeue_until: Option<Instant>,
}

static STATE: Mutex<State> = Mutex::new(State {
    due: None,
    snoozed_until: None,
    missed: Vec::new(),
    requeue_until: None,
});

// A WvW log was recorded while no session is running.
pub fn note_log(index: usize) {
    let mut state = STATE.lock().unwrap();
    if !state.missed.contains(&index) {
        if state.missed.len() == MISSED_LIMIT {
            state.missed.remove(0);
        }
        state.missed.push(index);
    }

    let now = Instant::now();
    if state.snoozed_until.is_some_and(|until| now < until) {
        return;
    }
    state.snoozed_until = None;
    state.due.get_or_insert(now);
}

pub fn take_requeue() -> Vec<usize> {
    let mut state = STATE.lock().unwrap();
    let Some(until) = state.requeue_until else {
        return Vec::new();
    };
    if Instant::now() > until {
        // The start never came back. Nothing to attach the logs to.
        state.requeue_until = None;
        state.missed.clear();
        return Vec::new();
    }
    if !wvwsession::is_active() {
        return Vec::new();
    }
    state.requeue_until = None;
    std::mem::take(&mut state.missed)
}

fn clear() {
    let mut state = STATE.lock().unwrap();
    state.due = None;
    state.missed.clear();
    state.requeue_until = None;
}

fn snooze() {
    let mut state = STATE.lock().unwrap();
    state.due = None;
    state.snoozed_until = Some(Instant::now() + SNOOZE);
}

pub fn render(ui: &Ui, settings: &mut Settings) {
    if !settings.enable_wvw_sessions || !settings.wvw_reminder || settings.wvw_token.is_empty() {
        clear();
        return;
    }
    if wvwsession::is_active() {
        clear();
        return;
    }
    let due = STATE.lock().unwrap().due;
    if due.is_none_or(|due| Instant::now() < due) {
        return;
    }

    Window::new(e("Start a WvW session?"))
        .position(super::DEFAULT_INDICATOR_POS, Condition::FirstUseEver)
        .resizable(false)
        .collapsible(false)
        .always_auto_resize(true)
        .focus_on_appearing(false)
        .build(ui, || {
            ui.text(e(
                "Found WvW logs without an active session. Do you want to start one?",
            ));
            if ui.button(e("Start session")) {
                wvwsession::send(wvwsession::SessionCommand::Start { name: None });
                let mut state = STATE.lock().unwrap();
                state.due = None;
                state.requeue_until = Some(Instant::now() + REQUEUE_WINDOW);
            }
            if ui.is_item_hovered() {
                ui.tooltip_text(e("The WvW logs recorded so far join the session."));
            }
            ui.same_line();
            if ui.button(e("Not now")) {
                snooze();
            }
            if ui.is_item_hovered() {
                ui.tooltip_text(e("Asks again in a few hours."));
            }
            ui.same_line();
            if ui.button(e("Never")) {
                settings.wvw_reminder = false;
                if let Err(e) = settings.store(crate::settings::config_path()) {
                    log::error!("Failed to store settings: {e}");
                }
                clear();
            }
            if ui.is_item_hovered() {
                ui.tooltip_text(e("Reenable in the addon options."));
            }
        });
}
