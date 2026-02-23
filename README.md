# Oni

An rsync-like file synchronisation tool written in Rust. Transfers only the parts
of a file that actually changed (delta transfer), preserves file metadata, and works
both locally and over a custom async TCP protocol.

## How it works

Oni implements a block-based delta algorithm inspired by the
[rsync rolling-checksum algorithm](https://rsync.samba.org/tech_report/node3.html):

1. **Receiver slices its copy** into fixed-size blocks and computes two checksums per block:
   - _Weak_: a rolling Adler-32-style checksum (O(1) to slide one byte forward)
   - _Strong_: SHA-256 (only computed on weak-checksum matches)
2. **Sender scans its file** with a sliding window the same size as a block. At each
   position it rolls the weak checksum forward cheaply; a full SHA-256 comparison is
   triggered only when the weak checksums collide.
3. **Sender emits a delta** — a sequence of `Literal` (new bytes) and `Reference`
   (index into the receiver's existing blocks) instructions.
4. **Receiver reconstructs** the new file by splicing its own blocks with the
   incoming literals.

Block size is chosen dynamically as `√(file_size)`, clamped between 4 KB and 4 MB,
and computed from the minimum of source and destination sizes so both sides agree
without extra negotiation.

## Protocol

The client and server communicate over TCP using a custom framed binary protocol.
Each frame is length-prefixed (`tokio-util` `LengthDelimitedCodec`, max 8 MB) and
serialised with `bincode`.

```
Client                                      Server
  |--- Init { src_base_dir, dest_path } ------->>|
  |<<-- InitAck ---------------------------------|
  |                                              |
  |  for each file in src (WalkDir):             |
  |--- FileMetadata --------------------------->>|
  |                                              |  file missing?
  |<<-- NeedEntireFile --------------------------|
  |--- EntireFile (or ChunkedStart/Data) ------>>|
  |                                              |  file exists but differs?
  |<<-- BlockDescriptors { checksums } ----------|
  |--- Deltas (or ChunkedStart/Data) ---------->>|
  |                                              |  file up-to-date -- skip
  |--- Fin ------------------------------------>>|
  |<<-- FinAck ----------------------------------|
```

Frames larger than 8 MB are split into `ChunkedStart` + `ChunkedData` sequences and
reassembled transparently by the `ReadConnection` before they reach application code.

## Usage

```bash
cargo build --release
```

**Remote sync** -- start the server on the destination machine, then run the client:

```bash
# remote side
./target/release/oni --server=0.0.0.0

# local side
./target/release/oni local-folder <ip-addr>:~/remote-folder
```

**Local sync**

```bash
./target/release/oni src-folder dst-folder
```

Both modes support arbitrary directory trees. The destination directory (and any
missing parents) is created automatically if it does not exist.

**Flags**

| Flag              | Description                                          |
| ----------------- | ---------------------------------------------------- |
| `--verbose`       | Print info-level logs                                |
| `--debug`         | Print debug-level logs                               |
| `--server=<addr>` | Start in server mode, listening on the given address |

## Architecture

| Module        | Responsibility                                                          |
| ------------- | ----------------------------------------------------------------------- |
| `main.rs`     | CLI parsing (clap), local vs. remote dispatch, Tokio runtime            |
| `protocol.rs` | TCP server/client, frame send/receive, file walk, delta application     |
| `delta.rs`    | Block slicing, delta computation, `FileMetadata` (mtime, permissions)   |
| `block.rs`    | Rolling weak checksum, SHA-256 strong checksum, block size selection    |
| `path.rs`     | Parses `[user@]host:path` and local path strings into a `FilePath` enum |

The client spawns three concurrent Tokio tasks: a **walker** that traverses the
source tree and enqueues `FileMetadata` frames, a **listener** that processes server
responses (computing deltas or reading files on demand), and a **sender** that drains
the outbound queue and writes to the TCP stream.
