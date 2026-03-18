## Command Line Reference

crc32tool is a fast, parallel CRC32 hashing and verification tool with live progress bars, manifest verification, and CSV/TXT export.

## Usage

```sh
crc32tool [OPTIONS] <paths>...
```

## Options

### Recursive
`--recursive`, `-r` 

Scan directories recursively. Required when passing a directory path (**Windows only**).

Example:

```sh
crc32tool --recursive C:\Downloads
```

---

### Extension

`--ext <extension>`

Filter files by extension.
Can be repeated multiple times.

Example:

```sh
crc32tool --recursive --ext zip --ext iso C:\Downloads
```

### Threads

`--threads <N>`, `-j <N>`

Set the number of worker threads for parallel hashing.
Defaults to the number of CPU cores.

Example:

```sh
crc32tool -j 8 C:\Downloads
```

### Verify

`--verify <manifest>`, `-v <manifest>`

Verify files against a checksum manifest.
Manifest format:

`<CRC32> <full_path>`

Example:

```sh
crc32tool --verify checksums.txt
```

### Export

`--export <file>`

Perform a double‑pass hash (compute + verify) and export results to CSV or TXT.

- If the filename ends with `.csv`, output is CSV
- Otherwise, output is TXT
- CSV fields are fully quoted
- Full file paths are included

Exported columns:

`file_path,source_crc,computed_crc,status`

Example:

```sh
crc32tool --recursive C:\Downloads --export results.csv
```

### Table

`--table`

Forces table output mode.
(Currently the default; included for compatibility.)

### Benchmark

`--bench <size>`

Run a CRC32 throughput benchmark.
Size examples: `4GB`, `8GB`, `1GiB`, etc.

Example:

```sh
crc32tool --bench 4GB
```

### Arguments

`<paths>...`

One or more files, directories, or glob patterns.

Examples:

```sh
crc32tool file.iso
crc32tool *.zip
crc32tool --recursive C:\Downloads
```

## Features Summary

- Parallel CRC32 hashing (Rayon)
- Live progress bars for each file
- Table-style terminal output
- Manifest verification (--verify)
- Double-pass hashing with export (--export)
- CSV/TXT output with full paths
- CPU feature detection (SSE4.2, AVX, AVX2, AVX‑512)
- Recursive directory scanning
- Extension filtering
- Thread control
- Benchmark mode