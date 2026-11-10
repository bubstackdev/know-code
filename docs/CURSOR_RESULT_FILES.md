# Coordinator-owned cursor result files

`crates/core/src/cursor/result_store.rs` implements the filesystem helper specified in [CURSOR_INTEGRATION.md](CURSOR_INTEGRATION.md). It stores opaque, already-computed result bytes; the coordinator retains ownership of cursor identity, graph execution, the shared quota, deadlines and retirement. This helper alone does not establish integrated pagination or release acceptance.

## API and ownership

`ResultStore::open(&Path)` opens a dedicated private directory. Calls require external serialization under the coordinator ownership fence. The caller reserves all expected file bytes in the shared quota **before** `begin`; no independent quota pool is introduced.

| Operation | Behavior |
| --- | --- |
| `begin(key, expected_bytes)` | Creates an empty `<key>.part` with mode 0600, without preallocation. An identical active begin is retryable; conflicting lengths, existing completed files and untracked prior uploads are rejected. |
| `append(key, offset, bytes) -> u64` | Accepts the next exact offset, or an entirely contained identical retry, returning the accepted total offset. Gaps, conflicts, partially overlapping new bytes, overflow and overruns fail. |
| `complete(key, sha256) -> StoredResult` | Checks the exact declared length and actual SHA-256, sets mode 0400 and atomically renames to `<key>.ready` without replacement. An identical completed retry is accepted. |
| `complete_with(key, sha256, check)` | Performs the same completion with a caller-supplied `FnMut() -> Result<()>` check at admission, between bounded hash reads and before publication. The integrator can pass its existing execution/deadline check. |
| `get(key) -> StoredResult` | Returns the accepted absolute path, bytes and lowercase SHA-256 after checking file identity, length and private immutable mode. Missing or unknown completed results invalidate the cursor. |
| `remove(key)` | Removes owned partial/ready entries; absent entries are harmless. Only success clears the helper's in-memory state. |
| `reset()` | Removes well-shaped owned files from the dedicated directory, including uploads/completions from a prior incarnation. Errors preserve in-memory state for retry. |

Keys must contain exactly 32 lowercase hexadecimal characters. SHA-256 completion values must contain exactly 64. The shared constants bound each saved file to 16 MiB and each append/hash buffer to 64 KiB. A declaration does not reserve RAM or create a full-size file. Appends compare at most one chunk for a retry. Completion hashes at most the declared file guard; `get` uses the accepted checksum rather than rehashing the full file on every page.

## Filesystem safety and recovery

Directory creation/open walks individual components using descriptor-relative operations with no-follow flags; parent traversal and symlink components are rejected. The final directory must be owned by the effective user with mode 0700. File operations are anchored to its retained descriptor and reject replacement of the final directory, symlinks, nonregular files, hardlinks, wrong owners and nonprivate permissions. Completion uses macOS exclusive rename, so an existing ready entry is never overwritten. The Linux equivalent is present but does not establish a supported release platform.

Completed files are immutable through this API. These permissions and identity checks protect the coordinator-managed lifecycle; they do not promise protection against a hostile process with the same user credentials changing a file in place. Cleanup inspects entries without opening or following them, allowing deletion of owned regular files even if their read permission was lost. Unknown directory entries and unsafe file types produce errors and remain untouched. Reset scans at most 4,096 entries before attempting any deletion; larger directories fail explicitly with `resource_limited` rather than allocating or scanning without a bound.

Cleanup can partly succeed before encountering an error. It deliberately retains state on any failure: the coordinator must keep the corresponding reservation charged until cleanup succeeds. Retry treats already-absent files as success. The same rule applies to a failed begin, append or completion: the caller must complete cleanup before releasing retained-byte reservations. An already-open read descriptor remains valid after a successful unlink, so its reader can finish a coherent page.

There is no per-chunk fsync and no persistent helper metadata. `open` does not silently clear or restore prior files. After acquiring exclusive coordinator ownership at startup, the caller explicitly invokes `reset` before admitting cursors for the new incarnation. Crashes invalidate prior tokens; leftover partial/ready files are cleanup state, never restored cursors. Startup reset is bounded by the entry guard rather than an individual query deadline. No live indexes or source files belong in this directory.

## Focused validation

The inline `result_store` tests exercise length/chunk/offset guards, conflicting and identical retries, actual-byte hash verification, exclusive publication, immutable completion, source-independent Unicode bytes, deadline interruption, symlink/hardlink/FIFO/directory rejection, cleanup retry after partial success, lost read permissions, explicit restart cleanup, preservation of unrelated files, directory replacement, the reset scan guard and an open reader surviving unlink.

Run in an isolated target directory:

```sh
export PATH="$HOME/.cargo/bin:$HOME/.dotnet:$HOME/.local/bin:$PATH"
export MACOSX_DEPLOYMENT_TARGET=15.0
export CARGO_TARGET_DIR="$PWD/.artifacts/cursor-result-files/target"
cargo test -p know-code-core --lib result_store --locked --offline
cargo clippy -p know-code-core --lib --tests --locked --offline -- -D warnings
cargo fmt --all -- --check
```

This work does not run provider/native/package suites or measure performance. The integrator must still wire registry retirement before quota release, pass the shared deadline into completion and validate the assembled control/native/CLI/MCP boundary.
