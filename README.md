# DatioLabs Retail

[![CI](https://github.com/miguelcorderopamphile/datiolabs/actions/workflows/ci.yml/badge.svg)](https://github.com/miguelcorderopamphile/datiolabs/actions/workflows/ci.yml)

DatioLabs is a point-of-sale program for small shops in Venezuela. It runs on Windows,
works without an internet connection, and keeps every record on the shop's computer.

The shops it was built for have unreliable connections and modest PCs. Prices are in USD
and customers pay in bolívares (Venezuela's currency) at the BCV rate (Banco Central de
Venezuela, the country's central bank), which changes every day. A sale cannot wait for a
server, so there is no server.

## How it is built

It is a Tauri application. `datiolabs-core` is the Rust domain: catalog, stock, sales,
accounts, shifts and backups. `datiolabs-ui` is the Rust desktop shell that exposes the
commands over Tauri IPC. The frontend is a separate repository,
[datiolabs-frontend](https://github.com/miguelcorderopamphile/datiolabs-frontend), cloned
into `ui/` for local builds. It also runs as a web demo with an in-memory store.

Money uses `rust_decimal` in the core and integer cents in the frontend. I did not want a
float anywhere in a price, a tax or a change calculation. The catalog is stored as a
struct-of-arrays, so a price or stock sweep touches contiguous memory. `Sku` and `Nombre` are
fixed-size arrays with checked construction, so a row has no heap indirection.

Stock moves through an append-only tree. Each movement is signed with SHA-256 before the
first write, and the catalog stock is updated in the same operation. Sales, cancellations and
roundings are stored the same way, so the audit trail does not depend on the current state.
Persistence is Sled, with a single writer.

## The daily rate

The only external input is the BCV rate. The BCV publishes it as an HTML page, not an API.
`datiolabs-ui/src/tasa_bcv.rs` fetches it hourly with a 30-second timeout, selects the USD
box, normalizes the number (`1.234,56` and `36,92` both parse), rejects zero and out-of-range
values, and writes a signed `EventoTasaBcv` to the `tasas_bcv` tree plus a JSON cache. If the
page changes or the network fails, the last rate stays. A manual rate set by the operator
wins until they explicitly return to BCV; the manual/BCV decision is re-checked under the same
write lock as the write, so a scrape in flight cannot overwrite an operator's choice. A
missing rate blocks the sale instead of becoming zero.

It is a small pipeline, not a data platform: one source, one value, hourly. The engineering is
in the failure contract: untrusted HTML, validation, fallback, operator override and a signed
history.

## Tests

`cargo test -p datiolabs-core` runs 67 tests. The ones that matter:

| What it checks | Test |
|---|---|
| Stock is conserved across unit, pack and fraction sales | `test_invariante_conservacion_stock` |
| Cart totals do not depend on insertion order | `test_invarianza_metamorfica_total_carrito` |
| `total = payments + shortfall − change`, and change equals the overpayment | `test_cuadratura_liquidacion_multimoneda` |
| FEFO takes the oldest lot across several lots | `prop_panaderia_fefo_multi_lote` |
| A process killed mid-write leaves stock and movement count consistent | `crash_kill_keeps_movement_stock_consistent` |
| A movement without its stock update (and the reverse) still reloads, and the app keeps working | `torn_movement_only_reloads_and_continues`, `torn_product_only_reloads_and_continues` |
| Backups round-trip; tampered or truncated files are rejected | `backup_roundtrip_preserves_data`, `tampered_backup_is_rejected`, `truncated_backup_is_rejected` |

The desktop shell has six tests around the rate pipeline: USD vs EUR parsing, number formats,
the manual rate staying put across a refresh, and the cache.

The frontend has its own tests and CI in
[datiolabs-frontend](https://github.com/miguelcorderopamphile/datiolabs-frontend):
integer-cent arithmetic, fast-check properties and view-model invariants. The Tauri command
contract is audited here, against the cloned frontend: `scripts/auditar_contrato_tauri.py`
checks every command, its arguments and its registration against the frontend's `invocar()`
calls.

The end-to-end runs are documented in [`docs/e2e.md`](docs/e2e.md). The internal harness drove
the real Windows build through WebView2 and found defects in install, upgrade, restore and
shift handling. The scripts are Windows-specific and not in this repository.

## Build

```bash
cargo test -p datiolabs-core
git clone https://github.com/miguelcorderopamphile/datiolabs-frontend.git ui
cd ui && npm ci && npm run build
cd ../datiolabs-ui && cargo tauri build --target x86_64-pc-windows-msvc
```

Requires Rust stable, Node.js 20+ and WebView2. The installer is built on Windows.

## Download

The release workflow builds the Windows installer as a compile/smoke check; the latest build
is attached to the [latest release](https://github.com/miguelcorderopamphile/datiolabs/releases/latest).
The product runs from the same source.

## Demo

The engineering write-up is at [datiolabs.com/engineering](https://datiolabs.com/engineering).
The web demo is at [datiolabs.com/demo](https://datiolabs.com/demo).

## License

MIT. See [`LICENSE`](LICENSE).
