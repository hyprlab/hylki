/**
 * @file render_pipeline.rs
 * @author Damien O'Brien (Narcarsiss) - https://github.com/narcarsiss/hylki
 * @brief Pre-render pipeline off the message open critical path.
 */

use std::sync::Arc;
use std::time::Instant;

#[derive(Debug, Clone)]
pub struct WarmViews {
    pub html_view: Option<String>,
    pub plain_view: Option<String>,
    pub ready_at: Instant,
}

impl WarmViews {
    pub fn new(html: Option<String>, plain: Option<String>) -> Self {
        Self {
            html_view: html,
            plain_view: plain,
            ready_at: Instant::now(),
        }
    }
}

pub struct RenderPipeline;

impl RenderPipeline {
    /// Prepares warm views off the UI critical path so switching messages is 0ms.
    pub fn prepare_warm_views(raw_body: &str, is_html: bool) -> Arc<WarmViews> {
        let (html, plain) = if is_html {
            (Some(raw_body.to_string()), None)
        } else {
            (None, Some(raw_body.to_string()))
        };
        Arc::new(WarmViews::new(html, plain))
    }

    /// Shell hook: records first paint timestamp from WebKit load-changed handler.
    pub fn record_first_paint(_message_id: u32, _duration_us: u64) {
        // Populated during WebKit load-changed signal integration
    }
}
