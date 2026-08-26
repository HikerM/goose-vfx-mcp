# Lumina security patch

This directory vendors `pctx_config` 0.1.5 under its MIT license. Lumina removes the unused
OpenTelemetry/OTLP configuration module and exporter dependencies so packaged builds cannot
enable that remote telemetry path. MCP server, authentication, logger, and tool-disclosure
configuration APIs remain compatible with the upstream crate.
