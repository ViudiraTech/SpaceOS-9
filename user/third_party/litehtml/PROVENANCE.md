# litehtml provenance

Upstream: https://github.com/litehtml/litehtml
Pinned commit: d65d55b1143920133cf088cf95b3e93b53e237db
Files: include/, src/ (including bundled gumbo-parser), LICENSE (BSD-3-Clause).

Local DOM-restyle extensions for dynamic JS bindings:
- `el_text::set_text`: updates text node contents in place.
- `el_style`/`el_link`: idempotent `m_loaded` guard on `parse_attributes()`.
- `element::clear_styles_for_reapply` and `document::restyle()`: re-applies master/document/user CSS after DOM mutations (`className`/`id`/tree edits) without breaking JS element identity.

Used by user/browser as the HTML5 + CSS layout engine for Space Browser.
