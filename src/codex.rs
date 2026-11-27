use super::*;
use std::collections::{BTreeMap, HashMap, HashSet};

struct Segment {
    id: String,
    time: String,
    cwd: String,
    base: String,
    cutoff: u64,
    internal: bool,
    messages: Vec<(u64, Message<'static>)>,
}

fn read(path: &Path) -> Result<Segment> {
    let mut decompress = None;
    let mut command = Command::new("jq");
    command.args(["-Rn", "-r", include_str!("codex.jq")]);
    if path.extension().is_some_and(|ext| ext == "zst") {
        let mut child = Command::new("zstd")
            .args(["-dcq", "--"])
            .arg(path)
            .stdout(Stdio::piped())
            .spawn()?;
        command.stdin(child.stdout.take().ok_or("zstd stdout unavailable")?);
        decompress = Some(child);
    } else {
        command.stdin(File::open(path)?);
    }
    let output = command.output();
    if let Some(mut child) = decompress
        && !child.wait()?.success()
    {
        return Err("zstd decompression failed".into());
    }
    let output = output?;
    if !output.status.success() {
        return Err(format!("jq: {}", String::from_utf8_lossy(&output.stderr).trim()).into());
    }
    let stdout = String::from_utf8(output.stdout)?;
    let mut lines = stdout.lines();
    let mut header = lines.next().ok_or("missing session metadata")?.split('\t');
    let [
        Some(_),
        Some("meta"),
        Some(id),
        Some(time),
        Some(cwd),
        Some(base),
        Some(cutoff),
        Some(internal),
    ] = std::array::from_fn(|_| header.next())
    else {
        return Err("invalid session metadata".into());
    };
    if !uuid(id) {
        return Err("invalid session UUID".into());
    }
    let mut messages = Vec::new();
    for line in lines {
        let mut parts = line.splitn(4, '\t');
        let offset = parts.next().ok_or("missing byte offset")?.parse()?;
        let role = parts.next().ok_or("missing role")?;
        if role == "meta" {
            continue;
        }
        let time = parts.next().ok_or("missing message timestamp")?.to_owned();
        let text = unescape_tsv(parts.next().ok_or("missing message text")?);
        messages.push((
            offset,
            Message {
                user: role == "user",
                time: time.into(),
                text: text.into(),
            },
        ));
    }
    Ok(Segment {
        id: id.to_string(),
        time: time.to_string(),
        cwd: unescape_tsv(cwd),
        base: base.to_string(),
        cutoff: cutoff.parse()?,
        internal: internal.parse()?,
        messages,
    })
}

fn append_history<'a>(
    key: &'a str,
    cutoff: u64,
    segments: &'a HashMap<String, Segment>,
    seen: &mut HashSet<&'a str>,
    messages: &mut Vec<Message<'a>>,
) -> Result<()> {
    if !seen.insert(key) {
        return Err("cycle in rollout history".into());
    }
    let segment = segments
        .get(key)
        .ok_or_else(|| format!("missing rollout segment {key}"))?;
    if !segment.base.is_empty() {
        append_history(&segment.base, segment.cutoff, segments, seen, messages)?;
    }
    messages.extend(
        segment
            .messages
            .iter()
            .take_while(|(end, _)| *end <= cutoff)
            .map(|(_, message)| message.borrowed()),
    );
    Ok(())
}

fn rollout_paths(root: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for dir in ["sessions", "archived_sessions"] {
        let path = root.join(dir);
        if path.try_exists()? {
            collect(
                &path,
                &|name| {
                    name.starts_with("rollout-")
                        && (name.ends_with(".jsonl") || name.ends_with(".jsonl.zst"))
                },
                &mut paths,
            )?;
        }
    }
    Ok(paths)
}

pub fn existing_ids(root: &Path) -> Result<HashSet<String>> {
    Ok(rollout_paths(root)?
        .iter()
        .filter_map(|path| {
            let name = path.file_name()?.to_str()?.strip_prefix("rollout-")?;
            let id = name.get(20..56)?;
            uuid(id).then(|| id.to_owned())
        })
        .collect())
}

// The index carries user-assigned names. The newest state database supplements it
// with native fallback titles that older versions did not write to the index.
#[derive(Default)]
struct ThreadIndex {
    titles: HashMap<String, String>,
    internal: HashSet<String>,
}

fn thread_index(root: &Path, classify: bool) -> Result<ThreadIndex> {
    let state = fs::read_dir(root)?
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let name = entry.file_name();
            let version = name
                .to_str()?
                .strip_prefix("state_")?
                .strip_suffix(".sqlite")?
                .parse::<u32>()
                .ok()?;
            Some((version, entry.path()))
        })
        .max_by_key(|(version, _)| *version);
    let mut index = ThreadIndex::default();
    if let Some((_, path)) = state {
        let columns = sqlite(
            &path,
            "SELECT name FROM pragma_table_info('threads');\n",
            true,
        )?;
        if classify {
            let mut checks = Vec::with_capacity(2);
            if columns.lines().any(|column| column == "thread_source") {
                checks.push("thread_source IN ('subagent','guardian_review')");
            }
            if columns.lines().any(|column| column == "source") {
                checks.push("source IN ('subagent','guardian_review') OR CASE WHEN json_valid(source) THEN json_type(source, '$.subagent') IS NOT NULL ELSE 0 END");
            }
            if !checks.is_empty() {
                let sql = format!("SELECT id FROM threads WHERE {};\n", checks.join(" OR "));
                let result = sqlite(&path, &sql, true)?;
                index
                    .internal
                    .extend(result.lines().filter(|id| uuid(id)).map(str::to_owned));
            }
        }
        if columns.lines().any(|column| column == "title") {
            let title = if columns.lines().any(|column| column == "name") {
                "coalesce(nullif(trim(name), ''), title)"
            } else {
                "title"
            };
            let result = sqlite(
                &path,
                &format!("SELECT id, hex({title}) FROM threads WHERE length(trim({title})) > 0;\n"),
                true,
            )?;
            for (id, value) in result.lines().filter_map(|row| row.split_once('\t')) {
                index
                    .titles
                    .insert(id.to_owned(), String::from_utf8(unhex(value)?)?);
            }
        }
    }
    let path = root.join("session_index.jsonl");
    if path.try_exists()? {
        let output = Command::new("jq").args(["-Rr",
            r#"try fromjson catch empty | select((.id | type) == "string" and (.thread_name | type) == "string") | [.id,.thread_name] | @tsv"#])
            .stdin(File::open(path)?).output()?;
        if !output.status.success() {
            return Err("could not read Codex session index".into());
        }
        for (id, value) in std::str::from_utf8(&output.stdout)?
            .lines()
            .filter_map(|row| row.split_once('\t'))
        {
            if !value.trim().is_empty() {
                index.titles.insert(id.to_owned(), unescape_tsv(value));
            }
        }
    }
    Ok(index)
}

pub fn read_all(
    root: &Path,
    include_internal: bool,
    filter: &NameFilter,
    mut convert: impl FnMut(&Session<'_>) -> Result<bool>,
) -> Result<()> {
    let mut paths = rollout_paths(root)?;
    if paths.is_empty() {
        return Err(format!("no Codex rollouts in {}", root.display()).into());
    }
    paths.sort();
    let mut index = thread_index(root, !include_internal)?;
    let mut segments = HashMap::new();
    let mut latest = BTreeMap::<String, (String, String)>::new();
    let mut failed = 0;
    let results = std::thread::scope(|scope| {
        let workers = std::thread::available_parallelism()
            .map_or(1, usize::from)
            .min(4);
        let chunk_size = paths.len().div_ceil(workers);
        let handles: Vec<_> = paths
            .chunks(chunk_size)
            .map(|chunk| {
                scope.spawn(move || chunk.iter().map(|path| read(path)).collect::<Vec<_>>())
            })
            .collect();
        handles
            .into_iter()
            .flat_map(|handle| handle.join().expect("rollout reader panicked"))
            .collect::<Vec<_>>()
    });
    for (path, result) in paths.iter().zip(results) {
        match result {
            Ok(segment) => {
                let name = path
                    .file_name()
                    .and_then(|s| s.to_str())
                    .ok_or("invalid rollout filename")?;
                let stem = name.split('.').next().ok_or("missing rollout stem")?;
                let key = stem
                    .get(stem.len().saturating_sub(36)..)
                    .ok_or("invalid rollout identifier")?
                    .to_owned();
                if !uuid(&key) {
                    return Err(format!("invalid rollout filename: {name}").into());
                }
                if !include_internal && segment.internal {
                    index.internal.insert(segment.id.clone());
                }
                let entry = latest.entry(segment.id.clone()).or_default();
                if segment.time >= entry.0 {
                    *entry = (segment.time.clone(), key.clone());
                }
                segments.insert(key, segment);
            }
            Err(error) => {
                eprintln!("{}: {error}", path.display());
                failed += 1;
            }
        }
    }
    let mut written = 0;
    let mut existing = 0;
    let mut empty = 0;
    let mut internal = 0;
    let mut filtered = 0;
    for (id, (_, key)) in latest {
        let segment = &segments[&key];
        if index.internal.contains(&id) {
            internal += 1;
            continue;
        }
        if filter.is_active()
            && index
                .titles
                .get(&id)
                .and_then(|title| usable_title(title))
                .is_some_and(|title| !filter.matches_title(title))
        {
            filtered += 1;
            continue;
        }
        let mut messages = Vec::new();
        if let Err(e) = append_history(
            &key,
            u64::MAX,
            &segments,
            &mut HashSet::new(),
            &mut messages,
        ) {
            eprintln!("{id}: {e}");
            failed += 1;
            continue;
        }
        if messages.is_empty() {
            empty += 1;
            continue;
        }
        let session = Session {
            title: index.titles.remove(&id),
            id,
            time: messages[0].time.to_string(),
            cwd: segment.cwd.clone(),
            messages,
        };
        if !filter.matches_session(&session) {
            filtered += 1;
            continue;
        }
        match convert(&session) {
            Ok(true) => written += 1,
            Ok(false) => existing += 1,
            Err(e) => {
                eprintln!("{}: {e}", session.id);
                failed += 1;
            }
        }
    }
    if filter.is_active() {
        println!(
            "{} rollout files: {written} converted, {existing} already present, {empty} without text, {internal} internal skipped, {filtered} filtered, {failed} failed.",
            paths.len()
        );
    } else {
        println!(
            "{} rollout files: {written} converted, {existing} already present, {empty} without text, {internal} internal skipped, {failed} failed.",
            paths.len()
        );
    }
    if failed > 0 {
        return Err("some rollouts could not be imported".into());
    }
    if filter.is_active() && written + existing == 0 {
        return Err("no sessions matched --name".into());
    }
    Ok(())
}

pub fn write(root: &Path, session: &Session<'_>, dry_run: bool) -> Result<bool> {
    let time = session
        .time
        .get(..19)
        .filter(|s| s.bytes().all(|b| b.is_ascii_digit() || b"-T:".contains(&b)))
        .ok_or("invalid session timestamp")?;
    let date = time.get(..10).ok_or("missing date")?;
    let path = root
        .join("sessions")
        .join(date.replace('-', "/"))
        .join(format!("rollout-{time}-{}.jsonl", session.id));
    if dry_run {
        return Ok(!path.try_exists()?);
    }
    install(&path, |_, mut file| {
        writeln!(
            file,
            r#"{{"timestamp":{},"type":"session_meta","payload":{{"id":{},"timestamp":{},"cwd":{},"originator":"codex_cli_rs","cli_version":"0.0.0","source":"cli","model_provider":"openai"}}}}"#,
            json(&session.time),
            json(&session.id),
            json(&session.time),
            json(&session.cwd)
        )?;
        for message in &session.messages {
            let time = json(&message.time);
            let text = json(&message.text);
            let (role, part) = if message.user {
                ("user", "input_text")
            } else {
                ("assistant", "output_text")
            };
            if message.user {
                writeln!(
                    file,
                    r#"{{"timestamp":{time},"type":"event_msg","payload":{{"type":"user_message","message":{text},"images":[],"local_images":[],"text_elements":[]}}}}"#
                )?;
            } else {
                writeln!(
                    file,
                    r#"{{"timestamp":{time},"type":"event_msg","payload":{{"type":"agent_message","message":{text},"phase":"final_answer"}}}}"#
                )?;
            }
            writeln!(
                file,
                r#"{{"timestamp":{time},"type":"response_item","payload":{{"type":"message","role":"{role}","content":[{{"type":"{part}","text":{text}}}]}}}}"#
            )?;
        }
        file.sync_all()?;
        Ok(())
    })
}
