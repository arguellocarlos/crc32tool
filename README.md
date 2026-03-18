# crc32tool
A fast, parallel, and reliable CRC32 hashing and verification utility written in Rust

**crc32tool** is a fast, parallel, and reliable CRC32 hashing and verification utility designed for real‑world file integrity workflows. Built in Rust for performance and safety, it provides live progress bars, multi‑threaded hashing, manifest verification, and exportable reports, all with a clean, professional command‑line interface.

The tool supports:

High‑speed CRC32 hashing using all available CPU cores

Live per‑file progress bars for large files

Double‑pass verification (compute + verify) for maximum integrity

Manifest checking against existing CRC lists

CSV/TXT export with full file paths and verification results

Recursive directory scanning with optional extension filtering

Cross‑platform builds (Windows + Linux)

Whether you're validating large downloads, generating checksum manifests, or verifying data integrity across systems, crc32tool gives you a dependable, transparent, and efficient workflow.