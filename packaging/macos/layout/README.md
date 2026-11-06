# #34 unsigned layout fixture

These are test bundle resources, **not an installer or production host/daemon**.
Use [the reproduction and acceptance report](../../../docs/validation/APP_LAYOUT_POC.md).
The actual `citation-manager` binary has an off-by-default `layout-smoke` feature.
Standalone `tests/fixtures/lifecycle-probe/probe.rs` is compiled without adding a
workspace crate. Both actual browser gates remain blocked; do not ship the helpers,
nonce configuration or fixture extension as product functionality.

Full selected Apache-2.0 wrapper text and author/source attribution are included.
Rights-holder notice confirmation and complete shipping-license/signing work remain #31.
PDFium archive notices are copied by the builder only after #11 hash verification.
Host manifest templates become structured JSON with explicit absolute paths and
per-browser allowlists. The fixture registrar only writes new temp registrations.
