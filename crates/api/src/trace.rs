use axum::extract::{MatchedPath, Request};
use tower_http::{
    classify::{ServerErrorsAsFailures, SharedClassifier},
    trace::{DefaultOnResponse, TraceLayer},
};
use tracing::{Level, Span};

use crate::RequestId;

pub(crate) type Layer = TraceLayer<SharedClassifier<ServerErrorsAsFailures>, fn(&Request) -> Span>;

pub(crate) fn layer() -> Layer {
    TraceLayer::new_for_http()
        .make_span_with(make_span as fn(&Request) -> Span)
        .on_response(DefaultOnResponse::new().level(Level::INFO))
}

/// Method, route template and request id only: the raw path, query, headers and body may carry
/// names or tokens and never reach the logs.
fn make_span(request: &Request) -> Span {
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map_or("<unmatched>", MatchedPath::as_str);
    let request_id = request
        .extensions()
        .get::<RequestId>()
        .map_or("", RequestId::as_str);
    tracing::info_span!("request", method = %request.method(), route, request_id)
}
