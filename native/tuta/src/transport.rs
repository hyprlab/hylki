//! The SDK's HTTP client, wrapped to record what a send's submit request did
//! on the wire. The SDK's own error cannot say: a precondition reason it does
//! not know turns a 412 into an `InternalSdkError`, and so do a failed
//! serialization before the request and an unreadable reply after a 200. The
//! status seen here is what decides whether a failed send went out.
//!
//! With diagnostics on it also traces every request as method, service path
//! and status -- never the query, the body or the ids in the path.

use std::sync::{Arc, Mutex};

use tutasdk::bindings::rest_client::{
    HttpMethod, RestClient, RestClientError, RestClientOptions, RestResponse,
};

use crate::diag;

/// Where the submit goes: `SendDraftService` POST.
const SUBMIT_PATH: &str = "/rest/tutanota/senddraftservice";

/// What the submit request is known to have done.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Submit {
    /// Never left the process: nothing can have been delivered.
    NotSent,
    /// Handed to the HTTP client, but no status came back.
    NoStatus,
    /// The server answered with this status.
    Answered(u32),
}

/// One helper process runs one request, so a process-wide record is exact.
static SUBMIT: Mutex<Submit> = Mutex::new(Submit::NotSent);

/// What the submit has done so far.
pub fn submit() -> Submit {
    *SUBMIT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn record(state: Submit) {
    *SUBMIT
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = state;
}

/// `/rest/<app>/<service or type>` of a request URL: no host, query or ids.
fn service_path(url: &str) -> String {
    let path = url
        .split_once("://")
        .map_or(url, |(_, rest)| rest.find('/').map_or("", |at| &rest[at..]));
    let path = path.split(['?', '#']).next().unwrap_or_default();
    path.split('/').take(4).collect::<Vec<_>>().join("/")
}

pub struct Traced(pub Arc<dyn RestClient>);

#[async_trait::async_trait]
impl RestClient for Traced {
    async fn request_binary(
        &self,
        url: String,
        method: HttpMethod,
        options: RestClientOptions,
    ) -> Result<RestResponse, RestClientError> {
        let path = service_path(&url);
        let label = format!("{method:?} {path}");
        let is_submit = method == HttpMethod::POST && path == SUBMIT_PATH;
        if is_submit {
            record(Submit::NoStatus);
        }
        let result = self.0.request_binary(url, method, options).await;
        match &result {
            Ok(response) => {
                if is_submit {
                    record(Submit::Answered(response.status));
                }
                if diag::enabled() {
                    let reason = response
                        .headers
                        .get("precondition")
                        .map(|reason| format!(" precondition={reason}"))
                        .unwrap_or_default();
                    diag::note(&format!("{label} -> {}{reason}", response.status));
                }
            }
            Err(error) => {
                // Both are raised while the request is still being built.
                if is_submit
                    && matches!(
                        error,
                        RestClientError::InvalidURL | RestClientError::InvalidRequest
                    )
                {
                    record(Submit::NotSent);
                }
                diag::note(&format!("{label} -> {error:?}"));
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_paths_drop_host_query_and_ids() {
        assert_eq!(
            service_path("https://app.tuta.com/rest/tutanota/senddraftservice"),
            SUBMIT_PATH
        );
        assert_eq!(
            service_path("https://app.tuta.com/rest/tutanota/mail/O1sd9dd-0/O1sd9e2-3?x=1"),
            "/rest/tutanota/mail"
        );
        assert_eq!(
            service_path("https://w1.api.tuta.com/rest/sys/publickeyservice?_body=%7B%7D"),
            "/rest/sys/publickeyservice"
        );
        assert_eq!(service_path("https://app.tuta.com"), "");
    }

    /// Answers every request with `reply`.
    struct Fake(Result<u32, RestClientError>);

    #[async_trait::async_trait]
    impl RestClient for Fake {
        async fn request_binary(
            &self,
            _url: String,
            _method: HttpMethod,
            _options: RestClientOptions,
        ) -> Result<RestResponse, RestClientError> {
            self.0.clone().map(|status| RestResponse {
                status,
                headers: Default::default(),
                body: None,
            })
        }
    }

    async fn call(reply: Result<u32, RestClientError>, method: HttpMethod, url: &str) {
        let options = RestClientOptions {
            headers: Default::default(),
            body: None,
            suspension_behavior: None,
        };
        let _ = Traced(Arc::new(Fake(reply)))
            .request_binary(url.to_string(), method, options)
            .await;
    }

    // The only test touching the process-wide record, so it runs in sequence.
    #[tokio::test]
    async fn the_submit_status_is_recorded_from_the_wire() {
        let submit_url = "https://app.tuta.com/rest/tutanota/senddraftservice";
        assert_eq!(submit(), Submit::NotSent);
        call(
            Ok(200),
            HttpMethod::POST,
            "https://app.tuta.com/rest/tutanota/draftservice",
        )
        .await;
        call(Ok(200), HttpMethod::DELETE, submit_url).await;
        assert_eq!(submit(), Submit::NotSent);
        call(Ok(412), HttpMethod::POST, submit_url).await;
        assert_eq!(submit(), Submit::Answered(412));
        call(
            Err(RestClientError::NetworkError),
            HttpMethod::POST,
            submit_url,
        )
        .await;
        assert_eq!(submit(), Submit::NoStatus);
        call(
            Err(RestClientError::InvalidURL),
            HttpMethod::POST,
            submit_url,
        )
        .await;
        assert_eq!(submit(), Submit::NotSent);
    }
}
