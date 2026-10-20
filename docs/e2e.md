# End-to-end runs

The end-to-end suite drove the real Windows build through WebView2 over CDP. It clicked the
actual interface and went through Tauri IPC, with no browser shim and no mocks. I used it to
find defects in flows that unit tests cannot reach: install, upgrade, restore, shift
boundaries and misuse.

The scripts depend on Windows, a Tauri build and a disposable dataset, so they are not part
of this repository. These are the cases that mattered:

## Install, upgrade and restore

- Restoring a backup that already carries a PIN. The wizard must not deadlock when the
  restored configuration skips steps.
- Upgrading over the installed build. Data, license and backups have to survive; the check is
  a hash comparison.
- Reinstalling over existing data. The installer must not reset it.
- The final installer must not carry the debug flag or the CDP port.
- Restoring with an open shift. It has to be blocked by a guard, not corrupt the shift.
- A corrupt backup must be rejected before the current data is touched.

## Shifts and time

- Midnight rollover in VET. Sales before and after the boundary, and closing at 00:00, have
  to land in the right shift.
- Each guarded operation is rejected outside the shift, and the store can reopen and keep
  working.

## Sales, concurrency and misuse

- The same command UUID must not create a second sale, whether the calls are parallel or
  come from the UI.
- Five concurrent sales against a fixed stock. The final stock has to be exact, and an
  oversell has to be rejected.
- A sale that fails halfway must not leave a partial stock change.
- A real double click on Charge must not create two sales.
- Zero, negative and huge quantities, and special or Unicode strings in labels and notes.
- Mixed payments in USD, bolívares and other methods, with change equal to the overpayment.

## Accounts and inventory

- FEFO inside an open account. A perishable consumed there has to take the oldest lot.
- A debt converted from an account survives the shift close.
- Adjustments and sales cannot push stock below zero.
- Deleting a product that has history. The history has to remain.

## Rates

- A manual rate stays until going back to BCV, and the rate at charge time wins over the
  preview.
- Extreme rates (0.0001 and 999999) still charge coherently.
