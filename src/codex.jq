def frame($values): $values | @tsv;
def injected_context:
  test("^\\s*(# AGENTS\\.md instructions(\\s|$)|<(environment_context|external_codex_apps_open_page|codex_internal_context|guardian_tool_descriptions)([ >]|$))");

def record:
if .type == "session_meta" then
  . as $record | .payload as $p |
  frame(["meta", $p.id, ($p.timestamp // $record.timestamp), ($p.cwd // ""),
    ($p.history_base.thread_id // ""), ($p.history_base.end_byte_offset // 0),
    ([$p.thread_source, $p.source] | any(. == "subagent" or . == "guardian_review" or
      (type == "object" and has("subagent"))))])
elif .type == "response_item" and .payload.type == "message" then
  . as $record | .payload as $p |
  select($p.role == "user" or $p.role == "assistant") |
  [$p.content[]? | select(.type == "input_text" or .type == "output_text") |
    .text | select(type == "string") | select($p.role != "user" or (injected_context | not))] | join("\n") |
  select(length > 0) |
  frame([$p.role, $record.timestamp, .])
elif .type == "realtime_item" and .payload.type == "transcript_segment" then
  . as $record | .payload |
  select((.role == "user" or .role == "assistant") and (.text | type == "string" and length > 0)) |
  frame([.role, $record.timestamp, .text])
else empty end;

# Codex emits these discriminators as plain JSON strings. Skip decoding the
# large tool outputs and telemetry records while still counting their bytes.
foreach inputs as $line (
  0;
  . + (($line | utf8bytelength) + 1);
  . as $end |
  select($line | contains("\"session_meta\"") or contains("\"transcript_segment\"")
    or (contains("\"response_item\"") and contains("\"message\""))) |
  $line | fromjson | record | "\($end)\t\(.)"
)
