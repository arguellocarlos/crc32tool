use clap::Parser;
use crc32fast::Hasher;
use glob::glob;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use rayon::prelude::*;
use raw_cpuid::CpuId;
use std::fs::File;
use std::time::Instant;
use std::io::{self, Read, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    Mutex,
    atomic::{AtomicUsize, Ordering},
};
use walkdir::WalkDir;

//
//  CLI
//

#[derive(Parser, Debug)]
#[command(
    author,
    version,
    about = "crc32tool is a fast, parallel CRC32 hashing and verification tool with live progress bars, manifest verification, and CSV/TXT export.",
    long_about = "crc32tool is a fast, parallel CRC32 hashing and verification tool with live progress bars, manifest verification, and CSV/TXT export.\n\n\
USAGE:\n    crc32tool [OPTIONS] <paths>...\n\n\
EXAMPLES:\n    crc32tool file.iso\n    crc32tool *.zip\n    crc32tool --recursive C:\\Downloads"
)]
struct Cli {
    /// Files or directories to process
    ///
    /// One or more files, directories, or glob patterns.
    /// Examples: file.iso, *.zip, --recursive C:\\Downloads
    #[arg(long_help = "Files or directories to process\n\n\
One or more files, directories, or glob patterns.\n\n\
Examples:\n    crc32tool file.iso\n    crc32tool *.zip\n    crc32tool --recursive C:\\Downloads")]
    paths: Vec<PathBuf>,

    /// Recurse into directories
    ///
    /// Scan directories recursively. Required when passing a directory path (Windows only).
    #[arg(short, long, long_help = "Recurse into directories\n\n\
Scan directories recursively. Required when passing a directory path (Windows only).\n\n\
Example:\n    crc32tool --recursive C:\\Downloads")]
    recursive: bool,

    /// Only include files with this extension (repeatable)
    ///
    /// Filter files by extension. Can be repeated multiple times.
    #[arg(long, long_help = "Only include files with this extension (repeatable)\n\n\
Filter files by extension. Can be repeated multiple times.\n\n\
Example:\n    crc32tool --recursive --ext zip --ext iso C:\\Downloads")]
    ext: Vec<String>,

    /// Force table output (kept for compatibility)
    ///
    /// Forces table output mode. (Currently the default; included for compatibility.)
    #[arg(long, long_help = "Force table output (kept for compatibility)\n\n\
Forces table output mode. (Currently the default; included for compatibility.)")]
    table: bool,

    /// Verify checksums from a manifest file
    ///
    /// Verify files against a checksum manifest. Manifest format: <CRC32> <full_path>
    #[arg(short, long, long_help = "Verify checksums from a manifest file\n\n\
Verify files against a checksum manifest.\n\
Manifest format: <CRC32> <full_path>\n\n\
Example:\n    crc32tool --verify checksums.txt")]
    verify: Option<PathBuf>,

    /// Run benchmark suite
    ///
    /// Run a CRC32 throughput benchmark. Size examples: 4GB, 8GB, 1GiB, etc.
    #[arg(long, long_help = "Run benchmark suite\n\n\
Run a CRC32 throughput benchmark.\n\
Size examples: 4GB, 8GB, 1GiB, etc.\n\n\
Example:\n    crc32tool --bench 4GB")]
    bench: Option<String>,

    /// Number of worker threads
    ///
    /// Set the number of worker threads for parallel hashing. Defaults to the number of CPU cores.
    #[arg(short = 'j', long, long_help = "Number of worker threads\n\n\
Set the number of worker threads for parallel hashing.\n\
Defaults to the number of CPU cores.\n\n\
Example:\n    crc32tool -j 8 C:\\Downloads")]
    threads: Option<usize>,

    /// Export results to CSV or TXT (double-pass hashing)
    ///
    /// Perform a double-pass hash (compute + verify) and export results to CSV or TXT.
    /// If the filename ends with .csv, output is CSV; otherwise, TXT.
    /// CSV fields are fully quoted; full file paths are included.
    /// Exported columns: file_path,source_crc,computed_crc,status
    #[arg(long, long_help = "Export results to CSV or TXT (double-pass hashing)\n\n\
Perform a double-pass hash (compute + verify) and export results to CSV or TXT.\n\
- If the filename ends with .csv, output is CSV\n\
- Otherwise, output is TXT\n\
- CSV fields are fully quoted\n\
- Full file paths are included\n\n\
Exported columns: file_path,source_crc,computed_crc,status\n\n\
Example:\n    crc32tool --recursive C:\\Downloads --export results.csv")]
    export: Option<PathBuf>,
}

//
//  Helpers
//

fn truncate_filename(path: &Path, width: usize) -> String {
    let s = path.to_string_lossy().to_string();
    if s.len() <= width {
        return s;
    }
    let tail = &s[s.len() - (width - 3)..];
    format!("...{}", tail)
}

fn yesno(v: bool) -> &'static str {
    if v { "\x1b[32mYES\x1b[0m" } else { "\x1b[31mNO\x1b[0m" }
}

/// Parses a size string with optional unit suffixes (e.g., "1GB", "512MB", "1024").
/// Supports: B, KB/K, MB/M, GB/G, TB/T.
/// Returns the size in bytes as u64, or None if parsing fails.
fn parse_size(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }

    let (num_str, suffix) = if s.chars().all(|c| c.is_ascii_digit()) {
        (s, "")
    } else {
        // Find the first non-digit character to split number and suffix
        let last_digit_idx = s.chars().position(|c| !c.is_ascii_digit())?;
        let (num, suf) = s.split_at(last_digit_idx);
        (num, suf)
    };

    let num: u64 = num_str.parse().ok()?;
    let multiplier = match suffix.to_uppercase().as_str() {
        "" | "B" => 1,
        "KB" | "K" => 1024,
        "MB" | "M" => 1024 * 1024,
        "GB" | "G" => 1024 * 1024 * 1024,
        "TB" | "T" => 1024 * 1024 * 1024 * 1024,
        _ => return None,
    };

    num.checked_mul(multiplier)
}

/// Detects AMD Zen microarchitecture based on CPU family and model IDs.
/// These ranges are estimates and may not be 100% accurate for all CPUs.
fn detect_zen(family: u8, model: u8) -> &'static str {
    match (family, model) {
        (23, 1..=17) => "Zen 1 (estimated)",
        (23, 49..=113) => "Zen 2 (estimated)",
        (25, 1..=63) => "Zen 3 (estimated)",
        (25, 97..=255) => "Zen 4 (estimated)",
        (26, 1..=255) => "Zen 5 (estimated)",
        _ => "Unknown",
    }
}

/// Prints CPU information and hardware acceleration capabilities.
/// CRC32 performance benefits from SSE4.2 (CRC32 instruction) and other SIMD features.
fn print_cpu_info() {
    println!("\x1b[36m=== CPU Hardware Acceleration ===\x1b[0m");

    let cpuid = CpuId::new();

    let vendor = cpuid
        .get_vendor_info()
        .map(|v| v.as_str().to_string())
        .unwrap_or_else(|| "Unknown".into());

    let (family, model) = cpuid
        .get_feature_info()
        .map(|f| (f.family_id(), f.model_id()))
        .unwrap_or((0, 0));

    let microarch = detect_zen(family, model);

    println!("Detected CPU Vendor        : {}", vendor);
    println!("Detected CPU Family/Model  : {} / {}", family, model);
    println!("Detected Microarchitecture : {}", microarch);
    println!();

    println!("SSE4.2 (CRC32 instruction) : {}", yesno(is_x86_feature_detected!("sse4.2")));
    println!("PCLMULQDQ (CLMUL)          : {}", yesno(is_x86_feature_detected!("pclmulqdq")));
    println!("AVX                        : {}", yesno(is_x86_feature_detected!("avx")));
    println!("AVX2                       : {}", yesno(is_x86_feature_detected!("avx2")));
    println!("AVX-512F (Foundation)      : {}", yesno(is_x86_feature_detected!("avx512f")));
    println!("AVX-512VL (Vector Length)  : {}", yesno(is_x86_feature_detected!("avx512vl")));
    println!("AVX-512BW (Byte/Word)      : {}", yesno(is_x86_feature_detected!("avx512bw")));
    println!();
}

//
//  File collection
//

/// Collects files from various input sources: directories, glob patterns, or individual files.
/// Handles recursive directory traversal and extension filtering.
fn collect_files(cli: &Cli) -> Vec<PathBuf> {
    let mut out = Vec::new();

    for p in &cli.paths {
        let s = p.to_string_lossy().to_string();
        let path = Path::new(&s);

        if path.is_dir() {
            if !cli.recursive {
                eprintln!("Error: '{}' is a directory. Use --recursive.", s);
                std::process::exit(1);
            }
            // Recursively walk directory and collect files
            for entry in WalkDir::new(path).into_iter().filter_map(Result::ok) {
                if entry.file_type().is_file() {
                    let file_path = entry.path().to_path_buf();

                    if cli.ext.is_empty() {
                        out.push(file_path);
                    } else if let Some(ext) = file_path.extension().and_then(|e| e.to_str()) {
                        if cli.ext.iter().any(|x| x.eq_ignore_ascii_case(ext)) {
                            out.push(file_path);
                        }
                    }
                }
            }

            continue;
        }

        // Handle glob patterns (*, ?, [ ])
        if s.contains('*') || s.contains('?') || s.contains('[') {
            for entry in glob(&s).unwrap() {
                if let Ok(path) = entry {
                    if path.is_file() {
                        out.push(path);
                    }
                }
            }
            continue;
        }

        if path.is_file() {
            out.push(path.to_path_buf());
        } else {
            eprintln!("Error: '{}' is not a file.", s);
            std::process::exit(1);
        }
    }

    if out.is_empty() {
        eprintln!("Error: no files found.");
        std::process::exit(1);
    }

    out
}

//
//  Live progress hashing (pass 1)
//

fn compute_crc32_live(
    path: &PathBuf,
    mp: &MultiProgress,
) -> io::Result<String> {
    let file = File::open(path)?;
    let metadata = file.metadata()?;
    let total_size = metadata.len();

    let truncated = truncate_filename(path, 50);

    let pb = mp.add(ProgressBar::new(total_size));
    let style = ProgressStyle::default_bar()
        .template("{prefix:<50}  {bar:12.cyan/blue}  {msg}")  // Shows filename, progress bar, and status message
        .unwrap()
        .progress_chars("=>-");

    pb.set_style(style);
    pb.set_prefix(truncated);
    pb.set_message(format!("{:<10}  {:<10}", "--------", "hashing"));

    let mut file = file;
    let mut hasher = Hasher::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut processed = 0;

    while processed < total_size {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
        processed += n as u64;
        pb.set_position(processed);
    }

    let crc = format!("{:08X}", hasher.finalize());
    pb.set_message(format!("{:<10}  {:<10}", crc, "DONE"));
    pb.finish();

    Ok(crc)
}

//
//  Silent hashing (pass 2)
//

fn compute_crc32_silent(path: &PathBuf) -> io::Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Hasher::new();
    let mut buffer = [0u8; 64 * 1024];

    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }

    Ok(format!("{:08X}", hasher.finalize()))
}

//
//  Hash mode (parallel live bars)
//

fn table_hash(files: Vec<PathBuf>, threads: Option<usize>) -> io::Result<()> {
    if let Some(t) = threads {
        rayon::ThreadPoolBuilder::new().num_threads(t).build_global().ok();
    }

    println!(
        "{:<50}  {:<14}  {:<10}  {:<10}",
        "File Name", "Progress", "CRC32", "Status"
    );

    let mp = Arc::new(MultiProgress::new());

    let ok_count = Arc::new(AtomicUsize::new(0));
    let err_count = Arc::new(AtomicUsize::new(0));

    files.par_iter().for_each(|path| {
        let mp = Arc::clone(&mp);
        let ok_count = Arc::clone(&ok_count);
        let err_count = Arc::clone(&err_count);

        match compute_crc32_live(path, &mp) {
            Ok(crc) => {
                if crc == "ERROR" {
                    err_count.fetch_add(1, Ordering::SeqCst);
                } else {
                    ok_count.fetch_add(1, Ordering::SeqCst);
                }
            }
            Err(_) => {
                err_count.fetch_add(1, Ordering::SeqCst);
            }
        }
    });

    let total = files.len();
    println!();
    println!(
        "Processed {} files: {} OK, {} ERROR",
        total,
        ok_count.load(Ordering::SeqCst),
        err_count.load(Ordering::SeqCst)
    );

    Ok(())
}

//
//  Verify mode (parallel live bars)
//

fn compute_crc32_live_verify(
    expected: &str,
    path: &PathBuf,
    mp: &MultiProgress,
) -> io::Result<(String, String, String)> {
    let file = File::open(path)?;
    let metadata = file.metadata()?;
    let total_size = metadata.len();

    let truncated = truncate_filename(path, 50);

    let pb = mp.add(ProgressBar::new(total_size));
    let style = ProgressStyle::default_bar()
        .template("{prefix:<50}  {bar:12.cyan/blue}  {msg}")
        .unwrap()
        .progress_chars("=>-");

    pb.set_style(style);
    pb.set_prefix(truncated);
    pb.set_message(format!("{:<10}  {:<10}", expected, "hashing"));

    let mut file = file;
    let mut hasher = Hasher::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut processed = 0;

    while processed < total_size {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
        processed += n as u64;
        pb.set_position(processed);
    }

    let computed = format!("{:08X}", hasher.finalize());

    let status = if computed == expected {
        "\x1b[32mOK\x1b[0m".to_string()
    } else {
        "\x1b[31mMISMATCH\x1b[0m".to_string()
    };

    pb.set_message(format!("{:<10}  {:<10}", computed, status));
    pb.finish();

    Ok((expected.to_string(), computed, status))
}

fn table_verify(manifest: &PathBuf, threads: Option<usize>) -> io::Result<()> {
    if let Some(t) = threads {
        rayon::ThreadPoolBuilder::new().num_threads(t).build_global().ok();
    }

    let file = File::open(manifest)?;
    let reader = BufReader::new(file);

    let mut entries = Vec::new();

    let is_csv = manifest.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("csv"))
        .unwrap_or(false);

    for (line_num, line_result) in reader.lines().enumerate() {
        let line = line_result?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        if is_csv && line_num == 0 && line.starts_with("\"file_path\"") {
            // Skip CSV header
            continue;
        }

        if is_csv {
            // Parse CSV format: "file_path","source_crc","computed_crc","status"
            let parts: Vec<&str> = line.split(',').collect();
            if parts.len() < 4 {
                eprintln!("Warning: Invalid CSV line {}: {}", line_num + 1, line);
                continue;
            }
            let file_path = parts[0].trim_matches('"').to_string();
            let expected_crc = parts[1].trim_matches('"').to_string();
            entries.push((expected_crc, PathBuf::from(file_path)));
        } else {
            // Parse manifest format: expected_crc filename
            let mut parts = line.split_whitespace();
            let expected = parts.next().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData,
                    format!("Line {}: missing expected CRC", line_num + 1))
            })?.to_string();
            let filename = parts.next().ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData,
                    format!("Line {}: missing filename", line_num + 1))
            })?.to_string();
            entries.push((expected, PathBuf::from(filename)));
        }
    }

    println!(
        "{:<50}  {:<14}  {:<10}  {:<10}  {}",
        "File Name", "Progress", "Expected", "Computed", "Status"
    );

    let mp = Arc::new(MultiProgress::new());

    let ok_count = Arc::new(AtomicUsize::new(0));
    let mismatch_count = Arc::new(AtomicUsize::new(0));
    let err_count = Arc::new(AtomicUsize::new(0));

    entries.par_iter().for_each(|(expected, path)| {
        let mp = Arc::clone(&mp);
        let ok_count = Arc::clone(&ok_count);
        let mismatch_count = Arc::clone(&mismatch_count);
        let err_count = Arc::clone(&err_count);

        match compute_crc32_live_verify(expected, path, &mp) {
            Ok((exp, comp, _)) => {
                if comp == "ERROR" {
                    err_count.fetch_add(1, Ordering::SeqCst);
                } else if comp == exp {
                    ok_count.fetch_add(1, Ordering::SeqCst);
                } else {
                    mismatch_count.fetch_add(1, Ordering::SeqCst);
                }
            }
            Err(_) => {
                err_count.fetch_add(1, Ordering::SeqCst);
            }
        }
    });

    let total = entries.len();

    println!();
    println!(
        "Verification summary: {} OK, {} MISMATCH, {} ERROR ({} files total)",
        ok_count.load(Ordering::SeqCst),
        mismatch_count.load(Ordering::SeqCst),
        err_count.load(Ordering::SeqCst),
        total
    );

    Ok(())
}

//
//  Export mode (double-pass + CSV/TXT)
//

fn is_csv(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| e.eq_ignore_ascii_case("csv"))
        .unwrap_or(false)
}

fn csv_escape(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        if c == '"' {
            out.push('"');
            out.push('"');
        } else {
            out.push(c);
        }
    }
    out.push('"');
    out
}

/// Exports CRC32 results to CSV or TXT file using double-pass hashing for verification.
/// First pass: compute CRC with progress bars for user feedback.
/// Second pass: recompute silently to verify consistency.
/// This ensures data integrity and detects any I/O issues.
fn export_results(files: Vec<PathBuf>, export_path: &PathBuf, threads: Option<usize>) -> io::Result<()> {
    if let Some(t) = threads {
        rayon::ThreadPoolBuilder::new().num_threads(t).build_global().ok();
    }

    let mut out_file = File::create(export_path)?;

    let use_csv = is_csv(export_path);

    if use_csv {
        writeln!(
            out_file,
            "\"file_path\",\"source_crc\",\"computed_crc\",\"status\""
        )?;
    }

    println!(
        "{:<50}  {:<14}  {:<10}  {:<10}",
        "File Name", "Progress", "CRC32", "Status"
    );

    let mp = Arc::new(MultiProgress::new());

    let ok_count = Arc::new(AtomicUsize::new(0));
    let mismatch_count = Arc::new(AtomicUsize::new(0));
    let err_count = Arc::new(AtomicUsize::new(0));

    let results: Arc<Mutex<Vec<(PathBuf, String, String, String)>>> =
        Arc::new(Mutex::new(Vec::new()));

    files.par_iter().for_each(|path| {
        let mp = Arc::clone(&mp);
        let ok_count = Arc::clone(&ok_count);
        let mismatch_count = Arc::clone(&mismatch_count);
        let err_count = Arc::clone(&err_count);
        let results = Arc::clone(&results);

        let source_crc_res = compute_crc32_live(path, &mp);  // First pass with progress
        if let Ok(source_crc) = source_crc_res {
            let computed_crc_res = compute_crc32_silent(path);  // Second pass silent
            match computed_crc_res {
                Ok(computed_crc) => {
                    let status = if computed_crc == source_crc {
                        ok_count.fetch_add(1, Ordering::SeqCst);
                        "OK".to_string()
                    } else {
                        mismatch_count.fetch_add(1, Ordering::SeqCst);
                        "MISMATCH".to_string()
                    };

                    let mut guard = results.lock().unwrap();
                    guard.push((path.clone(), source_crc, computed_crc, status));
                }
                Err(_) => {
                    err_count.fetch_add(1, Ordering::SeqCst);
                    let mut guard = results.lock().unwrap();
                    guard.push((
                        path.clone(),
                        source_crc,
                        "ERROR".to_string(),
                        "ERROR".to_string(),
                    ));
                }
            }
        } else {
            err_count.fetch_add(1, Ordering::SeqCst);
            let mut guard = results.lock().unwrap();
            guard.push((
                path.clone(),
                "ERROR".to_string(),
                "ERROR".to_string(),
                "ERROR".to_string(),
            ));
        }
    });

    // Write export file
    let results = results.lock().unwrap();
    for (path, source_crc, computed_crc, status) in results.iter() {
        let full_path = path.to_string_lossy().to_string();
        if use_csv {
            writeln!(
                out_file,
                "{},{},{},{}",
                csv_escape(&full_path),
                csv_escape(source_crc),
                csv_escape(computed_crc),
                csv_escape(status),
            )?;
        } else {
            writeln!(
                out_file,
                "{}  {}  {}  {}",
                full_path, source_crc, computed_crc, status
            )?;
        }
    }

    let total = files.len();
    println!();
    println!(
        "Exported {} files: {} OK, {} MISMATCH, {} ERROR",
        total,
        ok_count.load(Ordering::SeqCst),
        mismatch_count.load(Ordering::SeqCst),
        err_count.load(Ordering::SeqCst),
    );
    println!("Export written to: {}", export_path.to_string_lossy());

    Ok(())
}

//
//
//   BENCHMARK SUITE (Option C)
//
//

fn bench_crc32(size: u64, threads: Option<usize>) {
    if let Some(t) = threads {
        rayon::ThreadPoolBuilder::new()
            .num_threads(t)
            .build_global()
            .ok();
    }

    println!("=== CRC32 Benchmark Suite ===");
    println!("Total benchmark size: {:.2} GB", size as f64 / 1e9);

    let buffer_sizes = [4 * 1024, 64 * 1024, 1 * 1024 * 1024];

    //
    // Single-thread benchmark
    // Measures maximum single-threaded CRC32 throughput
    //
    println!("\n[1] Single-thread benchmark");

    let buffer = vec![0u8; 1 * 1024 * 1024];
    let mut hasher = Hasher::new();

    let start = Instant::now();
    let mut processed = 0;

    while processed < size {
        hasher.update(&buffer);
        processed += buffer.len() as u64;
    }

    let elapsed = start.elapsed().as_secs_f64();
    println!(
        "Throughput: {:.2} GB/s",
        (processed as f64 / 1e9) / elapsed
    );

    //
    // Multi-thread benchmark
    // Measures total throughput across all available CPU cores
    //
    println!("\n[2] Multi-thread benchmark");

    let threads_used = threads.unwrap_or_else(num_cpus::get);
    let per_thread = size / threads_used as u64;

    let start = Instant::now();

    (0..threads_used).into_par_iter().for_each(|_| {
        let mut hasher = Hasher::new();
        let buffer = vec![0u8; 1 * 1024 * 1024];
        let mut processed = 0;

        while processed < per_thread {
            hasher.update(&buffer);
            processed += buffer.len() as u64;
        }
    });

    let elapsed = start.elapsed().as_secs_f64();
    println!(
        "Total throughput: {:.2} GB/s",
        (size as f64 / 1e9) / elapsed
    );

    //
    // Buffer size comparison
    // Tests how buffer size affects throughput (memory bandwidth vs CPU overhead)
    //
    println!("\n[3] Buffer size comparison");

    for &bs in &buffer_sizes {
        let buffer = vec![0u8; bs];

        // Single-thread
        let start = Instant::now();
        let mut processed = 0;
        let mut hasher = Hasher::new();

        while processed < size {
            hasher.update(&buffer);
            processed += bs as u64;
        }

        let elapsed_single = start.elapsed().as_secs_f64();

        // Multi-thread
        let start = Instant::now();

        (0..threads_used).into_par_iter().for_each(|_| {
            let mut hasher = Hasher::new();
            let mut processed = 0;

            while processed < per_thread {
                hasher.update(&buffer);
                processed += bs as u64;
            }
        });

        let elapsed_multi = start.elapsed().as_secs_f64();

        println!(
            "{} KB: {:.2} GB/s (1 thread), {:.2} GB/s ({} threads)",
            bs / 1024,
            (size as f64 / 1e9) / elapsed_single,
            (size as f64 / 1e9) / elapsed_multi,
            threads_used
        );
    }
}


//
//  Main
//

fn main() -> io::Result<()> {
    let cli = Cli::parse();

    print_cpu_info();

    // Benchmark mode takes precedence - run benchmarks and exit
    if let Some(size_str) = cli.bench {
        let size = parse_size(&size_str).unwrap_or(4 * 1024 * 1024 * 1024);
        bench_crc32(size, cli.threads);
        return Ok(());
    }

    // Verification mode - check files against manifest
    if let Some(manifest) = cli.verify.as_ref() {
        return table_verify(manifest, cli.threads);
    }

    // Normal operation: collect files and either export or hash
    let files = collect_files(&cli);
    if let Some(export_path) = cli.export.as_ref() {
        return export_results(files, export_path, cli.threads);
    }
    // Default: hash files with live progress bars
    table_hash(files, cli.threads)
}
