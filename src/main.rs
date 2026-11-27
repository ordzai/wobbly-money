//! Backs up Wobbly Life saves and sets Wobbly Island money and Space money
//! for every character in every save slot.

mod locate;
mod nrbf;

use std::ffi::OsString;
use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail, ensure};

use nrbf::Finding;

/// Console output. Write errors (closed pipe or console) are ignored so that
/// processing always completes; the log file records the full result.
macro_rules! say {
    ($($t:tt)*) => {{
        let _ = writeln!(std::io::stdout(), $($t)*);
    }};
}

/// As `say!`, without a trailing newline.
macro_rules! say_part {
    ($($t:tt)*) => {{
        let mut out = std::io::stdout();
        let _ = write!(out, $($t)*);
        let _ = out.flush();
    }};
}

/// Default amount. Below `i32::MAX` so that in-game earnings do not overflow.
const DEFAULT_MONEY: i32 = 999_999_999;

const HELP: &str = "\
wobbly-money: back up Wobbly Life saves and set money for every character

USAGE:
    wobbly-money [OPTIONS]

OPTIONS:
    --money <N>         Wobbly Island money (default 999999999, max 2147483647)
    --space-money <N>   Space money (default: same as --money)
    --save-dir <PATH>   Wobbly Life 'Save' directory (default: auto-detect)
    --backup-dir <PATH> Backup and log root (default: '<Save>/../SaveBackups')
    --log <PATH>        Log file (default: '<backup root>/<timestamp>.log')
    --dry-run           Report current values; modify no save files
    --backup-only       Create a backup; modify no save files
    -h, --help          Print this help

The log lists every parsed PlayerData file, the applied changes, and the
re-parsed modified data as verified before it replaces the original file.

Close Wobbly Life before running. Steam Cloud uploads the modified files on
the next game launch.
";

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Report,
    BackupOnly,
    Apply,
}

impl Mode {
    fn name(self) -> &'static str {
        match self {
            Self::Report => "dry run",
            Self::BackupOnly => "backup only",
            Self::Apply => "apply",
        }
    }
}

struct Args {
    island: i32,
    space: i32,
    save_dir: Option<PathBuf>,
    backup_dir: Option<PathBuf>,
    log: Option<PathBuf>,
    mode: Mode,
}

fn parse_money(flag: &str, v: Option<OsString>) -> Result<i32> {
    let v = v.with_context(|| format!("{flag} requires a value"))?;
    let s = v.to_str().with_context(|| format!("{flag}: value is not valid UTF-8"))?;
    let n: i32 = s
        .replace([',', '_'], "")
        .parse()
        .with_context(|| format!("{flag}: '{s}' is not an integer in 0..=2147483647"))?;
    ensure!(n >= 0, "{flag}: value must not be negative");
    Ok(n)
}

fn path_arg(flag: &str, v: Option<OsString>) -> Result<PathBuf> {
    v.map(PathBuf::from).with_context(|| format!("{flag} requires a value"))
}

fn parse_args() -> Result<Option<Args>> {
    let mut island = DEFAULT_MONEY;
    let mut space = None;
    let mut save_dir = None;
    let mut backup_dir = None;
    let mut log = None;
    let mut mode = Mode::Apply;
    let mut it = std::env::args_os().skip(1);
    while let Some(arg) = it.next() {
        match arg.to_str().unwrap_or("") {
            "-h" | "--help" => return Ok(None),
            "--money" => island = parse_money("--money", it.next())?,
            "--space-money" => space = Some(parse_money("--space-money", it.next())?),
            "--save-dir" => save_dir = Some(path_arg("--save-dir", it.next())?),
            "--backup-dir" => backup_dir = Some(path_arg("--backup-dir", it.next())?),
            "--log" => log = Some(path_arg("--log", it.next())?),
            "--dry-run" => mode = Mode::Report,
            "--backup-only" => mode = Mode::BackupOnly,
            _ => bail!("unknown argument {}\n\n{HELP}", arg.to_string_lossy()),
        }
    }
    Ok(Some(Args {
        island,
        space: space.unwrap_or(island),
        save_dir,
        backup_dir,
        log,
        mode,
    }))
}

fn main() {
    let result = parse_args().and_then(|args| {
        if let Some(a) = args {
            run(&a)
        } else {
            say_part!("{HELP}");
            Ok(())
        }
    });
    if let Err(e) = result {
        eprintln!("error: {e:#}");
        pause_if_double_clicked();
        std::process::exit(1);
    }
    pause_if_double_clicked();
}

/// Keeps the console window open when the program is started from Explorer.
fn pause_if_double_clicked() {
    #[cfg(windows)]
    if std::env::args_os().len() == 1 {
        say!("\nPress Enter to exit.");
        let mut s = String::new();
        let _ = std::io::stdin().read_line(&mut s);
    }
}

fn run(args: &Args) -> Result<()> {
    let save_dirs = match &args.save_dir {
        Some(d) => {
            ensure!(d.is_dir(), "{} is not a directory", d.display());
            vec![d.clone()]
        }
        None => locate::save_dirs(),
    };
    ensure!(
        !save_dirs.is_empty(),
        "no Wobbly Life save directory found; pass --save-dir <PATH> to the 'Save' directory"
    );
    let stamp = timestamp();
    for save_dir in &save_dirs {
        say!("Save directory: {}", save_dir.display());
        let backup_root = match &args.backup_dir {
            Some(b) => b.clone(),
            None => save_dir.parent().unwrap_or(save_dir).join("SaveBackups"),
        };
        let log_path = match &args.log {
            Some(p) => p.clone(),
            None => backup_root.join(format!("{stamp}.log")),
        };
        let mut log = open_log(&log_path)?;
        say!("Log: {}", log_path.display());
        writeln!(log, "wobbly-money {} ({})", env!("CARGO_PKG_VERSION"), args.mode.name())?;
        writeln!(log, "time (UTC): {stamp}")?;
        writeln!(log, "save directory: {}", save_dir.display())?;
        if args.mode == Mode::Apply {
            writeln!(log, "target: island money {}, Space money {}", args.island, args.space)?;
        }

        let accounts = locate::account_dirs(save_dir)
            .with_context(|| format!("reading {}", save_dir.display()))?;
        if accounts.is_empty() {
            say!("  no account directories found");
            writeln!(log, "no account directories found")?;
        }
        for account in &accounts {
            process_account(args, account, &backup_root, &stamp, &mut log)?;
        }
        log.flush().with_context(|| format!("writing {}", log_path.display()))?;
    }
    Ok(())
}

/// Opens `path` for appending, creating parent directories as needed.
fn open_log(path: &Path) -> Result<BufWriter<File>> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
    }
    let f = File::options()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("opening log {}", path.display()))?;
    Ok(BufWriter::new(f))
}

fn process_account(
    args: &Args,
    account: &Path,
    backup_root: &Path,
    stamp: &str,
    log: &mut impl Write,
) -> Result<()> {
    let id = account.file_name().unwrap_or_default();
    say!("Account {}", id.to_string_lossy());
    writeln!(log, "\n######## account {}", id.to_string_lossy())?;
    if args.mode != Mode::Report {
        let dest = backup_root.join(id).join(stamp);
        copy_dir(account, &dest).with_context(|| format!("backing up to {}", dest.display()))?;
        say!("  backup: {}", dest.display());
        writeln!(log, "backup: {}", dest.display())?;
    }
    let files = player_data_files(&account.join("GameSaves"))?;
    if files.is_empty() {
        say!("  no PlayerData files found");
        writeln!(log, "no PlayerData files found")?;
    }
    for path in &files {
        process_file(args, account, path, log)?;
    }
    Ok(())
}

fn process_file(args: &Args, account: &Path, path: &Path, log: &mut impl Write) -> Result<()> {
    let rel = path.strip_prefix(account.join("GameSaves")).unwrap_or(path);
    let buf = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    writeln!(log, "\n======== {} ({} bytes)", rel.display(), buf.len())?;
    let parsed = nrbf::parse(&buf).with_context(|| format!("parsing {}", path.display()))?;
    writeln!(log, "-------- original\n{}", parsed.listing)?;

    let (island, space) = summarize(&parsed.findings);
    let fmt = |v: Option<i64>| v.map_or_else(|| "-".to_owned(), |n| n.to_string());
    say_part!("  {}: island {}, space {}", rel.display(), fmt(island), fmt(space));

    let target_space = i64::from(args.space);
    let up_to_date = parsed.findings.iter().all(|f| match f {
        Finding::Island { value, .. } => *value == args.island,
        Finding::Space { value, .. } => *value == target_space,
    });
    if args.mode != Mode::Apply || up_to_date {
        say!();
        let reason = if args.mode == Mode::Apply { "values already at target" } else { args.mode.name() };
        writeln!(log, "-------- no changes ({reason})")?;
        return Ok(());
    }

    let (out, changes) = nrbf::apply(&buf, &parsed.findings, args.island, target_space);
    writeln!(log, "-------- changes")?;
    for c in &changes {
        write!(log, "{} at byte 0x{:x}: {} -> {}", c.what, c.offset, c.old, c.new)?;
        if let Some((old_len, new_len)) = c.string_len {
            write!(log, " (string length {old_len} -> {new_len} bytes)")?;
        }
        writeln!(log)?;
    }
    writeln!(log, "file size: {} -> {} bytes", buf.len(), out.len())?;

    let check = nrbf::parse(&out).context("modified data failed to parse")?;
    writeln!(log, "-------- modified (re-parsed before rename)\n{}", check.listing)?;
    let expected = (island.map(|_| i64::from(args.island)), space.map(|_| target_space));
    ensure!(
        check.findings.len() == parsed.findings.len() && summarize(&check.findings) == expected,
        "modified data verification failed for {}",
        path.display()
    );
    writeln!(log, "-------- verified; replacing {}", path.display())?;
    log.flush()?;
    write_replace(path, &out)?;
    say!(" -> island {}, space {}", fmt(expected.0), fmt(expected.1));
    Ok(())
}

/// First island and Space values, or `None` where absent.
fn summarize(findings: &[Finding<'_>]) -> (Option<i64>, Option<i64>) {
    let island = findings.iter().find_map(|f| match f {
        Finding::Island { value, .. } => Some(i64::from(*value)),
        Finding::Space { .. } => None,
    });
    let space = findings.iter().find_map(|f| match f {
        Finding::Space { value, .. } => Some(*value),
        Finding::Island { .. } => None,
    });
    (island, space)
}

/// `GameSaves/SaveSlot_*/PlayerData_*.sav`, sorted.
fn player_data_files(game_saves: &Path) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    if !game_saves.is_dir() {
        return Ok(out);
    }
    for slot in sorted_entries(game_saves)? {
        if !slot.is_dir() || !name_of(&slot).starts_with("SaveSlot_") {
            continue;
        }
        out.extend(sorted_entries(&slot)?.into_iter().filter(|p| {
            let n = name_of(p);
            p.is_file()
                && n.starts_with("PlayerData_")
                && Path::new(n).extension().is_some_and(|e| e.eq_ignore_ascii_case("sav"))
        }));
    }
    Ok(out)
}

fn name_of(p: &Path) -> &str {
    p.file_name().and_then(|n| n.to_str()).unwrap_or("")
}

fn sorted_entries(dir: &Path) -> Result<Vec<PathBuf>> {
    let mut v = std::fs::read_dir(dir)
        .with_context(|| format!("reading {}", dir.display()))?
        .map(|e| e.map(|e| e.path()))
        .collect::<std::io::Result<Vec<_>>>()?;
    v.sort();
    Ok(v)
}

fn copy_dir(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &to)?;
        } else {
            std::fs::copy(entry.path(), &to)
                .with_context(|| format!("copying {}", entry.path().display()))?;
        }
    }
    Ok(())
}

/// Writes to a sibling temporary file, then renames it over `path`.
fn write_replace(path: &Path, data: &[u8]) -> Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    std::fs::write(&tmp, data).with_context(|| format!("writing {}", tmp.display()))?;
    std::fs::rename(&tmp, path).with_context(|| format!("replacing {}", path.display()))
}

/// UTC time as `YYYY-MM-DD_HHMMSS`.
fn timestamp() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let rem = secs % 86_400;
    // Civil-from-days conversion (proleptic Gregorian calendar).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    format!(
        "{year:04}-{month:02}-{day:02}_{:02}{:02}{:02}",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}
