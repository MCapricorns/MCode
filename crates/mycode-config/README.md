# mycode-config

`mycode-config` owns MYCode's strict home layout and configuration authorities. It
does not discover project configuration or load provider, plugin, session, or
CLI state.

## Canonical layout

```text
~/.mycode/
├─ config.json
└─ plugins/
   ├─ .host/auth.json
   ├─ .staging.lock
   ├─ .staging/
   │  └─ tx1-<32 lowercase hex>/{transaction.lock,journal.json,payload/}
   └─ <family>/
      └─ packs/<pack-id>/{installation.json,data/,versions/<pack-version>/}
```

`HomeLayout` constructs this hierarchy for the 5 closed `PluginFamily` values:
`providers`, `web`, `mcp`, `usage`, and `ui`. The `ui` family only carries
Theme Pack assets; the UI runtime is first-party built-in. Pack IDs use a
portable lowercase ASCII grammar; `.host` and `.staging` are reserved.

`ProviderId` is the sole provider identity used by root composition and Host
routing. It is a 1-through-64-byte lowercase ASCII slug with single hyphens;
`DefaultRoute` accepts only that canonical type. Model IDs remain exact
1-through-256-byte visible ASCII values.

`HOME` is the normal user-home source. On Windows only, an absent or empty
`HOME` falls back to `USERPROFILE`. A nonempty `MYCODE_HOME` replaces both.
Invalid higher-priority values fail closed rather than trying another source.
This platform home resolution is not a product data compatibility path.

## Bootstrap and owned files

`ensure_home_layout` eagerly creates exactly the owned root and `plugins/`.
Everything else is lazy. Existing owned directories must have current ownership
and private access: Unix `0700`, or a protected Windows DACL containing only the
current user and `SYSTEM`. Owned paths use handle-relative no-follow operations,
reject wrong-case aliases and foreign owners, and receive native durability
barriers.

Owned-file transactions create only the ancestors required by a requested
canonical path. Reads create nothing. Mutations retain a sibling lock across
read, validation, and replacement; use bounded zeroizing read buffers; write a
private same-directory temporary file; atomically rename it; verify the
published identity and access control; and sync the parent directory. These
single-file transactions are distinct from the staging protocol below.

## Normative T6 staging contract

This section freezes the T6 staging contract. The public API implements
Host-generated typed transaction IDs, lexical staging paths, the native bounded
payload writer through the durable `writing` to `staged` handoff, and native
abandoned-transaction recovery.

Staging is lazy, Host-only, and never discovered or exported. Transaction IDs
are generated from 128 OS-CSPRNG bits and have the sole persistent spelling
`tx1-[0-9a-f]{32}`; public APIs do not accept arbitrary string IDs. The global
persistent lock is `plugins/.staging.lock`. Each transaction has an empty
private `transaction.lock`, a `journal.json`, and a `payload/` tree on the same
volume as `plugins/`.

A journal is at most 1 KiB. The canonical v1 writer emits compact UTF-8 JSON and
one LF with exactly `formatVersion`, `kind="mycode-staging-transaction"`, the
matching `transactionId`, and `state`. T6 writes only `writing` and `staged`.
`committing` and `committed`, plus `commit/wal.json`, belong exclusively to T10.
The journal contains no target, action, digest, signature, trust, rollback, or
redo/undo data and is not a WAL.

A writing payload may contain 0 through 4096 link-count-one regular files; a
staged payload requires 1 through 4096. Each has at most 4096 directories and
8192 combined file-plus-directory entries. A file is at most 256 MiB and a
transaction at most 512 MiB, accumulated with checked `u64` arithmetic. Paths
reuse the lowercase portable `BundlePath` grammar and its 512-byte, 128-component,
and 128-byte-component limits. Links, reparse points, hard-link aliases, mounts,
cross-volume objects, and special files fail closed.

Lock order is blocking global lock followed by nonblocking transaction lock.
The transaction guard retains the latter through staging and handoff. Creation
makes each new directory and lock durable, then publishes `writing` before
releasing the global lock. Every journal publication follows the same exact
sequence: write a canonical private same-directory temp, flush the temp, atomic
replace, verify the published identity and access, then flush the transaction
directory. A crash temp is an unknown retained entry. Native handle-relative
exclusive payload creation, no-follow validation, file flushes, and bottom-up
directory barriers all complete before `staged`; its post-replace transaction
directory barrier completes before `StagedTransaction` is returned. A
transaction ID by itself is not a survival lease; releasing the guard without a
T10 durable claim abandons the payload.

Recovery scans at most 1024 direct `.staging` entries under the global lock; an
over-limit root causes zero deletion. It opens each existing transaction lock
without creating it, skips a busy lock, and completely preflights ownership,
access, volume, identity, journal, shape, paths, types, sizes, and counts before
modification. Only inactive exact-v1 `writing` or `staged` transactions whose
root is exactly `transaction.lock`, `journal.json`, and `payload/` are deleted,
bottom-up through native handle-relative operations with parent durability.
Missing, malformed, future, `committing`, `committed`, unknown-entry, special,
cross-volume, over-limit, raced, or I/O-failing preflights are preserved without
quarantine or repair. A native delete or barrier failure after deletion starts
returns an indeterminate failure, stops the whole recovery, preserves any
residue that still exists, and is never reported as clean. A final-parent
barrier can fail after the transaction name has disappeared, so visible residue
is not promised. Recovery of an absent `.staging` creates neither it nor
`.staging.lock`; if `.staging` exists but its existing global lock is missing or
cannot be verified, the whole recovery makes zero modifications. Path-based
`read_dir`/`remove_dir_all` recursion is forbidden.

`staged` proves only that untrusted bytes are mechanically durable, private,
same-volume, and bounded. Signed inventory completeness, digests, signatures,
source trust, installation, activation, rollback, WAL, and committed recovery
remain T10 responsibilities. T10 lock order continues from the retained
transaction lock to its coordinator/WAL lock and then authority locks sorted by
canonical path bytes; code holding any later lock never reacquires the global
staging lock.

## Authorities

All authority documents are bounded strict UTF-8 JSON. Parsing rejects duplicate
keys, malformed or partial JSON, trailing content, excessive depth or nodes, and
non-UTF-8 input. Mutations validate the current document before revision
compare-and-swap and durable replacement.

- Root `config.json` is the complete Host composition. It owns explicit
  provider/model defaults, ordered provider and usage Pack sets, UI theme
  selection, and the `web` and `mcp` singleton Pack slots.
- `plugins/<family>/packs/<pack-id>/installation.json` owns that Pack's source,
  selected artifact, trust high-water state, and sorted inventory.
- `plugins/.host/auth.json` is the only credential authority. It is created only
  by `initialize_empty_host_vault`; status reads expose only absence or revision.

For an executable Pack, `component.wasm` is the sole executable inventory path
and maps to
`plugins/<family>/packs/<pack-id>/versions/<selected.version>/component.wasm`.
`read_pack_component` reads only that owned no-follow path with a fixed 4 MiB
bound. Executable loading checks its exact bytes against the `component.wasm`
inventory row; the selected artifact digest identifies the whole artifact and
cannot replace that content digest. Declarative Packs such as themes may omit
the row and file.

The currently implemented authority APIs provide storage mechanics, strict
schemas, and revision state. The staging writer and recovery provide only the
bounded native mechanical guarantees above. Neither the authorities nor staging
verify signed inventory completeness, bundle
digests or signatures, source trust, activate artifacts, create credential
injection leases, or infer composition defaults.

## Frozen old-path policy

There is no executable recognition, migration, compatibility read, dual read,
alias, or fallback for old `.MYCode`, top-level `settings.json`, `models.json`,
auth/credential/auth-state, `plugins.lock*`, global session, sibling Pack-root,
profile/provider, Fake, M1, TOML, or Tier layouts. Existing obsolete artifacts
are outside the product and are never read, migrated, or deleted. MYCode does not
recursively clean an old root; negative tests freeze rejection and non-creation
without touching legacy secrets, unknown user data, or current Plugin state.
