//! Source implementations — the connectors that read.
//!
//! Counterpart to `sinks`. The direction split is the trait, so a connector that both reads and
//! writes implements both and is registered in both maps; `google` is exactly that, pulling a
//! calendar and creating mail drafts through one deployment.

mod google;
mod microsoft;

pub use microsoft::{
    authorize_url, base64url, parse_graph_time, GraphAttendee, GraphEvent, GraphTime,
    MicrosoftGraph, Pkce, CLIENT_ID_KEY, REFRESH_TOKEN_KEY, SCOPES,
};

pub use google::{
    join_url_of, to_inbound, Calendar, DraftRef, GoogleBridge, ScriptEvent, ScriptGuest,
    DEPLOYMENT_URL_KEY, REQUIRED_VERSION, SHARED_KEY, WINDOW_BACK_DAYS, WINDOW_FORWARD_DAYS,
};
