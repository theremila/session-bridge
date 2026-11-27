if .type == "custom-title" and (.customTitle | type) == "string" and (.customTitle | length) > 0 then
  ["title", "", "", .customTitle] | @tsv
elif .type == "summary" and (.summary | type) == "string" and (.summary | length) > 0 then
  ["summary", "", "", .summary] | @tsv
elif (.type == "user" or .type == "assistant") and .isSidechain != true and .isMeta != true then
  . as $entry | .message.content
  | if type == "string" then .
    elif type == "array" then
      [ .[] | select(.type == "text") | .text | select(type == "string") ] | join("\n")
    else "" end
  | select(length > 0)
  | [$entry.type, ($entry.timestamp // ""), ($entry.cwd // ""), .]
  | @tsv
else
  empty
end
