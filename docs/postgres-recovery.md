# PostgreSQL backup, restore and authority selection

The optional PostgreSQL profile provides a canonical archive and a recoverable
maintenance workflow between distinct schemas in **one PostgreSQL 18 database**.
A dedicated control schema stores the selected authority and operation journal.
The standalone server can resolve this selector and pin its dataset, incarnation
and epoch before startup. No operation falls back to a previous selection.

This profile includes reusing an archived backup after its original maintenance
operation has ended and the source has accepted newer writes. It does not cover
loss of the database/control journal, another server/database, SQLite, Selene,
rolling mixed-version deployments, production cutovers or power-loss testing.
The database still must satisfy the [PostgreSQL durability contract](postgres-durability.md).

## Configuration and commands

Install `haystack` with `--features postgres` as described in the
[standalone recipe](standalone-postgres.md). Both serving and administration use
strict version-1 TOML. Exactly one of `schema` and `selector_schema` is required.
Endpoint, credentials and transport remain explicit; the password is read only
from the named environment variable. For example, a local control configuration:

```toml
version = 1
host = "127.0.0.1"
port = 5432
database = "haystack_local"
username = "haystack_local"
selector_schema = "haystack_control"
password_env = "HAYSTACK_POSTGRES_PASSWORD"

[transport]
mode = "loopback-plaintext"
```

Use verified TLS for a non-loopback connection. A direct source configuration has
`schema = "haystack_source"` in place of `selector_schema`. The source must
already be an initialized application authority. The control and target schemas
must be distinct from it and each other. A new control schema is created by the
first export; unrelated existing schemas and pre-existing target schemas are
rejected. The schema names are bounded ASCII identifiers, not SQL fragments.

An operation ID is a bounded idempotency key. Retrying the same intent recovers
its outcome; changing its source, target, archive digest or operation kind fails.
Committed activation and matching import/restore retries report the retained
operation outcome alongside the current selected authority, even after later
maintenance is cancelled or committed. They never reactivate or reselect the
historical target.
Keep the control configuration, archive and journal. Administrative commands emit
JSON with the selected authority, operation state and available provenance.

```sh
# The first command drains in-flight managed SQL work and quiesces source writes.
haystack storage export --config control.toml --operation move_001 \
  --source haystack_source --target haystack_target --output backup.haystack
haystack storage verify --backup backup.haystack
haystack storage import --config control.toml --operation move_001 \
  --backup backup.haystack
haystack storage status --config control.toml --operation move_001
haystack storage activate --config control.toml --operation move_001
haystack storage reconcile --config control.toml --operation move_001

# The same selector configuration now resolves the committed target.
haystack serve --postgres-config control.toml --users users.toml \
  --host 127.0.0.1 --port 8080
```

The source remains quiesced between export and activation/cancellation. This is
an explicit maintenance window; writes do not accumulate behind the archive.
Long-lived SQL snapshots can delay acquisition of the exclusive schema gate
until their close/deadline. Existing listeners are not forcibly terminated and
already-returned data cannot be recalled. Their next managed admission, retained
history pull or prepared mutation is fenced. New direct-schema starts reject
quiesced, staged and retired authorities.

To make an archive and resume the original source, export under a distinct
operation and cancel it before activation. Cancellation retires an imported
stage if present and reactivates the source with another epoch. Existing owners
must be reconstructed; they do not silently become valid again.

```sh
haystack storage export --config control.toml --operation archive_001 \
  --source haystack_target --target archive_unused --output archive.haystack
haystack storage cancel --config control.toml --operation archive_001

# Later: deliberately replace the current cut with the older archived cut.
haystack storage restore --config control.toml --operation restore_002 \
  --target restored_002 --backup archive.haystack
haystack storage status --config control.toml --operation restore_002
haystack storage activate --config control.toml --operation restore_002
```

`restore` validates the complete artifact first, then quiesces the **currently
selected** source and stages a fresh target under the new operation ID. The
original artifact remains unchanged. Status reports the newer source revision
and history sequence beside the original archive manifest. Selecting the restore
intentionally discards source writes newer than that archive. Reuse requires the
same logical dataset and catalog; the producer operation need not remain active.

## Archive and identity contract

The version-1 canonical envelope contains the schema format (3), typed payload
format (1), original producer operation/schema, logical dataset, source
incarnation/epoch, entity revision, catalog selection/generation, history
authority/sequence, exact record counts and SHA-256 over the complete canonical
body. Payloads use the existing canonical typed codec encoded as canonical
base64. There is no Zinc or ordinary JSON-grid persistence conversion.

Validation rejects changed checksums, truncation, noncanonical representation,
unknown fields or payload kinds, duplicate or unsorted records, mismatched SQL
projections, unsupported schema/payload versions, another catalog and exceeded
bounds before target effects. The file is bounded to 128 MiB; entity, point,
sample and decoded-byte counts also obey the selected recovery limits. The
SHA-256 digest checks integrity, not authorship or administrator authorization.

History completeness means **all retained bounded-live data at the source cut**:
all retained samples, retained empty series, point generations and eviction
watermarks. Previously evicted samples are not reconstructed or described as
present. Entity values, nested values, references/displays, integer precision,
float bits, units and retained sample timestamps survive without coercion.

Import is one transaction. It preserves the logical dataset and archived entity
revision, and creates fresh entity/history/point incarnations, cursor key and
authority epoch. Entity feed floor becomes the archived revision with no retained
changes; history change sequence restarts at zero. Both receipt ledgers are empty.
Original source heads remain in manifest provenance, not fabricated new commit
receipts. Clients must obtain fresh cursors, subscriptions and history resources.

## Journal and interruption recovery

| Durable state | Source | Target | Next supported action |
| --- | --- | --- | --- |
| `quiesced` | Quiesced under operation/epoch | Absent | Retry export or cancel |
| `captured` | Quiesced | Absent | Retry export publication, import or cancel |
| `staged` | Quiesced | Staged, unavailable to servers | Activate or cancel |
| `committed` | Retired | Active and selected | Reconcile/finish forward |
| `cancelled` | Active under a fresh epoch | Absent or retired | Use a new operation ID |

`captured` means the journal has the immutable artifact digest; it does not prove
that a filesystem publication succeeded. A successful export result includes
`published: true`. Retry export with the same intent and output after a failed
publication. An existing identical artifact is accepted, while different content
is never overwritten. Publication uses a private same-directory temporary file,
file sync, atomic no-clobber hard-link publication, and parent-directory sync
before success. Deterministic errors remove their temporary file; a process kill
can leave an unselected `.haystack-backup-*.tmp` file. Such a file is never treated
as a successful output. Filesystem publication is qualified on the POSIX local
filesystems used by the fixture; other filesystem durability is not inferred.

Activation takes the source, target and control exclusive gates in deterministic
order and runs one `READ COMMITTED` transaction. It revalidates source
quiescence/operation/cut, target staging/identity and the selector, then retires
the source, activates the target, changes the selector and records `committed`
together. A lost response does not prove rollback. `reconcile` reads durable
state, and retrying activation is idempotent. No post-selection cancellation
reactivates the old source. Restore an older cut through another explicit
operation if that is the desired outcome.

Before selection, cancellation first proves that the target is absent or still
staged. An imported target is retired rather than silently discarded. The source
is then reactivated and selection/journal updated in one transaction. Managed
operations never publish two active authorities as a result of interruption.
An administrator issuing raw SQL outside these gates is outside this contract.

## Executable qualification

```sh
python3 scripts/postgres/qualify.py --docker
python3 scripts/postgres/standalone.py --docker
```

The first runner selects the administration tests together with the existing
PostgreSQL cases. These compare independent SQL observations of typed entities,
references, retained history/empty series/watermarks and identities. They cover
archive cancellation and reuse after newer entity/history writes, old live
handles, retained history resources, prepared entity writes, old cursors and
receipts, staged/retired startup refusal, snapshot blocking, collisions,
corruption and unrelated-schema isolation.

Interruption tests terminate real PostgreSQL backends after durable quiescence,
during the import transaction and before selection. Another terminates the
backend **after an internally acknowledged selection COMMIT and before the API
returns**, proving discarded-result reconciliation. That schedule does not prove
loss of the COMMIT acknowledgement on the wire. File-publication tests inject
errors before and after publication and verify absence-or-exact-data plus retry.
These are not power-loss or database-disaster tests.

The standalone runner installs into a clean disposable directory, exports and
verifies an archive, cancels its producer, starts a new restore operation, checks
direct-schema refusal, activates selection and serves exact archived values and
history through the selector over authenticated HTTP. Old entity/history
receipts are rejected under the new authority; signal shutdown and SQL session
cleanup remain independently checked. The current format-3 runtime is tested;
the historical format-2 rejection guard was inspected in source, not qualified
with a previous executable or a rolling mixed-version deployment.
