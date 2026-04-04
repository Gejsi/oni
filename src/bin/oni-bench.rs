//! Local benchmark harness for Oni.
//!
//! This binary is intentionally narrow for the first in-repo harness:
//! - generate one deterministic synthetic tree with mixed file sizes
//! - materialize repeatable sync scenarios
//! - run Oni and `rsync` against those scenarios
//! - verify the destination tree matches the source after each run
//! - emit machine-readable TSV results plus captured stdout/stderr
//!
//! The initial harness focuses on local runs only. Remote SSH benchmarks still
//! need helper-backed session execution in the main codebase.

use std::env;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use clap::{Parser, ValueEnum};
use walkdir::WalkDir;

const LARGE_FILE_SIZE: usize = 8 * 1024 * 1024;
const ZERO_FILE_SIZE: usize = 2 * 1024 * 1024;
const SMALL_FILE_COUNT: usize = 64;
const SMALL_FILE_SIZE: usize = 2048;
const PREPEND_SIZE: usize = 128;
const INSERT_SIZE: usize = 4 * 1024;
const OVERWRITE_SIZE: usize = 1024 * 1024;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let cli = Cli::parse();
    let repo_root = env::current_dir()?;
    let output_dir = create_output_dir(&cli.output_root)?;
    let oni_binary = ensure_oni_binary(&repo_root)?;

    write_environment(
        &output_dir.join("environment.tsv"),
        &capture_environment(&repo_root)?,
    )?;

    let tools = selected_tools(&cli);
    let scenarios = selected_scenarios(&cli);
    let mut results = File::create(output_dir.join("results.tsv"))?;
    writeln!(
        results,
        "dataset\tscenario\ttool\titeration\twall_seconds\texit_code\tsource_files\tsource_bytes\tstdout\tstderr\tcommand"
    )?;

    for iteration in 1..=cli.iterations {
        for scenario in &scenarios {
            for tool in &tools {
                let case_dir = output_dir.join("cases").join(format!(
                    "iter-{iteration:02}-{}-{}",
                    scenario.slug(),
                    tool.slug()
                ));
                let case = materialize_case(&case_dir, *scenario)?;
                let run = execute_tool(tool, &case, &oni_binary, cli.profile_oni_local)?;

                if run.exit_code != 0 {
                    return Err(format!(
                        "benchmark run failed for tool {} scenario {} iteration {} with exit code {}",
                        tool.slug(),
                        scenario.slug(),
                        iteration,
                        run.exit_code
                    )
                    .into());
                }

                verify_trees_match(&case.source, &case.destination)?;

                writeln!(
                    results,
                    "{}\t{}\t{}\t{}\t{:.6}\t{}\t{}\t{}\t{}\t{}\t{}",
                    DATASET_NAME,
                    scenario.slug(),
                    tool.slug(),
                    iteration,
                    run.wall_seconds,
                    run.exit_code,
                    case.stats.files,
                    case.stats.bytes,
                    run.stdout_path.display(),
                    run.stderr_path.display(),
                    run.command,
                )?;

                if !cli.keep_workdirs {
                    fs::remove_dir_all(&case_dir)?;
                }
            }
        }
    }

    println!("results\t{}", output_dir.join("results.tsv").display());
    println!(
        "environment\t{}",
        output_dir.join("environment.tsv").display()
    );

    Ok(())
}

const DATASET_NAME: &str = "mixed-tree-v1";

#[derive(Parser, Debug)]
#[command(about = "Run reproducible local benchmarks for Oni and rsync")]
struct Cli {
    /// Root directory for benchmark result runs.
    #[arg(long, default_value = "ignore/bench-results")]
    output_root: PathBuf,

    /// Number of repetitions per tool/scenario pair.
    #[arg(long, default_value_t = 1)]
    iterations: usize,

    /// Limit the run to one or more specific tools.
    #[arg(long = "tool", value_enum)]
    tools: Vec<Tool>,

    /// Limit the run to one or more specific scenarios.
    #[arg(long = "scenario", value_enum)]
    scenarios: Vec<Scenario>,

    /// Keep each prepared source/destination case directory after the run.
    #[arg(long)]
    keep_workdirs: bool,

    /// Ask Oni subprocesses to emit local timing breakdowns to stderr.
    #[arg(long)]
    profile_oni_local: bool,
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, ValueEnum)]
enum Tool {
    #[value(name = "oni-whole")]
    OniWhole,
    #[value(name = "oni-fixed")]
    OniFixed,
    #[value(name = "oni-cdc")]
    OniCdc,
    #[value(name = "rsync")]
    Rsync,
    #[value(name = "rsync-delta")]
    RsyncDelta,
}

impl Tool {
    fn slug(self) -> &'static str {
        match self {
            Self::OniWhole => "oni-whole",
            Self::OniFixed => "oni-fixed",
            Self::OniCdc => "oni-cdc",
            Self::Rsync => "rsync",
            Self::RsyncDelta => "rsync-delta",
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, ValueEnum)]
enum Scenario {
    #[value(name = "cold-copy")]
    ColdCopy,
    #[value(name = "warm-noop")]
    WarmNoop,
    #[value(name = "prepend-128b")]
    Prepend128B,
    #[value(name = "middle-insert-4k")]
    MiddleInsert4K,
    #[value(name = "equal-overwrite-1m")]
    EqualOverwrite1M,
}

impl Scenario {
    fn slug(self) -> &'static str {
        match self {
            Self::ColdCopy => "cold-copy",
            Self::WarmNoop => "warm-noop",
            Self::Prepend128B => "prepend-128b",
            Self::MiddleInsert4K => "middle-insert-4k",
            Self::EqualOverwrite1M => "equal-overwrite-1m",
        }
    }
}

#[derive(Debug)]
struct Case {
    source: PathBuf,
    destination: PathBuf,
    stats: TreeStats,
}

#[derive(Debug, Copy, Clone)]
struct TreeStats {
    files: usize,
    bytes: u64,
}

#[derive(Debug)]
struct RunResult {
    command: String,
    exit_code: i32,
    wall_seconds: f64,
    stdout_path: PathBuf,
    stderr_path: PathBuf,
}

#[derive(Debug)]
struct EnvironmentRecord {
    key: &'static str,
    value: String,
}

fn selected_tools(cli: &Cli) -> Vec<Tool> {
    if cli.tools.is_empty() {
        vec![
            Tool::OniWhole,
            Tool::OniFixed,
            Tool::OniCdc,
            Tool::Rsync,
            Tool::RsyncDelta,
        ]
    } else {
        cli.tools.clone()
    }
}

fn selected_scenarios(cli: &Cli) -> Vec<Scenario> {
    if cli.scenarios.is_empty() {
        vec![
            Scenario::ColdCopy,
            Scenario::WarmNoop,
            Scenario::Prepend128B,
            Scenario::MiddleInsert4K,
            Scenario::EqualOverwrite1M,
        ]
    } else {
        cli.scenarios.clone()
    }
}

fn create_output_dir(root: &Path) -> io::Result<PathBuf> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let output_dir = root.join(format!("run-{timestamp}"));
    fs::create_dir_all(output_dir.join("cases"))?;
    Ok(output_dir)
}

fn ensure_oni_binary(repo_root: &Path) -> io::Result<PathBuf> {
    // The user wants Oni measured in debug mode even when the benchmark
    // harness itself is launched another way, so always target `debug/oni`.
    let oni_binary = repo_root.join("target/debug/oni");

    if oni_binary.exists() {
        return Ok(oni_binary);
    }

    let mut build = Command::new("cargo");
    build.arg("build").arg("--bin").arg("oni");
    build.current_dir(repo_root);

    let status = build.status()?;
    if !status.success() {
        return Err(io::Error::other("failed to build the oni benchmark target"));
    }

    Ok(oni_binary)
}

fn materialize_case(case_dir: &Path, scenario: Scenario) -> io::Result<Case> {
    if case_dir.exists() {
        fs::remove_dir_all(case_dir)?;
    }
    fs::create_dir_all(case_dir)?;

    let source = case_dir.join("source");
    let destination = case_dir.join("destination");
    create_dataset(&source)?;

    match scenario {
        Scenario::ColdCopy => {}
        Scenario::WarmNoop => {
            copy_tree(&source, &destination)?;
        }
        Scenario::Prepend128B => {
            copy_tree(&source, &destination)?;
            prepend_bytes(&source.join("large/blob.bin"), PREPEND_SIZE)?;
        }
        Scenario::MiddleInsert4K => {
            copy_tree(&source, &destination)?;
            insert_bytes(
                &source.join("large/blob.bin"),
                LARGE_FILE_SIZE / 2,
                INSERT_SIZE,
            )?;
        }
        Scenario::EqualOverwrite1M => {
            copy_tree(&source, &destination)?;
            overwrite_bytes(
                &source.join("large/blob.bin"),
                LARGE_FILE_SIZE / 2,
                OVERWRITE_SIZE,
            )?;
        }
    }

    Ok(Case {
        stats: scan_tree(&source)?,
        source,
        destination,
    })
}

fn create_dataset(root: &Path) -> io::Result<()> {
    fs::create_dir_all(root.join("large"))?;
    fs::create_dir_all(root.join("zeroes"))?;
    fs::create_dir_all(root.join("docs"))?;
    fs::create_dir_all(root.join("small"))?;

    write_patterned_file(&root.join("large/blob.bin"), LARGE_FILE_SIZE)?;
    write_zero_runs_file(&root.join("zeroes/vm-like.bin"), ZERO_FILE_SIZE)?;
    write_text_file(
        &root.join("docs/readme.txt"),
        "Oni benchmark dataset.\nThe contents are deterministic.\n",
        512,
    )?;

    for index in 0..SMALL_FILE_COUNT {
        let path = root.join("small").join(format!("file-{index:03}.txt"));
        write_text_file(
            &path,
            &format!("small benchmark file {index}\n"),
            SMALL_FILE_SIZE / 32,
        )?;
    }

    Ok(())
}

fn write_patterned_file(path: &Path, len: usize) -> io::Result<()> {
    let mut file = File::create(path)?;
    let mut buffer = [0_u8; 8 * 1024];
    let mut written = 0usize;

    while written < len {
        let chunk_len = (len - written).min(buffer.len());
        for (offset, byte) in buffer[..chunk_len].iter_mut().enumerate() {
            let index = written + offset;
            *byte = ((index * 31 + index / 97) % 251) as u8;
        }
        file.write_all(&buffer[..chunk_len])?;
        written += chunk_len;
    }

    file.sync_all()?;
    Ok(())
}

fn write_zero_runs_file(path: &Path, len: usize) -> io::Result<()> {
    let mut file = File::create(path)?;
    let mut remaining = len;
    let pattern = [0_u8; 8 * 1024];
    let mut alternating = [0_u8; 8 * 1024];
    for (index, byte) in alternating.iter_mut().enumerate() {
        *byte = if index % 64 < 48 { 0 } else { 0xFF };
    }

    while remaining > 0 {
        let chunk_len = remaining.min(pattern.len());
        if (remaining / pattern.len()) % 2 == 0 {
            file.write_all(&pattern[..chunk_len])?;
        } else {
            file.write_all(&alternating[..chunk_len])?;
        }
        remaining -= chunk_len;
    }

    file.sync_all()?;
    Ok(())
}

fn write_text_file(path: &Path, line: &str, repeats: usize) -> io::Result<()> {
    let mut file = File::create(path)?;
    for _ in 0..repeats {
        file.write_all(line.as_bytes())?;
    }
    file.sync_all()?;
    Ok(())
}

fn copy_tree(source: &Path, destination: &Path) -> io::Result<()> {
    for entry in WalkDir::new(source) {
        let entry = entry?;
        let path = entry.path();
        let relative = path
            .strip_prefix(source)
            .map_err(|error| io::Error::other(error.to_string()))?;
        let target = destination.join(relative);

        if entry.file_type().is_dir() {
            fs::create_dir_all(&target)?;
            continue;
        }

        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }

        fs::copy(path, &target)?;

        let metadata = fs::metadata(path)?;
        fs::set_permissions(&target, metadata.permissions())?;
        if let Ok(modified) = metadata.modified() {
            File::options()
                .write(true)
                .open(&target)?
                .set_modified(modified)?;
        }
    }

    Ok(())
}

fn prepend_bytes(path: &Path, len: usize) -> io::Result<()> {
    let original = fs::read(path)?;
    let mut updated = Vec::with_capacity(original.len() + len);
    updated.extend((0..len).map(|index| b'A' + (index % 26) as u8));
    updated.extend_from_slice(&original);
    fs::write(path, updated)?;
    bump_modified_time(path)
}

fn insert_bytes(path: &Path, offset: usize, len: usize) -> io::Result<()> {
    let original = fs::read(path)?;
    let mut updated = Vec::with_capacity(original.len() + len);
    let split = offset.min(original.len());
    updated.extend_from_slice(&original[..split]);
    updated.extend((0..len).map(|index| b'k' + (index % 13) as u8));
    updated.extend_from_slice(&original[split..]);
    fs::write(path, updated)?;
    bump_modified_time(path)
}

fn overwrite_bytes(path: &Path, offset: usize, len: usize) -> io::Result<()> {
    let mut data = fs::read(path)?;
    let start = offset.min(data.len());
    let end = (start + len).min(data.len());

    for (index, byte) in data[start..end].iter_mut().enumerate() {
        *byte = b'Z' - (index % 23) as u8;
    }

    fs::write(path, data)?;
    bump_modified_time(path)
}

fn bump_modified_time(path: &Path) -> io::Result<()> {
    // Same-size overwrite benchmarks are specifically trying to catch quick-
    // check mistakes. Force the mutated source mtime past the preseeded
    // destination mtime so tools that rely on size+mtime do real work here.
    let bumped = SystemTime::now() + Duration::from_secs(2);
    File::options()
        .write(true)
        .open(path)?
        .set_modified(bumped)?;
    Ok(())
}

fn execute_tool(
    tool: &Tool,
    case: &Case,
    oni_binary: &Path,
    profile_oni_local: bool,
) -> io::Result<RunResult> {
    let run_dir = case
        .source
        .parent()
        .ok_or_else(|| io::Error::other("benchmark case source directory is missing a parent"))?;
    let stdout_path = run_dir.join(format!("{}.stdout.txt", tool.slug()));
    let stderr_path = run_dir.join(format!("{}.stderr.txt", tool.slug()));
    let stdout = File::create(&stdout_path)?;
    let stderr = File::create(&stderr_path)?;

    let (mut command_text, mut command) =
        build_command(tool, oni_binary, &case.source, &case.destination);
    if profile_oni_local && matches!(tool, Tool::OniWhole | Tool::OniFixed | Tool::OniCdc) {
        command.env("ONI_PROFILE_LOCAL", "1");
        command_text = format!("ONI_PROFILE_LOCAL=1 {command_text}");
    }
    let start = Instant::now();
    let status = command
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .status()?;
    let wall_seconds = start.elapsed().as_secs_f64();

    Ok(RunResult {
        command: command_text,
        exit_code: status.code().unwrap_or(-1),
        wall_seconds,
        stdout_path,
        stderr_path,
    })
}

fn build_command(
    tool: &Tool,
    oni_binary: &Path,
    source: &Path,
    destination: &Path,
) -> (String, Command) {
    match tool {
        Tool::OniWhole => {
            let mut command = Command::new(oni_binary);
            command
                .arg("--delete")
                .arg("--strategy")
                .arg("whole")
                .arg(source)
                .arg(destination);
            (
                format!(
                    "{} --delete --strategy whole {} {}",
                    oni_binary.display(),
                    source.display(),
                    destination.display()
                ),
                command,
            )
        }
        Tool::OniFixed => {
            let mut command = Command::new(oni_binary);
            command
                .arg("--delete")
                .arg("--strategy")
                .arg("fixed")
                .arg(source)
                .arg(destination);
            (
                format!(
                    "{} --delete --strategy fixed {} {}",
                    oni_binary.display(),
                    source.display(),
                    destination.display()
                ),
                command,
            )
        }
        Tool::OniCdc => {
            let mut command = Command::new(oni_binary);
            command
                .arg("--delete")
                .arg("--strategy")
                .arg("cdc")
                .arg("--chunker")
                .arg("fastcdc")
                .arg(source)
                .arg(destination);
            (
                format!(
                    "{} --delete --strategy cdc --chunker fastcdc {} {}",
                    oni_binary.display(),
                    source.display(),
                    destination.display()
                ),
                command,
            )
        }
        Tool::Rsync => {
            // This harness intentionally measures the installed local rsync
            // default. For local->local runs that means whole-file mode unless
            // `--no-whole-file` is added explicitly.
            let mut command = Command::new("rsync");
            let source = path_with_trailing_separator(source);
            let destination = path_with_trailing_separator(destination);
            command
                .arg("-a")
                .arg("--delete")
                .arg(&source)
                .arg(&destination);
            (
                format!("rsync -a --delete {} {}", source, destination),
                command,
            )
        }
        Tool::RsyncDelta => {
            let mut command = Command::new("rsync");
            let source = path_with_trailing_separator(source);
            let destination = path_with_trailing_separator(destination);
            command
                .arg("-a")
                .arg("--delete")
                .arg("--no-whole-file")
                .arg(&source)
                .arg(&destination);
            (
                format!(
                    "rsync -a --delete --no-whole-file {} {}",
                    source, destination
                ),
                command,
            )
        }
    }
}

fn path_with_trailing_separator(path: &Path) -> String {
    let mut rendered = path.display().to_string();
    if !rendered.ends_with(std::path::MAIN_SEPARATOR) {
        rendered.push(std::path::MAIN_SEPARATOR);
    }
    rendered
}

fn verify_trees_match(source: &Path, destination: &Path) -> io::Result<()> {
    let source_files = indexed_files(source)?;
    let destination_files = indexed_files(destination)?;

    if source_files.len() != destination_files.len() {
        return Err(io::Error::other(format!(
            "destination file count mismatch: source={} destination={}",
            source_files.len(),
            destination_files.len()
        )));
    }

    for (relative, source_path) in source_files {
        let destination_path = destination_files.get(&relative).ok_or_else(|| {
            io::Error::other(format!(
                "destination is missing expected file {}",
                relative.display()
            ))
        })?;

        if !files_match(&source_path, destination_path)? {
            return Err(io::Error::other(format!(
                "destination file does not match source for {}",
                relative.display()
            )));
        }
    }

    Ok(())
}

fn indexed_files(root: &Path) -> io::Result<std::collections::BTreeMap<PathBuf, PathBuf>> {
    let mut files = std::collections::BTreeMap::new();

    if !root.exists() {
        return Ok(files);
    }

    for entry in WalkDir::new(root) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }

        let relative = entry
            .path()
            .strip_prefix(root)
            .map_err(|error| io::Error::other(error.to_string()))?
            .to_path_buf();
        files.insert(relative, entry.path().to_path_buf());
    }

    Ok(files)
}

fn files_match(left: &Path, right: &Path) -> io::Result<bool> {
    let left_metadata = fs::metadata(left)?;
    let right_metadata = fs::metadata(right)?;
    if left_metadata.len() != right_metadata.len() {
        return Ok(false);
    }

    let mut left_file = File::open(left)?;
    let mut right_file = File::open(right)?;
    let mut left_buffer = [0_u8; 8 * 1024];
    let mut right_buffer = [0_u8; 8 * 1024];

    loop {
        let left_read = left_file.read(&mut left_buffer)?;
        let right_read = right_file.read(&mut right_buffer)?;

        if left_read != right_read {
            return Ok(false);
        }

        if left_read == 0 {
            return Ok(true);
        }

        if left_buffer[..left_read] != right_buffer[..right_read] {
            return Ok(false);
        }
    }
}

fn scan_tree(root: &Path) -> io::Result<TreeStats> {
    let mut files = 0usize;
    let mut bytes = 0u64;

    for entry in WalkDir::new(root) {
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }

        files += 1;
        bytes += entry.metadata()?.len();
    }

    Ok(TreeStats { files, bytes })
}

fn capture_environment(repo_root: &Path) -> io::Result<Vec<EnvironmentRecord>> {
    Ok(vec![
        EnvironmentRecord {
            key: "date_unix_secs",
            value: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs()
                .to_string(),
        },
        EnvironmentRecord {
            key: "dataset",
            value: DATASET_NAME.to_string(),
        },
        EnvironmentRecord {
            key: "oni_build_profile",
            value: "debug".to_string(),
        },
        EnvironmentRecord {
            key: "git_branch",
            value: run_capture(repo_root, "git", ["branch", "--show-current"])?,
        },
        EnvironmentRecord {
            key: "git_commit",
            value: run_capture(repo_root, "git", ["rev-parse", "HEAD"])?,
        },
        EnvironmentRecord {
            key: "cpu_model",
            value: cpu_model().unwrap_or_else(|| "unknown".to_string()),
        },
        EnvironmentRecord {
            key: "core_count",
            value: std::thread::available_parallelism()
                .map(|value| value.get().to_string())
                .unwrap_or_else(|_| "unknown".to_string()),
        },
        EnvironmentRecord {
            key: "ram_kib",
            value: mem_total_kib().unwrap_or_else(|| "unknown".to_string()),
        },
        EnvironmentRecord {
            key: "kernel",
            value: run_capture(repo_root, "uname", ["-srmo"])
                .unwrap_or_else(|_| format!("{} unknown", env::consts::OS)),
        },
        EnvironmentRecord {
            key: "rsync_version",
            value: first_line(
                &run_capture(repo_root, "rsync", ["--version"])
                    .unwrap_or_else(|_| "unknown".to_string()),
            )
            .to_string(),
        },
        EnvironmentRecord {
            key: "ssh_version",
            value: capture_ssh_version(repo_root).unwrap_or_else(|_| "unknown".to_string()),
        },
    ])
}

fn write_environment(path: &Path, records: &[EnvironmentRecord]) -> io::Result<()> {
    let mut file = File::create(path)?;
    writeln!(file, "key\tvalue")?;
    for record in records {
        writeln!(file, "{}\t{}", record.key, record.value)?;
    }
    Ok(())
}

fn run_capture<I, S>(workdir: &Path, program: &str, args: I) -> io::Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let output = Command::new(program)
        .args(args)
        .current_dir(workdir)
        .output()?;

    if !output.status.success() {
        return Err(io::Error::other(format!("command failed: {}", program)));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn capture_ssh_version(workdir: &Path) -> io::Result<String> {
    let output = Command::new("ssh")
        .arg("-V")
        .current_dir(workdir)
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other("ssh -V failed"));
    }

    Ok(first_line(&String::from_utf8_lossy(&output.stderr)).to_string())
}

fn cpu_model() -> Option<String> {
    let contents = fs::read_to_string("/proc/cpuinfo").ok()?;
    contents
        .lines()
        .find_map(|line| line.strip_prefix("model name\t: "))
        .map(str::to_string)
}

fn mem_total_kib() -> Option<String> {
    let contents = fs::read_to_string("/proc/meminfo").ok()?;
    contents
        .lines()
        .find_map(|line| line.strip_prefix("MemTotal:"))
        .map(|value| value.trim().trim_end_matches(" kB").to_string())
}

fn first_line(text: &str) -> &str {
    text.lines().next().unwrap_or(text)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{
        build_command, copy_tree, create_dataset, files_match, insert_bytes, materialize_case,
        overwrite_bytes, path_with_trailing_separator, prepend_bytes, scan_tree, Scenario, Tool,
        LARGE_FILE_SIZE,
    };

    fn temp_path(label: &str) -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        std::env::temp_dir().join(format!("oni-bench-{label}-{unique}"))
    }

    #[test]
    fn copy_tree_preserves_file_metadata_for_noop_cases() {
        let root = temp_path("copy-tree");
        let source = root.join("source");
        let destination = root.join("destination");
        create_dataset(&source).unwrap();

        copy_tree(&source, &destination).unwrap();

        let source_file = source.join("large/blob.bin");
        let destination_file = destination.join("large/blob.bin");
        assert!(files_match(&source_file, &destination_file).unwrap());
        assert_eq!(
            scan_tree(&source).unwrap().bytes,
            scan_tree(&destination).unwrap().bytes
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn scenarios_apply_the_expected_size_changes() {
        let root = temp_path("scenario-mutations");
        let dataset = root.join("dataset");
        create_dataset(&dataset).unwrap();
        let blob = dataset.join("large/blob.bin");

        prepend_bytes(&blob, 128).unwrap();
        assert_eq!(
            fs::metadata(&blob).unwrap().len(),
            (LARGE_FILE_SIZE + 128) as u64
        );

        create_dataset(&dataset).unwrap();
        insert_bytes(&blob, LARGE_FILE_SIZE / 2, 4096).unwrap();
        assert_eq!(
            fs::metadata(&blob).unwrap().len(),
            (LARGE_FILE_SIZE + 4096) as u64
        );

        create_dataset(&dataset).unwrap();
        overwrite_bytes(&blob, LARGE_FILE_SIZE / 2, 1024 * 1024).unwrap();
        assert_eq!(fs::metadata(&blob).unwrap().len(), LARGE_FILE_SIZE as u64);

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn command_builder_uses_the_expected_flags() {
        let oni = PathBuf::from("/tmp/oni");
        let source = PathBuf::from("/tmp/source");
        let destination = PathBuf::from("/tmp/destination");

        let (oni_cdc, _) = build_command(&Tool::OniCdc, &oni, &source, &destination);
        let (rsync, _) = build_command(&Tool::Rsync, &oni, &source, &destination);
        let (rsync_delta, _) = build_command(&Tool::RsyncDelta, &oni, &source, &destination);

        assert!(oni_cdc.contains("--strategy cdc"));
        assert!(oni_cdc.contains("--chunker fastcdc"));
        assert_eq!(rsync, "rsync -a --delete /tmp/source/ /tmp/destination/");
        assert_eq!(
            rsync_delta,
            "rsync -a --delete --no-whole-file /tmp/source/ /tmp/destination/"
        );
        assert_eq!(
            path_with_trailing_separator(Path::new("/tmp/source")),
            "/tmp/source/"
        );
    }

    #[test]
    fn materialized_equal_overwrite_case_keeps_the_dataset_size_stable() {
        let root = temp_path("materialize-case");
        let case = materialize_case(&root, Scenario::EqualOverwrite1M).unwrap();

        assert_eq!(case.stats.files, scan_tree(&case.source).unwrap().files);
        assert!(case.destination.exists());
        assert_eq!(
            scan_tree(&case.source).unwrap().bytes,
            scan_tree(&case.destination).unwrap().bytes
        );

        fs::remove_dir_all(root).unwrap();
    }
}
