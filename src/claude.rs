use super::*;
use std::collections::HashSet;
use std::io::Read;

fn transcripts(root: &Path) -> Result<Vec<(String, PathBuf)>> {
    let mut paths = Vec::new();
    let projects = root.join("projects");
    if !projects.try_exists()? {
        return Ok(paths);
    }
    for project in fs::read_dir(projects)? {
        let project = project?;
        if !project.file_type()?.is_dir() {
            continue;
        }
        for entry in fs::read_dir(project.path())? {
            let entry = entry?;
            let name = entry.file_name();
            if let Some(id) = name
                .to_str()
                .and_then(|s| s.strip_suffix(".jsonl"))
                .filter(|s| uuid(s))
                && entry.file_type()?.is_file()
            {
                paths.push((id.to_owned(), entry.path()));
            }
        }
    }
    paths.sort();
    Ok(paths)
}

pub fn existing_ids(root: &Path) -> Result<HashSet<String>> {
    Ok(transcripts(root)?.into_iter().map(|(id, _)| id).collect())
}

fn read(path: &Path, id: String, home: &Path) -> Result<Option<Session<'static>>> {
    let output = Command::new("jq")
        .args(["-r", include_str!("claude.jq")])
        .stdin(File::open(path)?)
        .output()?;
    if !output.status.success() {
        return Err(format!("jq exited with {}", output.status).into());
    }
    let mut messages = Vec::new();
    let mut cwd = None;
    let output = String::from_utf8(output.stdout)?;
    for row in output.lines() {
        let mut fields = row.splitn(4, '\t');
        let role = fields.next().ok_or("missing Claude message role")?;
        let time = fields
            .next()
            .filter(|s| !s.is_empty())
            .ok_or("missing Claude message timestamp")?;
        let workspace = unescape_tsv(fields.next().ok_or("missing Claude project field")?);
        let text = unescape_tsv(fields.next().ok_or("missing Claude message text")?);
        if cwd.is_none() && Path::new(&workspace).is_absolute() {
            cwd = Some(workspace);
        }
        messages.push(Message {
            user: role == "user",
            time: time.to_owned().into(),
            text: text.into(),
        });
    }
    let Some(first) = messages.first() else {
        return Ok(None);
    };
    Ok(Some(Session {
        time: first.time.to_string(),
        id,
        cwd: cwd.unwrap_or_else(|| home.to_string_lossy().into_owned()),
        title: None,
        messages,
    }))
}

pub fn read_all(
    root: &Path,
    home: &Path,
    mut convert: impl FnMut(&Session<'_>) -> Result<bool>,
) -> Result<()> {
    let paths = transcripts(root)?;
    if paths.is_empty() {
        return Err(format!("no Claude Code transcripts in {}", root.display()).into());
    }
    let (mut written, mut existing, mut empty, mut failed) = (0, 0, 0, 0);
    let mut seen = HashSet::new();
    for (id, path) in paths {
        match read(&path, id, home).and_then(|session| {
            session
                .as_ref()
                .map(|session| {
                    if seen.contains(&session.id) {
                        return Ok(false);
                    }
                    let result = convert(session)?;
                    seen.insert(session.id.clone());
                    Ok(result)
                })
                .transpose()
        }) {
            Ok(Some(true)) => written += 1,
            Ok(Some(false)) => existing += 1,
            Ok(None) => empty += 1,
            Err(error) => {
                eprintln!("{}: {error}", path.display());
                failed += 1;
            }
        }
    }
    println!(
        "{written} converted, {existing} already present, {empty} without text, {failed} failed."
    );
    if failed > 0 {
        return Err("some Claude Code transcripts could not be imported".into());
    }
    Ok(())
}

fn project_key(cwd: &str) -> String {
    let mut key: String = cwd
        .encode_utf16()
        .map(|unit| {
            char::from_u32(u32::from(unit))
                .filter(char::is_ascii_alphanumeric)
                .unwrap_or('-')
        })
        .collect();
    if key.len() > 200 {
        // Claude hashes the original UTF-16 path with signed 32-bit JavaScript arithmetic.
        let hash = cwd.encode_utf16().fold(0i32, |hash, unit| {
            hash.wrapping_mul(31).wrapping_add(i32::from(unit))
        });
        let mut value = hash.unsigned_abs();
        let digits = b"0123456789abcdefghijklmnopqrstuvwxyz";
        let mut reversed = Vec::new();
        loop {
            reversed.push(digits[(value % 36) as usize]);
            value /= 36;
            if value == 0 {
                break;
            }
        }
        key.truncate(200);
        key.push('-');
        key.extend(reversed.into_iter().rev().map(char::from));
    }
    key
}

fn message_uuid(random: &mut File) -> Result<String> {
    let mut bytes = [0u8; 16];
    random.read_exact(&mut bytes)?;
    bytes[6] = (bytes[6] & 15) | 64;
    bytes[8] = (bytes[8] & 63) | 128;
    let id = hex(bytes);
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &id[..8],
        &id[8..12],
        &id[12..16],
        &id[16..20],
        &id[20..]
    ))
}

pub fn write(root: &Path, home: &Path, session: &Session<'_>, dry_run: bool) -> Result<bool> {
    let path = Path::new(&session.cwd);
    let path = if path.is_absolute() { path } else { home };
    let canonical = path.canonicalize();
    let cwd = canonical.as_deref().unwrap_or(path).to_string_lossy();
    let destination = root
        .join("projects")
        .join(project_key(&cwd))
        .join(format!("{}.jsonl", session.id));
    if dry_run {
        return Ok(!destination.try_exists()?);
    }
    install(&destination, |_, mut file| {
        let mut random = File::open("/dev/urandom")?;
        let mut parent = String::from("null");
        let cwd = json(&path.to_string_lossy());
        let id = json(&session.id);
        for message in &session.messages {
            let uuid = message_uuid(&mut random)?;
            let entry_id = json(&uuid);
            let time = json(&message.time);
            let text = json(&message.text);
            let role = if message.user { "user" } else { "assistant" };
            let fields = if message.user {
                String::new()
            } else {
                format!(
                    r#","id":"msg_{}","type":"message","stop_reason":"end_turn","stop_sequence":null,"usage":{{"input_tokens":0,"output_tokens":0}}"#,
                    uuid.replace('-', "")
                )
            };
            writeln!(
                file,
                r#"{{"parentUuid":{parent},"isSidechain":false,"type":"{role}","uuid":{entry_id},"sessionId":{id},"cwd":{cwd},"timestamp":{time},"userType":"external","message":{{"role":"{role}","content":[{{"type":"text","text":{text}}}]{fields}}}}}"#
            )?;
            parent = entry_id;
        }
        file.sync_all()?;
        Ok(())
    })
}
