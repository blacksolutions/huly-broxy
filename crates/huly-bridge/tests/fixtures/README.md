# Integration test fixtures

JSON samples consumed by `huly-bridge` integration tests under
`crates/huly-bridge/tests/`.

## Naming convention

`<concept>_<role>.<ext>` — lowercase, snake_case.

- `<concept>`: the protocol concept being modelled, e.g. `hello`, `find_all`,
  `push_event_tx`, `config_json`, `login`.
- `<role>`: one of `request`, `response`, `tx`, or omitted when the concept
  itself is unambiguous (e.g. `config_json.json`).
- `<ext>`: `json` for JSON payloads, `bin` for raw msgpack/snappy frames once
  we start capturing real binary frames from a live transactor.

These fixtures are **synthetic** — they reproduce the *shape* of Huly 0.7.19
frames but use fabricated identifiers. Future phases may replace them with
captured real frames; preserve filenames when doing so to avoid breaking
existing tests.

Loaded via `common::load_fixture_json("hello_response.json")` etc.
