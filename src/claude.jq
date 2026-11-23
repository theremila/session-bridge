select((.type == "user" or .type == "assistant") and .isSidechain != true and .isMeta != true)
| . as $entry | .message.content
| if type == "string" then .
  elif type == "array" then
    [ .[] | select(.type == "text") | .text | select(type == "string") ] | join("\n")
  else "" end
| select(length > 0)
| [$entry.type, ($entry.timestamp // ""), ($entry.cwd // ""), .]
| @tsv
