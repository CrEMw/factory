//! OpenTelemetry SDK: traces, metrics and logs are sent over OTLP (http/protobuf).
//! Endpoint, headers, service name, sampling and intervals are read from the standard OTEL_* variables:
//! https://docs.rs/opentelemetry-otlp/0.33.1/opentelemetry_otlp/#environment-variables
//! The stdout log always stays; the tracing -> OTel bridge is added only when an endpoint is set.

use std::{sync::LazyLock, time::Instant};

use anyhow::Context;
use axum::{
    extract::{MatchedPath, Request},
    middleware::Next,
    response::Response,
};
use opentelemetry::{
    KeyValue, global, metrics::Histogram, propagation::TextMapPropagator, trace::TracerProvider,
};
use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use opentelemetry_http::HeaderExtractor;
use opentelemetry_otlp::{LogExporter, MetricExporter, SpanExporter};
use opentelemetry_sdk::{
    Resource, logs::SdkLoggerProvider, metrics::SdkMeterProvider,
    propagation::TraceContextPropagator, trace::SdkTracerProvider,
};
use tracing::{Instrument, field::Empty, level_filters::LevelFilter};
use tracing_opentelemetry::OpenTelemetrySpanExt;
use tracing_subscriber::{Layer, filter::filter_fn, layer::SubscriberExt, util::SubscriberInitExt};

/// Providers live until the process ends; `None` when export is off.
pub struct Telemetry(Option<(SdkTracerProvider, SdkMeterProvider, SdkLoggerProvider)>);

/// Export is on only if `OTEL_EXPORTER_OTLP_ENDPOINT` is set and `OTEL_SDK_DISABLED=true` is not.
pub fn export_enabled() -> bool {
    std::env::var_os("OTEL_EXPORTER_OTLP_ENDPOINT").is_some_and(|v| !v.is_empty())
        && !std::env::var("OTEL_SDK_DISABLED").is_ok_and(|v| v.eq_ignore_ascii_case("true"))
}

/// Installs the tracing subscriber. Called before tokio starts: the exporters keep their own threads
/// with a blocking HTTP client, which can be neither created nor shut down inside the runtime.
/// A bad export setting (for example `OTEL_EXPORTER_OTLP_PROTOCOL=grpc`: only http/protobuf is built in)
/// does not fail the service: one line to stderr, then work continues without export.
pub fn init(export: bool) -> Telemetry {
    let providers = export.then(providers).and_then(|built| {
        built
            .inspect_err(|err| eprintln!("telemetry disabled: {err:#}"))
            .ok()
    });
    let (spans, logs) = match &providers {
        Some((traces, _, logs)) => (
            Some(tracing_opentelemetry::layer().with_tracer(traces.tracer("hub"))),
            // Messages of the SDK itself and its HTTP client do not go to OTLP, otherwise an export failure would produce
            // a new record and a new export in a loop. They are in stdout.
            Some(
                OpenTelemetryTracingBridge::new(logs).with_filter(filter_fn(|m| {
                    !["opentelemetry", "reqwest", "hyper", "h2", "rustls"]
                        .iter()
                        .any(|own| m.target().starts_with(own))
                })),
            ),
        ),
        None => (None, None),
    };
    // Span fields are not written to the stdout line: the log format is the same as before spans existed.
    let stdout = tracing_subscriber::fmt::layer()
        .json()
        .with_current_span(false)
        .with_span_list(false);
    tracing_subscriber::registry()
        .with(LevelFilter::INFO)
        .with(spans)
        .with(logs)
        .with(stdout)
        .init();
    Telemetry(providers)
}

fn providers() -> anyhow::Result<(SdkTracerProvider, SdkMeterProvider, SdkLoggerProvider)> {
    // Resource::builder() reads OTEL_SERVICE_NAME and OTEL_RESOURCE_ATTRIBUTES itself.
    let mut resource = Resource::builder();
    if std::env::var_os("OTEL_SERVICE_NAME").is_none_or(|v| v.is_empty()) {
        resource = resource.with_service_name("hub");
    }
    let resource = resource.build();
    // build() without with_http()/with_tonic(): one transport, http/protobuf (the http-proto feature in Cargo.toml).
    let traces = SdkTracerProvider::builder()
        .with_resource(resource.clone())
        .with_batch_exporter(
            SpanExporter::builder()
                .build()
                .context("OTLP span exporter")?,
        )
        .build();
    let metrics = SdkMeterProvider::builder()
        .with_resource(resource.clone())
        .with_periodic_exporter(
            MetricExporter::builder()
                .build()
                .context("OTLP metric exporter")?,
        )
        .build();
    let logs = SdkLoggerProvider::builder()
        .with_resource(resource)
        .with_batch_exporter(
            LogExporter::builder()
                .build()
                .context("OTLP log exporter")?,
        )
        .build();
    global::set_meter_provider(metrics.clone());
    Ok((traces, metrics, logs))
}

impl Telemetry {
    #[cfg(test)]
    pub fn exporting(&self) -> bool {
        self.0.is_some()
    }

    /// Flushes what has accumulated. An export failure at exit does not change the exit code.
    pub fn shutdown(self) {
        if let Some((traces, metrics, logs)) = self.0 {
            for (signal, result) in [
                ("traces", traces.shutdown()),
                ("metrics", metrics.shutdown()),
                ("logs", logs.shutdown()),
            ] {
                if let Err(err) = result {
                    eprintln!("telemetry shutdown ({signal}): {err}");
                }
            }
        }
    }
}

// Without a provider global::meter returns a no-op: recording costs nothing and sends nothing.
// https://opentelemetry.io/docs/specs/semconv/http/http-metrics/#metric-httpserverrequestduration
static REQUEST_DURATION: LazyLock<Histogram<f64>> = LazyLock::new(|| {
    global::meter("hub")
        .f64_histogram("http.server.request.duration")
        .with_unit("s")
        .with_boundaries(vec![
            0.005, 0.01, 0.025, 0.05, 0.075, 0.1, 0.25, 0.5, 0.75, 1.0, 2.5, 5.0, 7.5, 10.0,
        ])
        .build()
});

/// Every request: a span with method, route and status (parent from the traceparent header) and a histogram point.
pub async fn observe(req: Request, next: Next) -> Response {
    let method = req.method().as_str().to_owned();
    // The route template, not the path: unmatched requests have no route, and label cardinality does not grow.
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map(|p| p.as_str().to_owned());
    let name = match &route {
        Some(route) => format!("{method} {route}"),
        None => method.clone(),
    };
    let span = tracing::info_span!(
        "request",
        otel.name = name,
        otel.kind = "server",
        otel.status_code = Empty,
        http.request.method = method,
        http.route = route,
        http.response.status_code = Empty,
        // The caller name is set by callers::require: verified or `unauthenticated`; the claimed one without verification
        // goes separately to hub.caller_claimed. The secret never gets into the span.
        hub.caller = Empty,
        hub.caller_claimed = Empty,
        // Actor kind of the write (tx.rs): human, agent, service:<name>; the identifier itself does not go into the span.
        hub.actor_kind = Empty,
    );
    // Without the OTel layer there is nowhere to set the parent: the error is expected and means nothing.
    let _ = span.set_parent(TraceContextPropagator::new().extract(&HeaderExtractor(req.headers())));

    let start = Instant::now();
    let res = next.run(req).instrument(span.clone()).await;
    let status = res.status().as_u16();
    span.record("http.response.status_code", status);
    if status >= 500 {
        span.record("otel.status_code", "ERROR");
    }
    let mut labels = vec![
        KeyValue::new("http.request.method", method),
        KeyValue::new("http.response.status_code", i64::from(status)),
    ];
    labels.extend(route.map(|route| KeyValue::new("http.route", route)));
    REQUEST_DURATION.record(start.elapsed().as_secs_f64(), &labels);
    res
}

/// Span of a database query: sqlx makes no spans of its own.
pub fn db_span(operation: &'static str) -> tracing::Span {
    tracing::info_span!(
        "db",
        otel.name = operation,
        otel.kind = "client",
        db.system.name = "postgresql",
    )
}
