# FNZ Bank browser connector

## Scope and setup

The FNZ Bank / ebase / finvesto connector uses the authenticated FNZ web portal.
Sign in to `https://portal.fnz.de/` in GiroCrab's browser window, approve with
FNZsecure, and choose **Konten und Depot abrufen**. Cookies and credentials stay
inside the web view; the generic portal traffic recorder is disabled for FNZ.

The import covers all owner groups, with separate cash accounts and depots:

- Account balances and depot totals in account currency.
- Individual investment-fund positions: ISIN, WKN when available, quantity,
  current price and position value. Foreign unit quotes are converted using
  the explicitly directed current exchange rate from the inventory response.
- Cash-account and investment-depot transaction history, including cancellations,
  tax/fee information and the separate migrated AAB history endpoint.
- Optional PDF mailbox including its archive, assigned using each document's
  account/depot number. Already stored IDs are skipped before download.

**PDF-Postkorb inkl. Archiv** defaults to enabled. Switch it off to fetch balances,
positions and transactions without downloading PDFs, then enable it on a later
fetch. Successful PDFs survive cancellation or partial failure. Three consecutive
PDF failures stop the remaining downloads. Archived/deleted accounts receive no
new PDFs, including when archived during a download.

Other depot families (managed, stock and precious-metal depots) retain total-value
and PDF support. Their detail endpoints have different schemas and are not yet
live validated; the UI explicitly reports this limitation.

No new database schema is introduced. Existing account references are retained;
repeated fetches use stable transaction IDs and do not duplicate bookings.

## Portal requests

All API paths below are relative to the current authenticated page's
`location.origin + location.pathname + 'eox/rest/'`. Its session prefix must never
be hardcoded or persisted. Requests include cookies, `X-EOX-Session`,
`X-EOX-CLIENT-VERSION` (currently `26.4.01`) and a fresh `X-Context-ID`.
POST requests below only read data; no orders or document statuses are changed.

| Method | API path | Purpose |
| --- | --- | --- |
| GET | `depot/getDepotOverviewDataForEndCustomer` | Accounts, depots, owners and totals |
| GET | `depot-inventory/getEndCustomerDepotInventory/depotId/{id}` | Investment-fund positions |
| POST | `cash-account-transaction/getCashAccountTransactionOverviewForEndCustomer` | Cash transactions |
| POST | `order-transaction/getOrderTransactionOverviewForEndCustomer` | Depot transactions |
| POST | `order-transaction/getMigratedAabTransactionOverview` | Migrated AAB history |
| POST | `postbox/getCustomerPostboxForCustomer` | Mailbox and archive |

PDFs use the current session prefix followed by
`p/postkorb.aspx?docId=…&download=true`. Content type and `%PDF-` are validated.

History requests cover 2000-01-01 through today, in pages of 100. Cash and depot
requests include incoming and outgoing transactions; depot requests additionally
include tax, other and cancelled transactions. Migrated history has its own
pagination. Missing/changed totals, repeated pages, unavailable resources or
inconsistent account IDs abort the snapshot before balances, positions or
transactions are stored. This date range requests all available data but does not
guarantee that the bank exposes the complete history since account opening.

Mailbox requests separately page through `archived=false` and `archived=true`;
IDs are deduplicated across both lists. PDFs are stored as documents, not parsed
into additional booking rows.

## Mapping and limitations

`ownerGroups` separates owners; group totals are never imported as extra accounts.
The external account/depot number is the stable reference. Closed accounts are
skipped. Hidden, stale, absent or invalid balances reject the snapshot rather
than writing zero. The overview has no valuation timestamp: its balance date is
the retrieval date. Empty investment inventories explicitly clear old positions.

Cash transactions preserve the bank's signed amount, currency, booking date,
value date and available SEPA references. Depot payments retain their original
currency, including foreign-currency reinvestments. Current exchange rates are
not used to convert historical payments. Pricing dates are not value dates.

Some depot information events (tax, distribution or fee information) have no
payment amount in the overview. They are retained as zero-value journal events;
these rows must not be interpreted as complete cash flows. Cash-account flows
are imported separately. Transaction IDs include the full structured bank ID,
including null/empty discriminators, with separate cash/depot/migration namespaces.
Cash rows that are still only an order (no `transactionId` yet) use their own
`fnz:order:` namespace and are kept only while the portal reports them, so the
booked row replaces them instead of duplicating them. Depot events without a
payment amount must carry `numberSign` `0` (also in migrated history).

The observed fund inventory supplies **no acquisition price**. Its lifetime
profit/loss includes realized gains/distributions and is not an unrealized
per-position gain. The importer therefore leaves acquisition price unknown;
it does not fabricate a cost basis or calculated position gain from that total.

Document metadata resolves the account via
`depot.externalDepotNumber.externalDepotNumber`.
`fnz:{connection-id}:{encoded-document-id}` identifies an imported document;
the session prefix is not stored.

## Validation

On 2026-11-14 the embedded snapshot script was executed in an authenticated Safari
session. Both owner groups returned an investment depot and a cash account:

| Owner group | Depot rows | Cash rows | Earliest returned booking |
| --- | ---: | ---: | --- |
| 1 | 86 | 4 | 2023-01-03 (depot), 2023-01-04 (cash) |
| 2 | 78 | 52 | 2023-01-03 |

Both migrated-history requests returned empty lists. The latest returned bookings
were 2026-11-02 (group 1 depot), 2025-12-19 (group 1 cash), and 2026-11-03
(group 2 depot and cash). On 2026-11-14 the user also confirmed that fetching
FNZ transactions through the existing connection worked in the locally rebuilt
macOS app. The other depot-family limitations above still apply.

```sh
cargo test -p girocrab-core connectors::fnz
node --test scripts/test-fnz-portal.cjs
python3 -m unittest scripts/test_sync_docs.py
```

Synthetic tests cover owner/account separation, signed and foreign-currency
payments, neutral/cancelled events, stable IDs, current quote conversion, missing
acquisition prices, malformed snapshots, atomic validation, repeat imports,
empty inventories, mailbox/archive pagination and expired sessions.

Sources: `GiroCrabApp/Sources/GiroCrabUI/FnzUI.swift`,
`girocrab-core/src/connectors/fnz.rs` and its `snapshot` module.
