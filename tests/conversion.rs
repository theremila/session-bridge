use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

const ID: &str = "82f06ba2-26cc-45bc-b811-782ba89291d7";
const START: &str = "2026-01-01T10:00:00.123456789Z";
const END: &str = "2026-01-01T10:00:01.987654321Z";
const WORKSPACE: &str = "/tmp/fixture workspace/é";
const TITLE: &str = "Sam's integration test 🧪";
const PREVIEW: &str = "Hello from the fixture.";
const USER_JSON: &str = r#""Hello from the fixture.\nTab:\t Quote:\" Slash:\\ Snow:雪""#;
const REPLY_JSON: &str = r#""Assistant reply:\nAll set. ✅""#;
const CONTEXT: &str =
    r#""<environment_context>\n<cwd>/tmp/fixture workspace</cwd>\n</environment_context>""#;
const AGENTS: &str = r##""# AGENTS.md instructions for /tmp/example\n\n<INSTRUCTIONS>\nFixture instructions.\n</INSTRUCTIONS>""##;

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        loop {
            let path = std::env::temp_dir().join(format!(
                "session-bridge-test-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed),
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("cannot create fixture directory: {error}"),
            }
        }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn output(command: &mut Command) -> Output {
    command.output().expect("failed to spawn test command")
}

fn success(command: &mut Command) -> String {
    let result = output(command);
    assert!(
        result.status.success(),
        "{command:?} failed with {}.\nstdout: {}\nstderr: {}",
        result.status,
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8(result.stdout).expect("command emitted invalid UTF-8")
}

fn bridge(from: &str, source: &Path, to: &str, target: &Path, extra: &[&str]) -> String {
    success(
        Command::new(env!("CARGO_BIN_EXE_session-bridge"))
            .args(["--from", from, "--to", to, &format!("--{from}-dir")])
            .arg(source)
            .arg(format!("--{to}-dir"))
            .arg(target)
            .args(extra),
    )
}

fn sqlite(path: &Path, sql: &str) -> String {
    success(
        Command::new("sqlite3")
            .args(["-batch", "-noheader", "-separator", "\t"])
            .arg(path)
            .arg(sql),
    )
}

fn write(path: PathBuf, data: impl AsRef<[u8]>) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, data).unwrap();
}

fn codex_record(role: &str, time: &str, content: &str) -> String {
    let kind = if role == "user" {
        "input_text"
    } else {
        "output_text"
    };
    format!(
        r#"{{"timestamp":"{time}","type":"response_item","payload":{{"type":"message","role":"{role}","content":[{{"type":"{kind}","text":{content}}}]}}}}"#
    )
}

fn codex_fixture(root: &Path, inject_context: bool, indexed_title: bool) {
    let path = root.join(format!(
        "sessions/2026/01/01/rollout-2026-01-01T10-00-00-{ID}.jsonl"
    ));
    let mut rows = vec![format!(
        r#"{{"timestamp":"{START}","type":"session_meta","payload":{{"id":"{ID}","timestamp":"{START}","cwd":"{WORKSPACE}"}}}}"#
    )];
    if inject_context {
        rows.push(codex_record("user", START, CONTEXT));
        rows.push(codex_record("user", START, AGENTS));
    }
    rows.push(codex_record("user", START, USER_JSON));
    rows.push(codex_record("assistant", END, REPLY_JSON));
    write(path, rows.join("\n") + "\n");
    if indexed_title {
        write(
            root.join("session_index.jsonl"),
            format!(
                "{{\"id\":\"{ID}\",\"thread_name\":\"Outdated fixture name\",\"updated_at\":\"{START}\"}}\n\
                 {{\"id\":\"{ID}\",\"thread_name\":\"{TITLE}\",\"updated_at\":\"{END}\"}}\n"
            ),
        );
    }
}

fn claude_fixture(root: &Path) {
    let content = format!(
        r#"{{"type":"user","timestamp":"{START}","cwd":"{WORKSPACE}","sessionId":"{ID}","message":{{"content":{USER_JSON}}}}}
{{"type":"assistant","timestamp":"{END}","message":{{"content":[{{"type":"thinking","thinking":"hidden reasoning"}},{{"type":"text","text":{REPLY_JSON}}},{{"type":"tool_use","name":"ignored"}}]}}}}
{{"type":"user","timestamp":"{END}","isSidechain":true,"message":{{"content":"hidden sidechain"}}}}
{{"type":"user","timestamp":"{END}","isMeta":true,"message":{{"content":"hidden context"}}}}
{{"type":"user","timestamp":"{END}","message":{{"content":[{{"type":"tool_result","content":"hidden tool result"}}]}}}}
"#
    );
    write(root.join(format!("projects/synthetic/{ID}.jsonl")), content);
}

fn transcript(root: &Path) -> PathBuf {
    let mut dirs = vec![root.to_path_buf()];
    let mut files = Vec::new();
    while let Some(dir) = dirs.pop() {
        for entry in fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                dirs.push(path);
            } else if path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(&format!("{ID}.jsonl")))
            {
                files.push(path);
            }
        }
    }
    assert_eq!(files.len(), 1, "expected one transcript in {root:?}");
    files.pop().unwrap()
}

fn assert_json_transcript(root: &Path, format: &str) {
    let texts = format!(
        "[{{time:\"{START}\",role:\"user\",text:{USER_JSON}}},{{time:\"{END}\",role:\"assistant\",text:{REPLY_JSON}}}]"
    );
    let filter = match format {
        "codex" => format!(
            r#"
            any(.[]; .type == "session_meta" and .payload.id == "{ID}" and .payload.cwd == "{WORKSPACE}") and
            ([.[] | select(.type == "response_item") | {{ time:.timestamp, role:.payload.role, text:.payload.content[0].text }}] == {texts})
            "#
        ),
        "claude" => format!(
            r#"
            all(.[]; .sessionId == "{ID}" and .cwd == "{WORKSPACE}") and
            .[0].parentUuid == null and .[1].parentUuid == .[0].uuid and
            ([.[] | {{ time:.timestamp,role:.type,text:.message.content[0].text }}] == {texts})
            "#
        ),
        _ => unreachable!(),
    };
    success(
        Command::new("jq")
            .args(["-es", &filter])
            .arg(transcript(root)),
    );
}

fn agy_db(root: &Path) -> PathBuf {
    root.join(format!("conversations/{ID}.db"))
}

fn summary(root: &Path) -> PathBuf {
    root.join("conversation_summaries.db")
}

fn annotation(root: &Path) -> PathBuf {
    root.join(format!("annotations/{ID}.pbtxt"))
}

fn step_payloads(root: &Path) -> String {
    sqlite(
        &agy_db(root),
        "SELECT idx,hex(step_payload) FROM steps ORDER BY idx;",
    )
}

fn assert_step_metadata(root: &Path) {
    assert_eq!(
        sqlite(
            &agy_db(root),
            "SELECT count(*) FROM steps WHERE length(metadata) > 0 AND instr(step_payload, metadata) > 0;"
        ),
        "2\n"
    );
}

fn assert_agy_transcript(root: &Path) {
    let db = agy_db(root);
    assert_eq!(sqlite(&db, "PRAGMA integrity_check;"), "ok\n");
    assert_step_metadata(root);
    let result = sqlite(
        &db,
        "SELECT step_type, instr(step_payload, 'Hello from the fixture.'), \
         instr(step_payload, 'Assistant reply:') FROM steps ORDER BY idx;",
    );
    let rows: Vec<_> = result.lines().collect();
    assert_eq!(rows.len(), 2, "{result:?}");
    assert!(
        rows[0].starts_with("14\t") && rows[0].ends_with("\t0"),
        "{result:?}"
    );
    assert_ne!(rows[0], "14\t0\t0", "{result:?}");
    assert!(rows[1].starts_with("15\t0\t"), "{result:?}");
    assert_ne!(rows[1], "15\t0\t0", "{result:?}");
}

#[test]
fn all_six_directions_dry_run_and_idempotency() {
    let scratch = Scratch::new();
    let codex = scratch.path("codex-source");
    let claude = scratch.path("claude-source");
    let agy = scratch.path("agy-source");
    codex_fixture(&codex, false, false);
    claude_fixture(&claude);
    bridge("codex", &codex, "agy", &agy, &[]);
    assert_agy_transcript(&agy);
    let stores = [("codex", &codex), ("claude", &claude), ("agy", &agy)];
    for (from, source) in stores {
        for (to, _) in stores {
            if from == to {
                continue;
            }
            let target = scratch.path(&format!("{from}-to-{to}"));
            let preview = bridge(from, source, to, &target, &["--dry-run"]);
            assert!(preview.contains("1 converted"), "{from} -> {to}: {preview}");
            assert!(!target.exists(), "{from} -> {to} dry run wrote data");
            let first = bridge(from, source, to, &target, &[]);
            assert!(first.contains("1 converted"), "{from} -> {to}: {first}");
            match to {
                "agy" => assert_agy_transcript(&target),
                _ => assert_json_transcript(&target, to),
            }
            let rerun = bridge(from, source, to, &target, &[]);
            assert!(
                rerun.contains("1 already present"),
                "{from} -> {to}: {rerun}"
            );
        }
    }
}

fn agy_title_and_preview(root: &Path) -> String {
    let result = sqlite(
        &summary(root),
        &format!("SELECT title,preview FROM conversation_summaries WHERE conversation_id='{ID}';"),
    );
    result.trim_end().to_owned()
}

fn codex_state(root: &Path, version: u8, title: &str, name: &str) {
    sqlite(
        &root.join(format!("state_{version}.sqlite")),
        &format!(
            "CREATE TABLE threads(id TEXT PRIMARY KEY, title TEXT, name TEXT); \
             INSERT INTO threads VALUES ('{ID}', '{title}', '{name}');"
        ),
    );
}

#[test]
fn codex_title_falls_back_to_the_latest_state_database() {
    let scratch = Scratch::new();
    let codex = scratch.path("codex");
    let agy = scratch.path("agy");
    codex_fixture(&codex, true, false);
    codex_state(&codex, 9, "Older state title", "");
    codex_state(&codex, 10, "Latest state title", "Named in Codex state");
    bridge("codex", &codex, "agy", &agy, &[]);
    assert_eq!(
        agy_title_and_preview(&agy),
        format!("Named in Codex state\t{PREVIEW}"),
    );
}

#[test]
fn indexed_codex_title_and_clean_preview_reach_agy() {
    let scratch = Scratch::new();
    let codex = scratch.path("codex");
    let agy = scratch.path("agy");
    codex_fixture(&codex, true, true);
    codex_state(&codex, 10, "Generated state title", "Older thread name");
    bridge("codex", &codex, "agy", &agy, &[]);
    assert_agy_transcript(&agy);
    assert_eq!(agy_title_and_preview(&agy), format!("{TITLE}\t{PREVIEW}"));
    let title_annotation = fs::read_to_string(annotation(&agy)).unwrap();
    assert!(
        title_annotation
            .lines()
            .any(|line| line.trim() == format!("title: \"{TITLE}\"")),
        "{title_annotation:?}"
    );
    let raw = sqlite(
        &summary(&agy),
        &format!(
            "SELECT instr(raw_summary, CAST(title AS BLOB)) > 0, \
             instr(raw_summary, '<environment_context>') FROM conversation_summaries \
             WHERE conversation_id='{ID}';"
        ),
    );
    assert_eq!(raw, "1\t0\n");
}

#[test]
fn boilerplate_is_skipped_when_falling_back_to_the_first_prompt() {
    let scratch = Scratch::new();
    let codex = scratch.path("codex");
    let agy = scratch.path("agy");
    codex_fixture(&codex, true, false);
    bridge("codex", &codex, "agy", &agy, &[]);
    assert_agy_transcript(&agy);
    assert_eq!(agy_title_and_preview(&agy), format!("{PREVIEW}\t{PREVIEW}"));
}

#[test]
fn rerun_repairs_owned_summary_and_preserves_user_title() {
    let scratch = Scratch::new();
    let codex = scratch.path("codex");
    let agy = scratch.path("agy");
    codex_fixture(&codex, true, true);
    bridge("codex", &codex, "agy", &agy, &[]);
    let polluted = "<environment_context>";
    let db = summary(&agy);
    sqlite(
        &db,
        &format!(
            "UPDATE conversation_summaries SET title='{polluted}',preview='{polluted}', \
             raw_summary=X'0A153C656E7669726F6E6D656E745F636F6E746578743E' WHERE conversation_id='{ID}';"
        ),
    );
    sqlite(&agy_db(&agy), "UPDATE steps SET metadata=NULL;");
    fs::remove_file(annotation(&agy)).unwrap();
    let before = step_payloads(&agy);
    bridge("codex", &codex, "agy", &agy, &["--dry-run"]);
    assert!(!annotation(&agy).exists());
    assert_eq!(
        sqlite(
            &agy_db(&agy),
            "SELECT count(*) FROM steps WHERE metadata IS NULL;"
        ),
        "2\n"
    );
    assert_eq!(
        agy_title_and_preview(&agy),
        format!("{polluted}\t{polluted}")
    );
    bridge("codex", &codex, "agy", &agy, &[]);
    assert_eq!(agy_title_and_preview(&agy), format!("{TITLE}\t{PREVIEW}"));
    let raw = sqlite(
        &db,
        &format!(
            "SELECT instr(raw_summary, CAST(title AS BLOB)) > 0, \
             instr(raw_summary, '{polluted}') FROM conversation_summaries \
             WHERE conversation_id='{ID}';"
        ),
    );
    assert_eq!(raw, "1\t0\n");
    assert_step_metadata(&agy);
    assert_eq!(
        step_payloads(&agy),
        before,
        "existing transcript payloads were replaced"
    );
    assert!(
        fs::read_to_string(annotation(&agy))
            .unwrap()
            .contains(TITLE)
    );

    let custom = "Renamed in the target";
    fs::remove_file(annotation(&agy)).unwrap();
    sqlite(
        &db,
        &format!(
            "UPDATE conversation_summaries SET title='{custom}' WHERE conversation_id='{ID}';"
        ),
    );
    bridge("codex", &codex, "agy", &agy, &[]);
    assert_eq!(agy_title_and_preview(&agy), format!("{custom}\t{PREVIEW}"));
}

#[test]
fn restored_legacy_summary_uses_original_timestamp() {
    let scratch = Scratch::new();
    let codex = scratch.path("codex");
    let agy = scratch.path("agy");
    codex_fixture(&codex, true, true);
    bridge("codex", &codex, "agy", &agy, &[]);
    let db = summary(&agy);
    let before = sqlite(
        &db,
        "SELECT last_modified_time,last_user_input_time FROM conversation_summaries;",
    );
    sqlite(
        &db,
        &format!(
            "UPDATE conversation_summaries SET title='',preview='',last_modified_time='1970-01-01T00:00:00Z', \
             last_user_input_time='1970-01-01T00:00:00Z' WHERE conversation_id='{ID}';"
        ),
    );
    bridge("codex", &codex, "agy", &agy, &[]);
    assert_eq!(agy_title_and_preview(&agy), format!("{TITLE}\t{PREVIEW}"));
    assert_eq!(
        sqlite(
            &db,
            "SELECT last_modified_time,last_user_input_time FROM conversation_summaries;"
        ),
        before
    );
}

#[test]
fn repair_leaves_custom_native_annotation_in_place() {
    let scratch = Scratch::new();
    let codex = scratch.path("codex");
    let agy = scratch.path("agy");
    codex_fixture(&codex, true, true);
    bridge("codex", &codex, "agy", &agy, &[]);
    let annotation_path = annotation(&agy);
    let custom = "title: \"Renamed through the native client\"\n";
    fs::write(&annotation_path, custom).unwrap();
    let db = summary(&agy);
    sqlite(
        &db,
        &format!(
            "UPDATE conversation_summaries SET title='',preview='' WHERE conversation_id='{ID}';"
        ),
    );
    bridge("codex", &codex, "agy", &agy, &[]);
    assert_eq!(fs::read_to_string(annotation_path).unwrap(), custom);
    assert_eq!(
        agy_title_and_preview(&agy),
        format!("Renamed through the native client\t{PREVIEW}")
    );
}

#[test]
fn compressed_codex_history_respects_the_inherited_byte_cutoff() {
    let scratch = Scratch::new();
    let codex = scratch.path("codex");
    let claude = scratch.path("claude");
    let original = codex.join(format!(
        "archived_sessions/rollout-2026-01-01T10-00-00-{ID}.jsonl"
    ));
    let inherited = format!(
        "{}\n{}\n",
        format_args!(
            r#"{{"timestamp":"{START}","type":"session_meta","payload":{{"id":"{ID}","timestamp":"{START}","cwd":"{WORKSPACE}"}}}}"#
        ),
        codex_record("user", START, USER_JSON)
    );
    let cutoff = inherited.len();
    write(
        original.clone(),
        format!(
            "{inherited}{}\n",
            codex_record("assistant", END, r#""Unrelated branch reply.""#)
        ),
    );
    success(
        Command::new("zstd")
            .args(["-q", "-o"])
            .arg(original.with_extension("jsonl.zst"))
            .arg(&original),
    );
    fs::remove_file(original).unwrap();

    let segment_id = "1a4b171a-e83a-47f2-a4e6-0fd2c2877e2a";
    write(
        codex.join(format!(
            "sessions/2026/01/01/rollout-2026-01-01T10-00-01-{ID}_{segment_id}.jsonl"
        )),
        format!(
            "{}\n{}\n",
            format_args!(
                r#"{{"timestamp":"{END}","type":"session_meta","payload":{{"id":"{ID}","timestamp":"{END}","cwd":"{WORKSPACE}","history_base":{{"thread_id":"{ID}","end_byte_offset":{cutoff}}}}}}}"#
            ),
            codex_record("assistant", END, REPLY_JSON)
        ),
    );

    let output = bridge("codex", &codex, "claude", &claude, &[]);
    assert!(output.contains("2 rollout files: 1 converted"), "{output}");
    assert_json_transcript(&claude, "claude");
}

#[test]
fn rerun_leaves_native_conversation_and_its_summary_alone() {
    let scratch = Scratch::new();
    let codex = scratch.path("codex");
    let agy = scratch.path("agy");
    codex_fixture(&codex, false, true);
    bridge("codex", &codex, "agy", &agy, &[]);
    sqlite(&agy_db(&agy), "PRAGMA application_id=0;");
    let db = summary(&agy);
    sqlite(
        &db,
        &format!(
            "UPDATE conversation_summaries SET title='<environment_context>',preview='Native content' \
             WHERE conversation_id='{ID}';"
        ),
    );
    let before = (fs::read(agy_db(&agy)).unwrap(), fs::read(&db).unwrap());
    bridge("codex", &codex, "agy", &agy, &[]);
    assert_eq!(
        (fs::read(agy_db(&agy)).unwrap(), fs::read(&db).unwrap()),
        before,
        "a native conversation with the same ID was modified"
    );
}

#[test]
fn agy_title_search_finds_repaired_title_and_keeps_body_search() {
    let scratch = Scratch::new();
    let codex = scratch.path("codex");
    let agy = scratch.path("agy");
    codex_fixture(&codex, true, true);
    bridge("codex", &codex, "agy", &agy, &[]);
    let db = summary(&agy);
    let stale = "Obsolete fixture title";
    sqlite(
        &db,
        &format!(
            "CREATE VIRTUAL TABLE conversations_fts USING fts5(\
                conversation_id UNINDEXED, chunk_index UNINDEXED, title, user_body\
             ); \
             INSERT INTO conversations_fts(conversation_id,chunk_index,title) VALUES ('{ID}',-1,'{stale}'); \
             INSERT INTO conversations_fts(conversation_id,chunk_index,user_body) VALUES ('{ID}',0,'findable body'); \
             UPDATE conversation_summaries SET title='',preview='' WHERE conversation_id='{ID}';"
        ),
    );
    let matches = |query| {
        sqlite(
            &db,
            &format!(
                "SELECT count(*) FROM conversations_fts WHERE conversation_id='{ID}' \
                 AND conversations_fts MATCH '{query}';"
            ),
        )
    };
    assert_eq!(matches("title:obsolete"), "1\n");
    assert_eq!(matches("title:integration"), "0\n");
    assert_eq!(matches("user_body:findable"), "1\n");

    bridge("codex", &codex, "agy", &agy, &[]);
    assert_eq!(agy_title_and_preview(&agy), format!("{TITLE}\t{PREVIEW}"));
    assert_eq!(matches("title:obsolete"), "0\n");
    assert_eq!(matches("title:integration"), "1\n");
    assert_eq!(matches("user_body:findable"), "1\n");
}

#[test]
fn codex_internal_threads_require_opt_in() {
    let scratch = Scratch::new();
    let codex = scratch.path("codex");
    let claude = scratch.path("claude");
    codex_fixture(&codex, false, false);

    let guardian = "82f06ba2-26cc-45bc-b811-782ba89291d8";
    let subagent = "82f06ba2-26cc-45bc-b811-782ba89291d9";
    let indexed = "82f06ba2-26cc-45bc-b811-782ba89291da";
    for (id, meta) in [
        (guardian, r#", "thread_source":"guardian_review""#),
        (subagent, r#", "source":{"subagent":"review"}"#),
        (indexed, ""),
    ] {
        let header = format!(
            r#"{{"timestamp":"{START}","type":"session_meta","payload":{{"id":"{id}","timestamp":"{START}","cwd":"{WORKSPACE}"{meta}}}}}"#
        );
        write(
            codex.join(format!("sessions/rollout-2026-01-01T10-00-00-{id}.jsonl")),
            format!("{header}\n{}\n", codex_record("user", START, USER_JSON)),
        );
    }
    sqlite(
        &codex.join("state_5.sqlite"),
        &format!(
            "CREATE TABLE threads(id TEXT, thread_source TEXT); INSERT INTO threads VALUES('{indexed}','subagent');"
        ),
    );

    let projected = bridge("codex", &codex, "claude", &claude, &["--dry-run"]);
    assert!(projected.contains("1 converted") && projected.contains("3 internal skipped"));
    assert!(!claude.exists());

    let first = bridge("codex", &codex, "claude", &claude, &[]);
    assert!(first.contains("1 converted") && first.contains("3 internal skipped"));
    let second = bridge("codex", &codex, "claude", &claude, &[]);
    assert!(second.contains("0 converted") && second.contains("3 internal skipped"));
    let opt_in = bridge("codex", &codex, "claude", &claude, &["--include-internal"]);
    assert!(opt_in.contains("3 converted") && opt_in.contains("0 internal skipped"));
}

#[test]
fn continued_internal_thread_is_skipped_but_a_distinct_fork_is_kept() {
    let scratch = Scratch::new();
    let codex = scratch.path("codex");
    let claude = scratch.path("claude");
    let continuation = "82f06ba2-26cc-45bc-b811-782ba89291e0";
    let fork = "82f06ba2-26cc-45bc-b811-782ba89291e1";
    let first = format!(
        "{}\n{}\n",
        format_args!(
            r#"{{"timestamp":"{START}","type":"session_meta","payload":{{"id":"{ID}","cwd":"{WORKSPACE}","thread_source":"guardian_review"}}}}"#
        ),
        codex_record("user", START, USER_JSON)
    );
    write(
        codex.join(format!("sessions/rollout-2026-01-01T10-00-00-{ID}.jsonl")),
        &first,
    );
    for (id, segment) in [(ID, continuation), (fork, fork)] {
        let header = format!(
            r#"{{"timestamp":"{END}","type":"session_meta","payload":{{"id":"{id}","cwd":"{WORKSPACE}","history_base":{{"thread_id":"{ID}","end_byte_offset":{}}}}}}}"#,
            first.len()
        );
        write(
            codex.join(format!(
                "sessions/rollout-2026-01-01T10-00-01-{segment}.jsonl"
            )),
            format!("{header}\n{}\n", codex_record("assistant", END, REPLY_JSON)),
        );
    }
    let result = bridge("codex", &codex, "claude", &claude, &[]);
    assert!(
        result.contains("1 converted") && result.contains("1 internal skipped"),
        "{result}"
    );
    let result = bridge("codex", &codex, "claude", &claude, &["--include-internal"]);
    assert!(
        result.contains("1 converted") && result.contains("1 already present"),
        "{result}"
    );
}

#[test]
fn antigravity_accepts_offsets_and_normalizes_index_and_transcript_times() {
    let scratch = Scratch::new();
    let claude = scratch.path("claude");
    let agy = scratch.path("agy");
    let roundtrip = scratch.path("roundtrip");
    claude_fixture(&claude);
    let path = transcript(&claude);
    let input = fs::read_to_string(&path).unwrap();
    fs::write(
        &path,
        input
            .replace(START, "2026-01-01T10:00:00.123456789+00:00")
            .replace(END, "2026-01-01T04:30:01.987654321-05:30"),
    )
    .unwrap();

    let result = bridge("claude", &claude, "agy", &agy, &["--dry-run"]);
    assert!(result.contains("1 converted") && !agy.exists(), "{result}");
    bridge("claude", &claude, "agy", &agy, &[]);
    assert_eq!(
        sqlite(
            &summary(&agy),
            "SELECT last_modified_time,last_user_input_time FROM conversation_summaries;"
        ),
        "2026-01-01 10:00:01.987654321+00:00\t2026-01-01 10:00:00.123456789+00:00\n"
    );
    bridge("agy", &agy, "claude", &roundtrip, &[]);
    let converted = fs::read_to_string(transcript(&roundtrip)).unwrap();
    assert!(converted.contains(START) && converted.contains(END));
}

#[test]
fn invalid_timestamp_fails_both_dry_run_and_import_before_writing() {
    let scratch = Scratch::new();
    let claude = scratch.path("claude");
    let agy = scratch.path("agy");
    write(
        claude.join(format!("projects/fixture/{ID}.jsonl")),
        format!(
            r#"{{"type":"user","timestamp":"2026-01-01T10:00:00+24:00","cwd":"{WORKSPACE}","message":{{"content":"Invalid time fixture"}}}}"#
        ),
    );
    for flags in [&[] as &[&str], &["--dry-run"]] {
        let result = output(
            Command::new(env!("CARGO_BIN_EXE_session-bridge"))
                .args(["--from", "claude", "--to", "agy", "--claude-dir"])
                .arg(&claude)
                .arg("--agy-dir")
                .arg(&agy)
                .args(flags),
        );
        assert!(!result.status.success() && !agy.exists());
    }
}
