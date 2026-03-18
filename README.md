## crc32tool
A fast, parallel, and reliable CRC32 hashing and verification utility written in Rust

**crc32tool** is a fast, parallel, and reliable CRC32 hashing and verification utility designed for real‑world file integrity workflows. Built in Rust for performance and safety, it provides live progress bars, multi‑threaded hashing, manifest verification, and exportable reports, all with a clean, professional command‑line interface.

The tool supports:

- **High‑speed CRC32** hashing using all available CPU cores
- **Live per‑file progress bars** for large files
- **Double‑pass verification** (compute + verify) for maximum integrity
- **Manifest checking** against existing CRC lists
- **CSV/TXT export** with full file paths and verification results
- **Recursive directory scanning** for Windows with optional extension filtering
- **Cross‑platform builds** (Windows + Linux)

Whether you're validating large downloads, generating checksum manifests, or verifying data integrity across systems, crc32tool gives you a dependable, transparent, and efficient workflow.

## Motivation

File integrity tools exist, but none of the common ones fully solve the problem in a modern, efficient, hardware‑accelerated way. I created crc32tool to fill that gap and to learn some Rust programming.

## Limitations of Existing Tools

### Teracopy

Teracopy includes CRC32 verification, but:

- It is single‑threaded
- It does not use AVX, AVX2, or AVX‑512 acceleration
- It relies on the Windows API for CRC32, which only uses SSE4.2 if available
- It cannot export results in structured formats (CSV/TXT)
- It does not support Linux
- It does not provide manifest verification
- It does not offer parallel hashing or multi‑file progress visibility

### rsync

rsync is a powerful synchronization tool, but:

- It does not use CRC32 for final file integrity
- It uses rolling checksums (Adler‑32 style) for block matching, not verification
- It uses MD5/SHA1/xxHash depending on build, but no hardware acceleration
- It is single‑threaded
- It provides no live per‑file progress bars
- It cannot export verification results (Yes, I know we can use >> to export to stdout)
- It is not designed for standalone hashing workflows
- rsync is excellent for network synchronization, but not for fast, parallel integrity checking.

**crc32tool** was built to provide a modern, fast, and transparent CRC32 workflow that neither Teracopy nor rsync offers.

- Uses SSE4.2 CRC32 instruction when available, with clean fallback
- Fully multi‑threaded using all CPU cores
- Each file gets its own real‑time progress indicator
- Computes CRC32, then immediately verifies it, Teracopy‑style integrity checking
- Reads and validates existing checksum lists

Produces structured reports with:

- Full file paths
- Source CRC
- Computed CRC
- Status

It is cross platform (Linux, Windows), written in Rust