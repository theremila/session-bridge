CREATE TABLE IF NOT EXISTS conversation_summaries (
    conversation_id TEXT PRIMARY KEY, title TEXT NOT NULL DEFAULT '', preview TEXT NOT NULL DEFAULT '',
    step_count INTEGER NOT NULL DEFAULT 0, last_modified_time DATETIME NOT NULL, workspace_uris TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT '', source TEXT NOT NULL DEFAULT '', project_id TEXT NOT NULL DEFAULT '',
    agent_name TEXT NOT NULL DEFAULT '', parent_conversation_id TEXT NOT NULL DEFAULT '',
    nesting_depth INTEGER NOT NULL DEFAULT 0, battle_id TEXT NOT NULL DEFAULT '',
    winning_conversation_id TEXT NOT NULL DEFAULT '', not_fully_idle NUMERIC NOT NULL DEFAULT false,
    killed NUMERIC NOT NULL DEFAULT false, last_user_input_time DATETIME NOT NULL,
    last_user_input_step_index INTEGER NOT NULL DEFAULT -1, app_data_dir TEXT NOT NULL DEFAULT '',
    raw_summary BLOB, group_id TEXT NOT NULL DEFAULT ''
);
