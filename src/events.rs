use std::ffi::CString;

use nexus::event::Event;

// Events for other addons to subscribe to
pub const EV_LOG_DETECTED: Event<LogDetectedEvent> =
    unsafe { Event::new("EV_LOG_UPLOADER_LOG_DETECTED") };
pub const EV_LOG_PARSED: Event<LogParsedEvent> =
    unsafe { Event::new("EV_LOG_UPLOADER_LOG_PARSED") };
pub const EV_DPSREPORT: Event<DpsReportEvent> = unsafe { Event::new("EV_LOG_UPLOADER_DPSREPORT") };
pub const EV_WINGMAN: Event<WingmanEvent> = unsafe { Event::new("EV_LOG_UPLOADER_WINGMAN") };

/// Event sent when a log is detected
/// C definition:
/// ```c
/// typedef struct {
///     const char* file_path;
///     uint32_t file_path_len;
/// } LogDetectedEvent;
/// ```
#[repr(C)]
pub struct LogDetectedEvent {
    pub file_path: *const std::ffi::c_char,
    pub file_path_len: u32,
}

/// Event sent when a log is parsed
/// C definition:
/// ```c
/// typedef struct {
///     const char* file_path;
///     uint32_t file_path_len;
///     uint16_t boss_id;
///     uint32_t player_count;
/// } LogParsedEvent;
/// ```
#[repr(C)]
pub struct LogParsedEvent {
    pub file_path: *const std::ffi::c_char,
    pub file_path_len: u32,
    pub boss_id: u16,
    pub player_count: u32,
}

/// Event sent when a dps.report response is received
/// C definition:
/// ```c
/// typedef struct {
///     const char* file_path;
///     uint32_t file_path_len;
///     const char* permalink;
///     uint32_t permalink_len;
///     int64_t boss_id;
///     bool success;
/// } DpsReportEvent;
/// ```
#[repr(C)]
pub struct DpsReportEvent {
    pub file_path: *const std::ffi::c_char,
    pub file_path_len: u32,
    pub permalink: *const std::ffi::c_char,
    pub permalink_len: u32,
    pub boss_id: i64,
    pub success: bool,
}

/// Event sent when a wingman response is received
/// This might change with wingman api changes
/// Always compare version to expected size of struct
/// new versions will only append fields
/// so backwards compatibility is guaranteed
/// C definition:
/// ```c
/// typedef struct {
///     uint32_t version;
///     const char* file_path;
///     uint32_t file_path_len;
///     uint16_t boss_id;
///     bool accepted;
/// } WingmanEvent;
/// ```
#[repr(C)]
pub struct WingmanEvent {
    pub version: u32,
    pub file_path: *const std::ffi::c_char,
    pub file_path_len: u32,
    pub boss_id: u16,
    pub accepted: bool,
}

pub fn path_to_cstring(path: &std::path::Path) -> CString {
    CString::new(path.to_string_lossy().as_bytes()).unwrap_or_default()
}
