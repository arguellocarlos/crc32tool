use clap::Parser;
use crc32fast::Hasher;
use glob::glob;
use indicatif::{MultiProgress, ProgressBar, ProgressStyle};
use rayon::prelude::*;
use raw_cpuid::CpuId;
use std::fs::File;
use std::io::{self, Read, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    Mutex,
    atomic::{AtomicUsize, Ordering},
};
use walkdir::WalkDir;

//  CLI

#[derive(Parser, Debug)]
#[command(author, version, about)]
struct Cli {
    /// Files or directories to process
    paths: Vec<PathBuf>,

    /// Recurse into directories
    #[arg(short, long)]
    recursive: bool,

    /// Only include files with this extension (repeatable)
    #[arg(long)]
    ext: Vec<String>,

    /// Force table output (kept for compatibility)
    #[arg(long)]
    table: bool,

    /// Verify checksums from a manifest file
    #[arg(short, long)]
    verify: Option<PathBuf>,

    /// Run benchmark suite (placeholder)
    #[arg(long)]
    bench: Option<String>,

    /// Number of worker threads
    #[arg(short = 'j', long)]
    threads: Option<usize>,

    /// Export results to CSV or TXT (double-pass hashing)
    #[arg(long)]
    export: Option<PathBuf>,
}

//  Helpers

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

//  File collection

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

//  Live progress hashing (pass 1)

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
        .template("{prefix:<50}  {bar:12.cyan/blue}  {msg}")
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

//  Silent hashing (pass 2)

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

//  Hash mode (parallel live bars)

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

//  Verify mode (parallel live bars)

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

    for line in reader.lines() {
        let line = line?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }

        let mut parts = line.split_whitespace();
        let expected = parts.next().unwrap().to_string();
        let filename = parts.next().unwrap().to_string();

        entries.push((expected, PathBuf::from(filename)));
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

//  Export mode (double-pass + CSV/TXT)

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

        let source_crc_res = compute_crc32_live(path, &mp);
        if let Ok(source_crc) = source_crc_res {
            let computed_crc_res = compute_crc32_silent(path);
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

//  Bench (placeholder)

fn bench_crc32(_size_str: &str, _threads: Option<usize>) {
    println!("Benchmark not implemented in this build.");
}

//  Main

fn main() -> io::Result<()> {
    let cli = Cli::parse();

    print_cpu_info();

    if let Some(size_str) = cli.bench {
        bench_crc32(&size_str, cli.threads);
        return Ok(());
    }

    if let Some(manifest) = cli.verify.as_ref() {
        return table_verify(manifest, cli.threads);
    }

    let files = collect_files(&cli);

    if let Some(export_path) = cli.export.as_ref() {
        return export_results(files, export_path, cli.threads);
    }

    table_hash(files, cli.threads)
}
