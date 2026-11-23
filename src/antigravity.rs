use super::*;
use crate::proto::{bytes, get, integer, number, replace_bytes};
use std::io::Read;

const APPLICATION_ID: u32 = 1396855367;

fn imported(path: &Path) -> Result<bool> {
    let mut header = [0; 72];
    match File::open(path)?.read_exact(&mut header) {
        Ok(()) => {
            Ok(&header[..16] == b"SQLite format 3\0"
                && header[68..] == APPLICATION_ID.to_be_bytes())
        }
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(false),
        Err(e) => Err(e.into()),
    }
}

fn prompt_preview(session: &Session<'_>) -> String {
    session
        .messages
        .iter()
        .find(|m| m.user)
        .unwrap_or(&session.messages[0])
        .text
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("Imported conversation")
        .trim()
        .chars()
        .take(100)
        .collect()
}

fn context_title(value: &str) -> bool {
    let value = value.trim_start();
    [
        "<environment_context",
        "# AGENTS.md instructions",
        "<external_codex_apps_open_page",
        "<codex_internal_context",
        "<guardian_tool_descriptions",
    ]
    .iter()
    .any(|prefix| value.starts_with(prefix))
}

fn title(session: &Session<'_>, preview: &str) -> String {
    session
        .title
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty() && !context_title(text))
        .unwrap_or(preview)
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(240)
        .collect()
}

fn annotation(root: &Path, id: &str, title: &str) -> Result<bool> {
    let path = root.join("annotations").join(format!("{id}.pbtxt"));
    install(&path, |_, mut file| {
        writeln!(file, "title: {}", json(title))?;
        file.sync_all()?;
        Ok(())
    })
}

fn native_date(iso: &str) -> String {
    let mut date = iso.trim_end_matches('Z');
    if let Some((stem, fraction)) = date.split_once('.') {
        let fraction = fraction.trim_end_matches('0');
        date = if fraction.is_empty() {
            stem
        } else {
            &date[..stem.len() + 1 + fraction.len()]
        };
    }
    date.replace('T', " ") + "+00:00"
}

fn read_annotation(path: &Path) -> Result<Option<String>> {
    let content = fs::read_to_string(path)?;
    let Some(literal) = content
        .lines()
        .find_map(|line| line.trim_start().strip_prefix("title:").map(str::trim))
    else {
        return Ok(None);
    };
    // Go's prototext writes quoted strings. JSON covers the escape sequences
    // used in normal titles. Leave other native annotations untouched.
    let result = sqlite(
        Path::new(":memory:"),
        &format!(
            "SELECT CASE WHEN json_valid({0}) THEN hex(json_extract({0}, '$')) END;\n",
            sql_text(literal)
        ),
        false,
    )?;
    Ok(Some(String::from_utf8(unhex(result.trim())?)?).filter(|title| !title.is_empty()))
}

fn indexed_dates(path: &Path) -> Result<Vec<String>> {
    let rows = sqlite(
        path,
        "SELECT hex((SELECT metadata FROM steps ORDER BY idx DESC LIMIT 1)), \
         hex((SELECT metadata FROM steps WHERE step_type=14 ORDER BY idx DESC LIMIT 1));\n",
        true,
    )?;
    let dates: Result<Vec<_>> = rows
        .trim_end_matches('\n')
        .split('\t')
        .filter(|value| !value.is_empty())
        .map(|value| {
            let metadata = unhex(value)?;
            let created = get(&metadata, 1)?.ok_or("missing step creation time")?;
            decode_time(created)
        })
        .collect();
    let dates = dates?;
    if dates.is_empty() {
        return Ok(Vec::new());
    }
    Ok(time::iso(&dates)?
        .iter()
        .map(|value| native_date(value))
        .collect())
}

// Earlier imports kept step metadata only within the payload. agy reads the
// separate column to determine recency when it rebuilds the conversation list.
fn repair_step_metadata(path: &Path) -> Result<bool> {
    let rows = sqlite(
        path,
        "SELECT idx,hex(step_payload) FROM steps WHERE metadata IS NULL AND step_format=0 AND step_type IN (14,15);\n",
        true,
    )?;
    if rows.is_empty() {
        return Ok(false);
    }
    let mut sql = String::from(".timeout 5000\nBEGIN;\n");
    for row in rows.lines() {
        let (index, payload) = row.split_once('\t').ok_or("invalid trajectory row")?;
        let index = index.parse::<i64>()?;
        let data = unhex(payload)?;
        if let Some(metadata) = get(&data, 5)? {
            writeln!(
                sql,
                "UPDATE steps SET metadata=X'{}' WHERE idx={index} AND metadata IS NULL AND step_payload=X'{payload}';",
                hex(metadata)
            )?;
        }
    }
    sql.push_str("COMMIT;\n");
    sqlite(path, &sql, false)?;
    Ok(true)
}

fn repair_existing(
    root: &Path,
    path: &Path,
    session: &Session<'_>,
    dry_run: bool,
) -> Result<Option<bool>> {
    if !imported(path)? {
        return Ok(Some(false));
    }
    let index = root.join("conversation_summaries.db");
    if !index.try_exists()? {
        if !dry_run {
            repair_step_metadata(path)?;
        }
        return Ok(None);
    }
    let id = &session.id;
    let probe = "SELECT name FROM sqlite_master WHERE name IN ('conversation_summaries','conversations_fts');\n";
    let tables = sqlite(&index, probe, true)?;
    if !tables.lines().any(|s| s == "conversation_summaries") {
        if !dry_run {
            repair_step_metadata(path)?;
        }
        return Ok(None);
    }
    let row = sqlite(
        &index,
        &format!(
            "SELECT hex(title),hex(preview),hex(raw_summary),hex(last_modified_time),hex(last_user_input_time) FROM conversation_summaries WHERE conversation_id='{id}';\n"
        ),
        true,
    )?;
    if row.is_empty() {
        if !dry_run {
            repair_step_metadata(path)?;
        }
        return Ok(None);
    }
    let mut cells = row.trim_end_matches('\n').split('\t');
    let [
        Some(old_title_hex),
        Some(old_preview_hex),
        Some(raw_hex),
        Some(modified_hex),
        Some(user_time_hex),
    ] = std::array::from_fn(|_| cells.next())
    else {
        return Err("invalid Antigravity summary row".into());
    };
    let old_title = String::from_utf8(unhex(old_title_hex)?)?;
    let old_preview = String::from_utf8(unhex(old_preview_hex)?)?;
    let preview = prompt_preview(session);
    let annotation_path = root.join("annotations").join(format!("{id}.pbtxt"));
    let has_annotation = annotation_path.try_exists()?;
    let old_title_missing = old_title.trim().is_empty() || context_title(&old_title);
    let mut desired = if old_title_missing || (!has_annotation && old_title == old_preview) {
        title(session, &preview)
    } else {
        old_title.clone()
    };
    if has_annotation
        && old_title_missing
        && let Some(title) = read_annotation(&annotation_path)?
    {
        desired = title;
    }
    let update_title = old_title != desired;
    let update_preview = old_preview.trim().is_empty() || context_title(&old_preview);
    if dry_run {
        let missing = sqlite(
            path,
            "SELECT 1 FROM steps WHERE metadata IS NULL AND step_type IN (14,15) LIMIT 1;\n",
            true,
        )?;
        return Ok(Some(
            !has_annotation || update_title || update_preview || !missing.is_empty(),
        ));
    }

    let metadata_repaired = repair_step_metadata(path)?;
    let annotated = annotation(root, id, &desired)?;
    let dates = if metadata_repaired || old_title_missing || update_preview {
        indexed_dates(path)?
    } else {
        Vec::new()
    };
    if !update_title && !update_preview && dates.is_empty() && !annotated && !metadata_repaired {
        return Ok(Some(false));
    }
    let mut patch = String::from(".timeout 5000\nBEGIN;\n");
    if update_title || annotated {
        let mut details = Vec::new();
        bytes(1, &desired, &mut details);
        let raw = replace_bytes(&unhex(raw_hex)?, &[(1, desired.as_bytes()), (15, &details)])?;
        writeln!(
            patch,
            "UPDATE conversation_summaries SET title={}, raw_summary=X'{}' WHERE conversation_id='{id}' AND hex(title)='{old_title_hex}';",
            sql_text(&desired),
            hex(raw)
        )?;
    }
    if update_preview {
        writeln!(
            patch,
            "UPDATE conversation_summaries SET preview={} WHERE conversation_id='{id}' AND hex(preview)='{old_preview_hex}';",
            sql_text(&preview)
        )?;
    }
    if let Some(date) = dates.first() {
        writeln!(
            patch,
            "UPDATE conversation_summaries SET last_modified_time={} WHERE conversation_id='{id}' AND hex(last_modified_time)='{modified_hex}';",
            sql_text(date)
        )?;
    }
    if let Some(date) = dates.get(1) {
        writeln!(
            patch,
            "UPDATE conversation_summaries SET last_user_input_time={} WHERE conversation_id='{id}' AND hex(last_user_input_time)='{user_time_hex}';",
            sql_text(date)
        )?;
    }
    if tables.lines().any(|s| s == "conversations_fts") {
        writeln!(
            patch,
            "UPDATE conversations_fts SET title=(SELECT title FROM conversation_summaries WHERE conversation_id='{id}') WHERE conversation_id='{id}' AND cast(chunk_index AS integer)=-1;"
        )?;
    }
    patch.push_str("COMMIT;\n");
    sqlite(&index, &patch, false)?;
    Ok(Some(true))
}

fn timestamp((seconds, nanos): (u64, u64)) -> Vec<u8> {
    let mut value = Vec::new();
    number(1, seconds, &mut value);
    if nanos != 0 {
        number(2, nanos, &mut value);
    }
    value
}

fn uri(path: &str) -> String {
    let mut uri = String::from("file://");
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || b"/-._~".contains(&byte) {
            uri.push(byte as char);
        } else {
            write!(uri, "%{byte:02X}").unwrap();
        }
    }
    uri
}

fn from_uri(uri: &str) -> Result<String> {
    let mut path = uri
        .strip_prefix("file://")
        .ok_or("workspace is not a file URI")?
        .as_bytes();
    let mut out = Vec::new();
    while let Some((&byte, rest)) = path.split_first() {
        path = rest;
        if byte == b'%' {
            let (encoded, rest) = path.split_at_checked(2).ok_or("invalid workspace URL")?;
            out.push(u8::from_str_radix(std::str::from_utf8(encoded)?, 16)?);
            path = rest;
        } else {
            out.push(byte);
        }
    }
    let path = String::from_utf8(out)?;
    if !Path::new(&path).is_absolute() {
        return Err("workspace is not an absolute path".into());
    }
    Ok(path)
}

pub fn write(root: &Path, session: &Session<'_>, dry_run: bool) -> Result<bool> {
    let id = &session.id;
    let path = root.join("conversations").join(format!("{id}.db"));
    let legacy = path.with_extension("pb");
    if legacy.try_exists()? {
        return Ok(false);
    }
    if path.try_exists()?
        && let Some(result) = repair_existing(root, &path, session, dry_run)?
    {
        return Ok(result);
    }
    let times: Vec<_> = std::iter::once(session.time.as_str())
        .chain(session.messages.iter().map(|m| m.time.as_ref()))
        .collect();
    let dates = time::timestamps(&times)?;
    if dry_run {
        return Ok(true);
    }

    let last_user = session
        .messages
        .iter()
        .rposition(|message| message.user)
        .unwrap_or(0);
    let modified = dates[dates.len() - 1];
    let user_time = dates[last_user + 1];
    let index_dates = time::iso(&[modified, user_time])?;
    let trajectory = format!(
        "{}{}",
        if id.starts_with('f') { 'e' } else { 'f' },
        &id[1..]
    );
    let workspace_uri = uri(&session.cwd);
    let mut workspace = Vec::new();
    bytes(1, &workspace_uri, &mut workspace);
    bytes(3, "", &mut workspace);
    let mut meta = Vec::new();
    bytes(1, &workspace, &mut meta);
    bytes(2, timestamp(dates[0]), &mut meta);
    bytes(6, id, &mut meta);
    bytes(7, &workspace_uri, &mut meta);

    let mut sql = format!(
        "PRAGMA application_id={APPLICATION_ID};\nBEGIN;\n{}\n",
        include_str!("trajectory.sql")
    );
    writeln!(
        sql,
        "INSERT INTO trajectory_meta VALUES ('{trajectory}','{id}',4,17);"
    )?;
    writeln!(
        sql,
        "INSERT INTO trajectory_metadata_blob VALUES ('main',X'{}');",
        hex(&meta)
    )?;
    for (idx, (message, &date)) in session.messages.iter().zip(&dates[1..]).enumerate() {
        let mut reference = Vec::new();
        bytes(1, &trajectory, &mut reference);
        number(2, idx as u64, &mut reference);
        bytes(4, id, &mut reference);
        let mut metadata = Vec::new();
        bytes(1, timestamp(date), &mut metadata);
        number(3, if message.user { 4 } else { 2 }, &mut metadata);
        bytes(20, reference, &mut metadata);
        bytes(32, timestamp(date), &mut metadata);

        let mut body = Vec::new();
        let (kind, field) = if message.user {
            bytes(2, message.text.as_bytes(), &mut body);
            let mut fragment = Vec::new();
            bytes(1, message.text.as_bytes(), &mut fragment);
            bytes(3, fragment, &mut body);
            (14, 19)
        } else {
            bytes(1, message.text.as_bytes(), &mut body);
            bytes(8, message.text.as_bytes(), &mut body);
            (15, 20)
        };
        let mut payload = Vec::new();
        number(1, kind, &mut payload);
        number(4, 3, &mut payload);
        bytes(5, &metadata, &mut payload);
        bytes(field, body, &mut payload);
        writeln!(
            sql,
            "INSERT INTO steps(idx,step_type,status,metadata,step_payload) VALUES ({idx},{kind},3,X'{}',X'{}');",
            hex(metadata),
            hex(payload)
        )?;
    }
    sql.push_str("COMMIT;\n");
    let created = install(&path, |temp, file| {
        drop(file);
        sqlite(temp, &sql, false)?;
        File::open(temp)?.sync_all()?;
        Ok(())
    })?;
    if !created && !imported(&path)? {
        return Ok(false);
    }

    let preview = prompt_preview(session);
    let title = title(session, &preview);
    annotation(root, id, &title)?;
    let mut summary = Vec::new();
    bytes(1, &title, &mut summary);
    number(2, session.messages.len() as u64, &mut summary);
    bytes(3, timestamp(modified), &mut summary);
    bytes(4, id, &mut summary);
    number(5, 1, &mut summary);
    bytes(7, timestamp(dates[0]), &mut summary);
    bytes(9, workspace, &mut summary);
    bytes(10, timestamp(user_time), &mut summary);
    let mut title_details = Vec::new();
    bytes(1, &title, &mut title_details);
    bytes(15, title_details, &mut summary);
    number(16, last_user as u64, &mut summary);
    bytes(17, meta, &mut summary);
    number(22, 4, &mut summary);
    number(28, 4, &mut summary);

    let index_path = root.join("conversation_summaries.db");
    if !index_path.try_exists()? {
        match private_file(&index_path) {
            Ok(_) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(e.into()),
        }
    }
    let insert = format!(
        ".timeout 5000\n{}\nINSERT OR IGNORE INTO conversation_summaries \
         (conversation_id,title,preview,step_count,last_modified_time,workspace_uris,status,last_user_input_time,last_user_input_step_index,raw_summary) \
         VALUES ('{id}',{title},{preview},{},{},{},'CASCADE_RUN_STATUS_IDLE',{},{last_user},X'{}');\n",
        include_str!("summaries.sql"),
        session.messages.len(),
        sql_text(&native_date(&index_dates[0])),
        sql_text(&format!("[{}]", json(&workspace_uri))),
        sql_text(&native_date(&index_dates[1])),
        hex(summary),
        title = sql_text(&title),
        preview = sql_text(&preview),
    );
    sqlite(&index_path, &insert, false)?;
    Ok(true)
}

fn decode_time(input: &[u8]) -> Result<(u64, u64)> {
    Ok((integer(input, 1)?, integer(input, 2)?))
}

fn read(path: &Path, id: &str, home: &Path) -> Result<Option<Session<'static>>> {
    let metadata = sqlite(
        path,
        "SELECT hex(data) FROM trajectory_metadata_blob WHERE id='main';\n",
        true,
    )?;
    let metadata = unhex(metadata.trim())?;
    let cwd = get(&metadata, 1)?
        .and_then(|workspace| get(workspace, 1).ok().flatten())
        .and_then(|uri| std::str::from_utf8(uri).ok())
        .and_then(|uri| from_uri(uri).ok())
        .unwrap_or_else(|| home.to_string_lossy().into_owned());
    let rows = sqlite(
        path,
        "SELECT step_type,step_format,hex(step_payload) FROM steps WHERE step_type IN (14,15) ORDER BY idx;\n",
        true,
    )?;
    let mut messages = Vec::new();
    let mut dates = Vec::new();
    for row in rows.lines() {
        let mut parts = row.splitn(3, '\t');
        let user = parts.next() == Some("14");
        if parts.next() != Some("0") {
            return Err("unsupported Antigravity step format".into());
        }
        let payload = unhex(parts.next().ok_or("missing step payload")?)?;
        let Some(body) = get(&payload, if user { 19 } else { 20 })? else {
            continue;
        };
        let text = if user {
            get(body, 2)?
        } else {
            get(body, 8)?.or(get(body, 1)?)
        };
        let Some(text) = text.filter(|s| !s.is_empty()) else {
            continue;
        };
        let meta = get(&payload, 5)?.ok_or("missing step metadata")?;
        dates.push(decode_time(get(meta, 1)?.ok_or("missing step timestamp")?)?);
        messages.push(Message {
            user,
            time: Cow::Borrowed(""),
            text: std::str::from_utf8(text)?.to_owned().into(),
        });
    }
    if messages.is_empty() {
        return Ok(None);
    }
    for (message, date) in messages.iter_mut().zip(time::iso(&dates)?) {
        message.time = date.into();
    }
    Ok(Some(Session {
        id: id.to_owned(),
        time: messages[0].time.to_string(),
        cwd,
        title: None,
        messages,
    }))
}

pub fn read_all(
    root: &Path,
    home: &Path,
    mut convert: impl FnMut(&Session<'_>) -> Result<bool>,
) -> Result<()> {
    let path = root.join("conversations");
    let mut entries = Vec::new();
    collect(
        &path,
        &|name| {
            name.strip_suffix(".db")
                .or_else(|| name.strip_suffix(".pb"))
                .is_some_and(uuid)
        },
        &mut entries,
    )?;
    entries.sort();
    let (mut written, mut existing, mut empty, mut legacy, mut failed) = (0, 0, 0, 0, 0);
    for entry in entries {
        if entry.extension().is_some_and(|ext| ext == "pb") {
            if !entry.with_extension("db").try_exists()? {
                legacy += 1;
            }
            continue;
        }
        let id = entry
            .file_stem()
            .and_then(|s| s.to_str())
            .ok_or("missing conversation ID")?;
        match read(&entry, id, home)
            .and_then(|session| session.as_ref().map(&mut convert).transpose())
        {
            Ok(Some(true)) => written += 1,
            Ok(Some(false)) => existing += 1,
            Ok(None) => empty += 1,
            Err(e) => {
                eprintln!("{id}: {e}");
                failed += 1;
            }
        }
    }
    println!(
        "{written} converted, {existing} already present, {empty} without text, {legacy} unsupported legacy .pb, {failed} failed."
    );
    if failed > 0 {
        return Err("some conversations could not be imported".into());
    }
    Ok(())
}
