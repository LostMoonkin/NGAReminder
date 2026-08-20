---
status: accepted
---

# Separate no-fetch periods from fetch schedules

Monitoring targets store no-fetch periods independently from their dynamic fetch schedule. A schedule continues to answer how often a target is fetched, while a no-fetch period suppresses only automatic collection, records one normal skip for each contiguous covered window, and permits explicit manual runs. This separation preserves the existing schedule contract, avoids overloading notification-oriented uses of “silent”, and lets one shared run coordinator apply identical trigger, lease, audit, and skip semantics before dispatching thread or user collectors.

## Considered Options

- Extend schedule rules with fetch/skip actions. Rejected because rule ordering would mix frequency selection with suppression and make the existing fallback behavior harder to explain and preserve.
- Record a skip at every normal interval. Rejected because a short interval would produce hundreds of audit rows during one overnight window.
- Implement suppression independently inside both collectors. Rejected because trigger provenance, leases, and skip bookkeeping would diverge between thread and user targets.

## Consequences

The public watch contract and both database backends gain no-fetch configuration and run-trigger provenance. Scheduled runs are suppressed before NGA I/O, manual runs bypass no-fetch periods but not paused or invalid target states, and the administration UI can expose the active no-fetch window without introducing a new persisted watch status.
