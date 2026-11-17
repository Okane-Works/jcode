use super::{App, ServerEvent};
use crate::tui::backend::RemoteEventState;

pub(in crate::tui::app) fn handle_server_event(
    app: &mut App,
    event: ServerEvent,
    remote: &mut impl RemoteEventState,
) -> bool {
    let kind = crate::tui::cloud_latency::event_key(&event);
    let started = std::time::Instant::now();
    // OpenRig seat activity: stream evidence posts working; a processing->idle
    // transition across this event posts the turn's Stop boundary.
    let was_processing = app.is_processing;
    let cancel_hint = crate::openrig_activity::observe_server_event(&event, was_processing);
    let redraw = super::server_events::handle_server_event(app, event, remote);
    if was_processing && !app.is_processing {
        crate::openrig_activity::turn_ended(cancel_hint);
    }
    if let Some((kind, _)) = kind {
        crate::tui::cloud_latency::applied(kind, started, redraw);
    }
    redraw
}
