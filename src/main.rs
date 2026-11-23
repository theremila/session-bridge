mod antigravity;
mod claude;
mod codex;
mod proto;
mod time;

use std::borrow::Cow;
use std::error::Error;
use std::fmt::Write as _;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

type Result<T> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

struct Message<'a> {
    user: bool,
    time: Cow<'a, str>,
    text: Cow<'a, str>,
}

impl Message<'_> {
    fn borrowed(&self) -> Message<'_> {
        Message {
            user: self.user,
            time: Cow::Borrowed(&self.time),
            text: Cow::Borrowed(&self.text),
        }
    }
}

struct Session<'a> {
    id: String,
    time: String,
    cwd: String,
    title: Option<String>,
    messages: Vec<Message<'a>>,
}

fn uuid(s: &str) -> bool {
    s.len() == 36
        && s.bytes().enumerate().all(|(i, byte)| {
            if matches!(i, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_hexdigit()
            }
        })
}

fn collect(dir: &Path, keep: &impl Fn(&str) -> bool, out: &mut Vec<PathBuf>) -> io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        if kind.is_dir() {
            collect(&entry.path(), keep, out)?;
        } else if kind.is_file() && entry.file_name().to_str().is_some_and(keep) {
            out.push(entry.path());
        }
    }
    Ok(())
}

fn private_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn install(path: &Path, write: impl FnOnce(&Path, File) -> Result<()>) -> Result<bool> {
    if path.try_exists()? {
        return Ok(false);
    }
    fs::create_dir_all(path.parent().ok_or("destination missing parent")?)?;
    let temp = path.with_extension(format!("{}.tmp", std::process::id()));
    let file = private_file(&temp)?;
    let result = write(&temp, file).and_then(|()| match fs::hard_link(&temp, path) {
        Ok(()) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(e) => Err(e.into()),
    });
    fs::remove_file(&temp)?;
    result
}

fn sqlite(path: &Path, script: &str, read_only: bool) -> Result<String> {
    let mut cmd = Command::new("sqlite3");
    cmd.args(["-batch", "-bail", "-noheader", "-separator", "\t"]);
    if read_only {
        cmd.arg("-readonly");
    }
    let mut child = cmd
        .arg(path)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let written = child
        .stdin
        .take()
        .ok_or("sqlite3 stdin unavailable")?
        .write_all(script.as_bytes());
    let out = child.wait_with_output()?;
    if !out.status.success() {
        return Err(format!("sqlite3: {}", String::from_utf8_lossy(&out.stderr).trim()).into());
    }
    written?;
    Ok(String::from_utf8(out.stdout)?)
}

fn hex(data: impl AsRef<[u8]>) -> String {
    const DIGITS: &[u8] = b"0123456789abcdef";
    let data = data.as_ref();
    let mut out = String::with_capacity(data.len() * 2);
    for &byte in data {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 15) as usize] as char);
    }
    out
}

fn unhex(s: &str) -> Result<Vec<u8>> {
    let bytes = s.as_bytes();
    if !bytes.len().is_multiple_of(2) {
        return Err("invalid SQLite hex string".into());
    }
    bytes
        .as_chunks::<2>()
        .0
        .iter()
        .map(|part| Ok(u8::from_str_radix(std::str::from_utf8(part)?, 16)?))
        .collect()
}

fn sql_text(s: &str) -> String {
    format!("CAST(X'{}' AS TEXT)", hex(s))
}

fn json(s: &str) -> String {
    let mut out = String::from("\"");
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => write!(out, "\\u{:04x}", c as u32).unwrap(),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn unescape_tsv(value: &str) -> String {
    let mut chars = value.chars();
    let mut result = String::new();
    while let Some(c) = chars.next() {
        result.push(if c == '\\' {
            match chars.next() {
                Some('t') => '\t',
                Some('n') => '\n',
                Some('r') => '\r',
                _ => '\\',
            }
        } else {
            c
        });
    }
    result
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    Codex,
    Agy,
    Claude,
}

impl Format {
    fn parse(value: &std::ffi::OsStr) -> Result<Self> {
        match value.to_str() {
            Some("codex") => Ok(Self::Codex),
            Some("agy" | "antigravity") => Ok(Self::Agy),
            Some("claude" | "claude-code") => Ok(Self::Claude),
            _ => Err("expected codex, agy, or claude (see --help)".into()),
        }
    }
}

fn usage() {
    println!(
        "session-bridge --from FORMAT --to FORMAT [--dry-run] [--include-internal]\n\
        Formats: codex, agy (antigravity), claude (claude-code)\n\
        --include-internal: include Codex subagent and Guardian conversations\n\
        Store overrides: --codex-dir PATH, --agy-dir PATH, --claude-dir PATH"
    );
}

fn run() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let mut from = None;
    let mut to = None;
    let home = PathBuf::from(std::env::var_os("HOME").ok_or("HOME is unset")?);
    let mut codex_dir = std::env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".codex"));
    let mut agy_dir = home.join(".gemini/antigravity-cli");
    let mut claude_dir = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".claude"));
    let mut dry_run = false;
    let mut include_internal = false;
    while let Some(flag) = args.next() {
        match flag.to_str() {
            Some("--help" | "-h") => {
                usage();
                return Ok(());
            }
            Some("--version" | "-V") => {
                println!("session-bridge {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            Some("--from" | "-from") if from.is_none() => {
                from = Some(Format::parse(&args.next().ok_or("missing --from value")?)?);
            }
            Some("--to" | "-to") if to.is_none() => {
                to = Some(Format::parse(&args.next().ok_or("missing --to value")?)?);
            }
            Some("--codex-dir") => {
                codex_dir = args.next().ok_or("missing --codex-dir value")?.into()
            }
            Some("--agy-dir") => agy_dir = args.next().ok_or("missing --agy-dir value")?.into(),
            Some("--claude-dir") => {
                claude_dir = args.next().ok_or("missing --claude-dir value")?.into()
            }
            Some("--include-internal") => include_internal = true,
            Some("--dry-run") => dry_run = true,
            _ => return Err("unknown or repeated argument (see --help)".into()),
        }
    }
    let from = from.ok_or("missing --from (see --help)")?;
    let to = to.ok_or("missing --to (see --help)")?;
    if from == to {
        return Err("source and destination formats must differ".into());
    }

    if include_internal && from != Format::Codex {
        return Err("--include-internal requires --from codex".into());
    }

    let existing = match to {
        Format::Codex => codex::existing_ids(&codex_dir)?,
        Format::Claude => claude::existing_ids(&claude_dir)?,
        Format::Agy => Default::default(),
    };
    let mut convert = |session: &Session| {
        if existing.contains(&session.id) {
            return Ok(false);
        }
        match to {
            Format::Codex => codex::write(&codex_dir, session, dry_run),
            Format::Agy => antigravity::write(&agy_dir, session, dry_run),
            Format::Claude => claude::write(&claude_dir, &home, session, dry_run),
        }
    };
    if dry_run {
        println!("Dry run. Converted count is projected.");
    }
    match from {
        Format::Codex => codex::read_all(&codex_dir, include_internal, &mut convert),
        Format::Agy => antigravity::read_all(&agy_dir, &home, &mut convert),
        Format::Claude => claude::read_all(&claude_dir, &home, &mut convert),
    }
}

fn main() {
    if let Err(e) = run() {
        eprintln!("session-bridge: {e}");
        std::process::exit(1);
    }
}
