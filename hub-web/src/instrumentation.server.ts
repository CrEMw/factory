// OpenTelemetry SDK for Node. SvelteKit loads this file before server code: https://svelte.dev/docs/kit/observability
// Built from separate packages, not @opentelemetry/sdk-node, which pulls in gRPC, Zipkin and Prometheus.
// Cost: of the OTEL_* variables only OTEL_EXPORTER_OTLP_*, OTEL_SERVICE_NAME, OTEL_RESOURCE_ATTRIBUTES
// and OTEL_METRIC_EXPORT_INTERVAL are read; the only transport is http/protobuf.
import { format } from 'node:util';
import { context, logs, metrics, propagation, SeverityNumber, trace } from '@opentelemetry/api';
import { AsyncLocalStorageContextManager } from '@opentelemetry/context-async-hooks';
import { W3CTraceContextPropagator } from '@opentelemetry/core';
import { OTLPLogExporter } from '@opentelemetry/exporter-logs-otlp-proto';
import { OTLPMetricExporter } from '@opentelemetry/exporter-metrics-otlp-proto';
import { OTLPTraceExporter } from '@opentelemetry/exporter-trace-otlp-proto';
import { UndiciInstrumentation } from '@opentelemetry/instrumentation-undici';
import {
  defaultResource,
  detectResources,
  resourceAttributesEnvDetector,
  resourceFromAttributes,
  serviceNameEnvDetector
} from '@opentelemetry/resources';
import { BatchLogRecordProcessor, LoggerProvider } from '@opentelemetry/sdk-logs';
import { MeterProvider, PeriodicExportingMetricReader } from '@opentelemetry/sdk-metrics';
import { BatchSpanProcessor, TracerProvider, type Span, type SpanProcessor } from '@opentelemetry/sdk-trace';

// Agent mailbox: the URL /api/mailbox/<token>?agent=&key=&msg= carries the agent key.
// SvelteKit puts the full URL into http.url when the span starts, so here, in onStart, the URL is cut
// down to /api/mailbox/:gate. Lesson `vz` of the old system: the key leaked into traces.
const MAILBOX_URL = /\/api\/mailbox\/[^?#\s]*(\?[^#\s]*)?/g;
const redactMailbox: SpanProcessor = {
  onStart(span: Span) {
    for (const [name, value] of Object.entries(span.attributes)) {
      if (typeof value === 'string' && value.includes('/api/mailbox/')) {
        span.setAttribute(name, value.replace(MAILBOX_URL, '/api/mailbox/:gate'));
      }
    }
  },
  onEnd() {},
  forceFlush: () => Promise.resolve(),
  shutdown: () => Promise.resolve()
};

// Without an endpoint the SDK does not start at all: SvelteKit spans go to the no-op API, console is untouched.
if (process.env.OTEL_EXPORTER_OTLP_ENDPOINT && process.env.OTEL_SDK_DISABLED?.toLowerCase() !== 'true') {
  const resource = defaultResource()
    .merge(resourceFromAttributes({ 'service.name': 'hub-web' }))
    .merge(detectResources({ detectors: [resourceAttributesEnvDetector, serviceNameEnvDetector] }));

  const tracerProvider = new TracerProvider({
    resource,
    spanProcessors: [redactMailbox, new BatchSpanProcessor({ exporter: new OTLPTraceExporter() })]
  });
  const meterProvider = new MeterProvider({
    resource,
    readers: [
      new PeriodicExportingMetricReader({
        exporter: new OTLPMetricExporter(),
        exportIntervalMillis: Number(process.env.OTEL_METRIC_EXPORT_INTERVAL) || 60_000
      })
    ]
  });
  const loggerProvider = new LoggerProvider({
    resource,
    processors: [new BatchLogRecordProcessor({ exporter: new OTLPLogExporter() })]
  });

  context.setGlobalContextManager(new AsyncLocalStorageContextManager().enable());
  propagation.setGlobalPropagator(new W3CTraceContextPropagator());
  trace.setGlobalTracerProvider(tracerProvider);
  metrics.setGlobalMeterProvider(meterProvider);
  logs.setGlobalLoggerProvider(loggerProvider);

  // Outgoing fetch: client span, traceparent header and the http.client.request.duration metric.
  // Works through diagnostics_channel, so it needs no import hooking (import-in-the-middle).
  new UndiciInstrumentation();

  // The app log is console: each line also goes out as an OTLP record, inside a request with its trace id.
  const logger = logs.getLogger('console');
  const levels = [
    ['error', SeverityNumber.ERROR],
    ['warn', SeverityNumber.WARN],
    ['info', SeverityNumber.INFO],
    ['log', SeverityNumber.INFO]
  ] as const;
  for (const [level, severityNumber] of levels) {
    const print = console[level].bind(console);
    console[level] = (...args: unknown[]) => {
      print(...args);
      logger.emit({ severityNumber, severityText: level.toUpperCase(), body: format(...args) });
    };
  }

  // adapter-node emits this event after closing the HTTP server and awaits the handlers:
  // https://svelte.dev/docs/kit/adapter-node#Graceful-shutdown
  process.on('sveltekit:shutdown', async () => {
    await Promise.allSettled([tracerProvider.shutdown(), meterProvider.shutdown(), loggerProvider.shutdown()]);
  });
}
