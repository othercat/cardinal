mod event;
mod event_flag;
mod event_stream;
mod utils;

pub use event::FsEvent;
pub use event_flag::{EventFlag, EventType, ScanType};
pub use event_stream::{EventStream, EventWatcher};
pub use objc2_core_services::FSEventStreamEventId;
pub use utils::{
    current_event_id, current_timestamp, event_id_to_timestamp, last_event_id_before_time,
};
