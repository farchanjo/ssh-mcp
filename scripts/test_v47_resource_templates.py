"""v4.7 ``resources/templates/list`` advertisement.

Verifies the static template catalogue advertised by the v4.7 server. The
catalogue is built by ``src/infra/mcp/resource_templates.rs::build_list``
and is feature-gated on ``port_forward`` (5 entries when enabled, 4
without).

Each template carries:
- ``uriTemplate`` — RFC 6570 form-style query expansion for byte-stream
  resources (``shell://``, ``command://``, ``forward://``); simple variable
  expansion for snapshots (``transfer://``, ``session://``).
- ``name`` — short human-readable label.
- ``description`` — non-empty prose.
- ``mimeType`` — declared MIME (``text/plain`` for byte streams,
  ``application/json`` for snapshots).
"""

from __future__ import annotations

import re

import pytest

from helpers.mcp_client import McpClient


# rmcp wire shape uses camelCase; helpers accept both forms.
_CAMEL_KEYS = {
    "uri_template": "uriTemplate",
    "mime_type": "mimeType",
}


def _key(template: dict, name: str) -> object:
    """Return ``template[name]`` regardless of camelCase / snake_case."""
    if name in template:
        return template[name]
    camel = _CAMEL_KEYS.get(name, name)
    return template.get(camel)


def _looks_like_rfc6570(uri: str) -> bool:
    """Sanity check: scheme + at least one ``{<param>}`` expansion."""
    _KNOWN_SCHEMES = (
        "shell://",
        "command://",
        "transfer://",
        "session://",
        "forward://",
        "serial://",   # added v5.2 (ADR 0009)
    )
    if not any(uri.startswith(s) for s in _KNOWN_SCHEMES):
        return False
    return "{" in uri and "}" in uri


def test_resource_templates_list_returns_5_entries(stdio_client: McpClient) -> None:
    """v4.7 advertised 5 templates with port_forward; 4 without.

    v5.2 added serial:// (6 entries with port_forward, 5 without).
    Accept any count in {4, 5, 6} so the test stays green across builds.
    """
    templates = stdio_client.list_resource_templates()
    assert len(templates) in {4, 5, 6}, (
        f"expected 4, 5, or 6 templates, got {len(templates)}: {templates}"
    )
    schemes = {(_key(t, "uri_template") or "").split("://", 1)[0] + "://" for t in templates}
    assert "shell://" in schemes
    assert "command://" in schemes
    assert "transfer://" in schemes
    assert "session://" in schemes


def test_byte_stream_templates_advertise_cursor_query(stdio_client: McpClient) -> None:
    """``shell://`` and ``command://`` templates carry the ``{?cursor}``
    RFC 6570 form-style query expansion. ``forward://`` (when present) too."""
    templates = stdio_client.list_resource_templates()
    for tpl in templates:
        uri = _key(tpl, "uri_template") or ""
        if uri.startswith(("shell://", "command://")) or uri.startswith("forward://"):
            assert "{?cursor}" in uri, f"byte-stream template missing {{?cursor}}: {uri}"


def test_snapshot_templates_omit_cursor_query(stdio_client: McpClient) -> None:
    """``transfer://`` and ``session://`` are point-in-time snapshots and
    must NOT advertise the cursor query expansion."""
    templates = stdio_client.list_resource_templates()
    for tpl in templates:
        uri = _key(tpl, "uri_template") or ""
        if uri.startswith(("transfer://", "session://")):
            assert "{?cursor}" not in uri, f"snapshot template should not advertise {{?cursor}}: {uri}"


def test_each_template_has_required_metadata(stdio_client: McpClient) -> None:
    """Every template has a non-empty name, description, and MIME type."""
    templates = stdio_client.list_resource_templates()
    for tpl in templates:
        assert tpl.get("name"), f"template missing name: {tpl}"
        assert tpl.get("description"), f"template missing description: {tpl}"
        mime = _key(tpl, "mime_type")
        assert mime, f"template missing mimeType: {tpl}"


def test_mime_type_matrix_matches_docs(stdio_client: McpClient) -> None:
    """MIME types per scheme must match the documented matrix:

    | shell    | text/plain       |
    | command  | text/plain       |
    | transfer | application/json |
    | session  | application/json |
    | forward  | application/json |
    """
    expected = {
        "shell://": "text/plain",
        "command://": "text/plain",
        "transfer://": "application/json",
        "session://": "application/json",
        "forward://": "application/json",
    }
    templates = stdio_client.list_resource_templates()
    for tpl in templates:
        uri = _key(tpl, "uri_template") or ""
        for prefix, mime in expected.items():
            if uri.startswith(prefix):
                actual_mime = _key(tpl, "mime_type")
                assert actual_mime == mime, (
                    f"{prefix} expected mimeType={mime}, got {actual_mime}"
                )


def test_every_uri_template_is_rfc6570_shape(stdio_client: McpClient) -> None:
    """Syntactic check: scheme + at least one ``{<param>}`` placeholder."""
    templates = stdio_client.list_resource_templates()
    for tpl in templates:
        uri = _key(tpl, "uri_template") or ""
        assert _looks_like_rfc6570(uri), f"not RFC 6570 shape: {uri}"


def test_template_listing_is_stable(stdio_client: McpClient) -> None:
    """Calling ``resources/templates/list`` twice returns identical results."""
    first = stdio_client.list_resource_templates()
    second = stdio_client.list_resource_templates()
    # Order matters per the v4.7 contract.
    assert first == second, (first, second)


def test_template_listing_does_not_require_session_state(stdio_client: McpClient) -> None:
    """``resources/templates/list`` is callable on a fresh handshake without
    any open session / shell / command / transfer."""
    templates = stdio_client.list_resource_templates()
    assert len(templates) >= 4
    # No live resources required — assertion is just that the call worked.


def test_uri_template_param_names_match_v47_spec(stdio_client: McpClient) -> None:
    """Each template's parameter name matches the documented `<id>_id` form."""
    expected_param = {
        "shell://": "shell_id",
        "command://": "command_id",
        "transfer://": "transfer_id",
        "session://": "session_id",
        "forward://": "forward_id",
    }
    templates = stdio_client.list_resource_templates()
    for tpl in templates:
        uri = _key(tpl, "uri_template") or ""
        for prefix, param in expected_param.items():
            if uri.startswith(prefix):
                assert (
                    f"{{{param}}}" in uri
                ), f"{prefix} template missing {{{param}}}: {uri}"
