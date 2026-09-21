# hafley-observe

Shared tracing configuration for hafley-rs binaries. `HAFLEY_OTLP_ENDPOINT`
turns on OTLP/HTTP span export alongside the formatter; unset, the process
keeps the formatter only. End every `init` caller's `main` with
`hafley_observe::shutdown()` to flush the last batch.

Run the local DuckDB viewer, then run a binary with the endpoint set:

    otel-desktop-viewer --db /tmp/observe.duckdb --open-browser=false
    HAFLEY_OTLP_ENDPOINT=http://127.0.0.1:4318/v1/traces ./your-binary

The DuckDB file stays locked while the viewer runs. Stop it, then query:

    duckdb -readonly /tmp/observe.duckdb "SELECT name, (end_time-start_time)/1000 dur_us FROM spans ORDER BY start_time"
