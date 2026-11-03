# Vendored JSON library

`json.h` is the single-header C library from
[sheredom/json.h](https://github.com/sheredom/json.h).

- Upstream revision: `2312d33bfd4e68a749fa7833353a78c21308ce8e`
- Source: https://raw.githubusercontent.com/sheredom/json.h/2312d33bfd4e68a749fa7833353a78c21308ce8e/json.h
- SHA-256: `19969ab51aef4ef17efe05447b23dd79d42d2745d4006401bb1b22a94a38a9b5`
- License: Unlicense / public domain; the full notice is retained in the header.
- Local modifications: none.

The header is committed to this repository. Building does not download code or
require a separately installed JSON library. Omapower uses strict JSON parsing
(no JSON5 extensions) and validates UTF-8 and process-name lengths separately.
