# Token-spend analysis over Claude Code transcripts for this project.
# Usage:  nu token-analysis-2026-09-03.nu            (uses cache if present)
#         nu token-analysis-2026-09-03.nu --rebuild  (re-extracts from the jsonl)
# Writes the tables to token-analysis-2026-09-03.md next to this script.

const DIR = '/Users/Matthew/.claude/projects/-Users-Matthew-Development-agmem/'
# The 2026-09-03 run is the `token-analysis-2026-09-03` review document in
# the agmem store; a rerun writes to the temp dir, not the repo.
const OUT = '/tmp/token-analysis.md'
const CACHE = '/tmp/agmem-token-analysis.msgpackz'

# chars of a content field that may be a string, a list of blocks, or anything
def content-chars [c] {
  match ($c | describe -d | get type) {
    string => ($c | str length),
    list => ($c | each {|b|
      if ($b | describe -d | get type) == string { $b | str length } else if ($b.text? | is-not-empty) { $b.text | str length } else { $b | to json | str length }
    } | math sum),
    nothing => 0,
    _ => ($c | to json | str length),
  }
}

def user-text [c] {
  match ($c | describe -d | get type) {
    string => $c,
    list => ($c | where ($it | describe -d | get type) == record | where type == text | get -o text | default [] | str join "\n"),
    _ => '',
  }
}

def classify-user [t: string] {
  if ($t =~ '<command-name>') {
    let name = ($t | parse -r '<command-name>([^<]+)</command-name>' | get -o capture0.0 | default '?')
    $'slash:($name | str trim)'
  } else if ($t | str starts-with '/') { $'slash:($t | split row " " | first | lines | first)' } else if ($t =~ '<task-notification>') { 'task-notification' } else if ($t =~ '<system-reminder>') { 'system-reminder' } else if ($t =~ 'Memory context') { 'memory-context' } else if ($t =~ '<local-command-') { 'local-command' } else if ($t | is-empty) { 'empty' } else { 'plain' }
}

def bash-head [cmd: string] {
  $cmd
  | str replace -r '^\s*(cd\s+\S+\s*&&\s*)+' ''
  | str replace -r '^\s*rtk\s+' ''
  | str replace -r '^\s*(RUST_\w+=\S+\s+|\w+=\S+\s+)+' ''
  | str trim
  | split row -r '\s+' | first | default ''
  | str replace -r '^\((.*)' '$1'
}

def extract [f: string] {
  let is_sub = ($f | str contains '/subagents/')
  let session = if $is_sub { $f | path dirname | path dirname | path basename } else { $f | path basename | str replace '.jsonl' '' }
  let recs = (open --raw $f | lines | each {|l| try { $l | from json } })
  let base = {session: $session, is_sub: $is_sub, file: $f}

  let turns = ($recs | where type == assistant
    | where ($it.message?.id? | is-not-empty)
    | each {|r| $base | merge {
        kind: 'turn', ts: $r.timestamp?, model: ($r.message.model? | default '?'), msgid: $r.message.id,
        input: ($r.message.usage?.input_tokens? | default 0),
        cache_creation: ($r.message.usage?.cache_creation_input_tokens? | default 0),
        cache_read: ($r.message.usage?.cache_read_input_tokens? | default 0),
        output: ($r.message.usage?.output_tokens? | default 0),
      }}
    # streaming writes one record per content block; only the last carries the final output_tokens
    | group-by msgid --to-table | each {|g| $g.items | first | merge {
        input: ($g.items.input | math max), cache_creation: ($g.items.cache_creation | math max),
        cache_read: ($g.items.cache_read | math max), output: ($g.items.output | math max) }})

  let tool_uses = ($recs | where type == assistant
    | each {|r| $r.message?.content? | default [] | each {|b| if ($b | describe -d | get type) == record { $b | merge {ts: $r.timestamp?} } } }
    | flatten | where type == tool_use
    | each {|b|
        let i = ($b.input? | default {})
        let cmd = ($i.command? | default '')
        let snip = (if ($cmd | is-not-empty) { $cmd } else if ($i.file_path? | is-not-empty) { $i.file_path } else if ($i.prompt? | is-not-empty) { $i.prompt } else if ($i.input? | is-not-empty) { $i.input } else if ($i.query? | is-not-empty) { $i.query } else if ($i.pattern? | is-not-empty) { $i.pattern } else { $i | to json })
        $base | merge {
          kind: 'tool_use', ts: $b.ts, id: $b.id, name: $b.name,
          input_chars: ($i | to json | str length),
          snip: ($snip | str replace -a "\n" ' ' | str substring 0..100),
          bash_head: (if ($cmd | is-not-empty) { bash-head $cmd } else { '' }),
          skill: ($i.skill? | default ''),
          subagent_type: ($i.subagent_type? | default ''),
          agent_model: ($i.model? | default ''),
          prompt_len: ($i.prompt? | default '' | str length),
          has_limit: (($i.limit? | is-not-empty) or ($i.offset? | is-not-empty)),
        }})

  let user_recs = ($recs | where type == user)
  let tool_results = ($user_recs
    | each {|r| let c = $r.message?.content?; if ($c | describe -d | get type) == list { $c | each {|b| if ($b | describe -d | get type) == record { $b | merge {ts: $r.timestamp?} } } } else { [] } }
    | flatten | where type == tool_result
    | each {|b| $base | merge {kind: 'tool_result', ts: $b.ts, id: $b.tool_use_id, chars: (content-chars $b.content?),
        has_reminder: (($b.content? | to json) =~ 'system-reminder')}})

  let users = ($user_recs | each {|r|
    let t = (user-text $r.message?.content?)
    let cls = (if ($r.isCompactSummary? | default false) { 'compact-summary' } else { classify-user $t })
    if ($t | is-not-empty) or ($cls != 'empty') {
      $base | merge {kind: 'user', ts: $r.timestamp?, uclass: $cls, chars: ($t | str length), is_meta: ($r.isMeta? | default false),
        ref_id: ($t | parse -r '<tool-use-id>([^<]+)</tool-use-id>' | get -o capture0.0 | default '')}
    }})

  let atts = ($recs | where type == attachment | each {|r|
    let a = $r.attachment
    let chars = (match $a.type {
      hook_success => ($a.stdout? | default '' | str length),
      hook_additional_context => (content-chars $a.content?),
      _ => ($a | to json | str length),
    })
    $base | merge {kind: 'attachment', ts: $r.timestamp?, atype: $a.type, hook: ($a.hookName? | default ''), chars: $chars}})

  let systems = ($recs | where type == system | each {|r| $base | merge {kind: 'system', ts: $r.timestamp?, subtype: ($r.subtype? | default ''), chars: (content-chars $r.content?)}})

  [$turns $tool_uses $tool_results $users $atts $systems] | flatten
}

def build [] {
  let files = (glob ($DIR + '**/*.jsonl'))
  print $'extracting ($files | length) files'
  let rows = ($files | par-each {|f| extract $f } | flatten)
  $rows | to msgpackz | save -f $CACHE
  $rows
}

def md-table [t] { $t | to md --pretty }

def main [--rebuild] {
  let rows = (if $rebuild or not ($CACHE | path exists) { build } else { open $CACHE | from msgpackz })
  let turns = ($rows | where kind == 'turn')
  let uses = ($rows | where kind == 'tool_use')
  let results = ($rows | where kind == 'tool_result')
  let users = ($rows | where kind == 'user')
  let atts = ($rows | where kind == 'attachment')
  let systems = ($rows | where kind == 'system')

  # ---- 1. totals -----------------------------------------------------------
  def tot [t] {
    { sessions: ($t | get session | uniq | length), files: ($t | get file | uniq | length), turns: ($t | length),
      input: ($t.input | math sum), cache_creation: ($t.cache_creation | math sum), cache_read: ($t.cache_read | math sum), output: ($t.output | math sum) }
  }
  let totals = ([
    ({scope: 'all'} | merge (tot $turns)),
    ({scope: 'main'} | merge (tot ($turns | where not is_sub))),
    ({scope: 'subagent'} | merge (tot ($turns | where is_sub))),
  ])
  let per_model = ($turns | group-by model --to-table | each {|g| {model: $g.model, scope: 'all'} | merge (tot $g.items | reject sessions files)}
    | append ($turns | group-by {|r| $'($r.model)|($r.is_sub)'} --to-table | each {|g| {model: ($g.items.0.model), scope: (if $g.items.0.is_sub { 'subagent' } else { 'main' })} | merge (tot $g.items | reject sessions files)})
    | sort-by scope model)

  # ---- 2. per session --------------------------------------------------------
  let joined = ($uses | join ($results | select id chars has_reminder) id --left | default 0 chars)
  let per_session = ($turns | group-by session --to-table | each {|g|
    let s = $g.session
    let su = ($uses | where session == $s)
    { session: ($s | str substring 0..8), date: ($g.items | get ts | sort | first | default '' | str substring 0..10),
      turns: ($g.items | length), main_turns: ($g.items | where not is_sub | length),
      cache_read: ($g.items.cache_read | math sum), cache_creation: ($g.items.cache_creation | math sum), output: ($g.items.output | math sum),
      total: (($g.items.cache_read | math sum) + ($g.items.cache_creation | math sum) + ($g.items.output | math sum) + ($g.items.input | math sum)),
      tool_calls: ($su | length), agents: ($su | where name == 'Agent' | length),
      compacted: ((($users | where session == $s and uclass == 'compact-summary' | length) > 0) or (($systems | where session == $s and subtype == 'compact_boundary' | length) > 0)),
    }} | sort-by total -r)

  # ---- 3. tool usage ---------------------------------------------------------
  def tool-table [j] { $j | group-by name --to-table | each {|g| {tool: $g.name, calls: ($g.items | length), result_chars: ($g.items.chars | math sum), avg_chars: (($g.items.chars | math avg) | math round), input_chars: ($g.items.input_chars | math sum)}} | sort-by result_chars -r }
  let tools_all = (tool-table $joined)
  let tools_main = (tool-table ($joined | where not is_sub))
  let tools_sub = (tool-table ($joined | where is_sub))
  let result_total = ($joined.chars | math sum)

  # ---- 4. agent dispatches ---------------------------------------------------
  # background agents return a ~1k "launched" stub as the tool_result; the real report arrives later as a
  # <task-notification> user message carrying the Agent tool_use id — join that in as `avg_notification`
  let notes = ($users | where uclass == 'task-notification' and ($it.ref_id | is-not-empty) | select ref_id chars | rename id note_chars)
  let agents = ($joined | where name == 'Agent' | join $notes id --left | default 0 note_chars | group-by {|r| $'($r.subagent_type)|($r.agent_model)'} --to-table | each {|g|
    {subagent_type: ($g.items.0.subagent_type | default '' | if ($in | is-empty) { '(none)' } else { $in }), model: ($g.items.0.agent_model | if ($in | is-empty) { '(default)' } else { $in }), count: ($g.items | length), avg_prompt: ($g.items.prompt_len | math avg | math round), avg_result_stub: ($g.items.chars | math avg | math round), notified: ($g.items | where note_chars > 0 | length), avg_notification: ($g.items | where note_chars > 0 | get note_chars | append 0 | math avg | math round), max_notification: ($g.items.note_chars | math max)}} | sort-by count -r)

  # ---- 5. bash -----------------------------------------------------------------
  let bash = ($joined | where name == 'Bash')
  let bash_by_count = ($bash | group-by bash_head --to-table | each {|g| {cmd: $g.bash_head, count: ($g.items | length), result_chars: ($g.items.chars | math sum), avg: ($g.items.chars | math avg | math round), over_5k: ($g.items | where chars > 5000 | length)}})
  let bash_top_count = ($bash_by_count | sort-by count -r | first 25)
  let bash_top_chars = ($bash_by_count | sort-by result_chars -r | first 25)
  let bash_big = ($bash | where chars > 5000 | sort-by chars -r | select session bash_head chars snip | update session { str substring 0..8 })

  # ---- 6. slash / skills -----------------------------------------------------
  let skills = ($joined | where name == 'Skill' | group-by skill --to-table | each {|g| {skill: $g.skill, count: ($g.items | length), result_chars: ($g.items.chars | math sum)}} | sort-by count -r)
  let slash = ($users | where ($it.uclass | str starts-with 'slash:') | group-by uclass --to-table | each {|g| {command: ($g.uclass | str replace 'slash:' ''), count: ($g.items | length), main: ($g.items | where not is_sub | length)}} | sort-by count -r)

  # ---- 7. mcp -----------------------------------------------------------------
  let mcp = ($tools_all | where ($it.tool | str starts-with 'mcp__'))

  # ---- 8. injected context ---------------------------------------------------
  let inj_users = ($users | where {|r| ($r.uclass in ['system-reminder' 'memory-context' 'task-notification' 'local-command' 'compact-summary']) or $r.is_meta }
    | group-by {|r| if $r.is_meta and $r.uclass == 'plain' { 'meta' } else { $r.uclass }} --to-table | each {|g| {kind: $'user:($g.closure_0)', count: ($g.items | length), chars: ($g.items.chars | math sum)}})
  let inj_atts = ($atts | group-by {|r| if ($r.hook | is-not-empty) { $'($r.atype):($r.hook)' } else { $r.atype }} --to-table | each {|g| {kind: $'attachment:($g.closure_0)', count: ($g.items | length), chars: ($g.items.chars | math sum)}})
  let inj_results = ([{kind: 'tool_result containing <system-reminder>', count: ($results | where has_reminder | length), chars: ($results | where has_reminder | get chars | append 0 | math sum)}])
  let injected = ($inj_users | append $inj_atts | append $inj_results | sort-by chars -r)
  let compaction = {compact_summaries: ($users | where uclass == 'compact-summary' | length), chars: ($users | where uclass == 'compact-summary' | get chars | append 0 | math sum), compact_boundaries: ($systems | where subtype == 'compact_boundary' | length)}

  # ---- 9. largest results ----------------------------------------------------
  let largest = ($joined | sort-by chars -r | first 15 | select name chars session is_sub snip | update session { str substring 0..8 })

  # ---- 10. Read ----------------------------------------------------------------
  let reads = ($joined | where name == 'Read')
  let read_tbl = ($reads | group-by {|r| if $r.has_limit { 'with limit/offset' } else { 'no limit/offset' }} --to-table | each {|g| {reads: $g.closure_0, count: ($g.items | length), chars: ($g.items.chars | math sum), avg: ($g.items.chars | math avg | math round), pct_of_all_result_chars: ((($g.items.chars | math sum) * 100 / $result_total) | math round -p 1)}})

  let md = ([
    '# Token analysis — agmem project transcripts — 2026-09-03',
    $'Source: `($DIR)` — ($rows | get file | uniq | length) transcript files, ($turns | get session | uniq | length) sessions.',
    '', '## 1. Totals', (md-table $totals), '', '### Per model', (md-table $per_model),
    '', '## 2. Per session (top 15 by total tokens)', (md-table ($per_session | first 15)),
    '', '## 3. Tool usage (all threads, by result chars)', $'Total tool_result chars: ($result_total)', (md-table $tools_all),
    '', '### Main thread only', (md-table $tools_main), '', '### Subagents only', (md-table $tools_sub),
    '', '## 4. Agent dispatches', (md-table $agents),
    '', '## 5. Bash commands', '### Top 25 by count', (md-table $bash_top_count), '', '### Top 25 by result chars', (md-table $bash_top_chars), '', $'### Commands returning > 5,000 chars \(($bash_big | length)\)', (md-table $bash_big),
    '', '## 6. Skills and slash commands', '### Skill tool', (md-table $skills), '', '### User slash commands', (md-table $slash),
    '', '## 7. MCP tools', (md-table $mcp),
    '', '## 8. Injected context', (md-table $injected), '', $'Compaction: ($compaction | to nuon)',
    '', '## 9. Largest single tool_results', (md-table $largest),
    '', '## 10. Read tool', (md-table $read_tbl),
  ] | str join "\n")
  $md | save -f $OUT
  { totals: $totals, per_model: $per_model, per_session: ($per_session | first 15), tools: ($tools_all | first 12), tools_main: ($tools_main | first 8), tools_sub: ($tools_sub | first 8), agents: $agents, bash_count: ($bash_top_count | first 10), bash_chars: ($bash_top_chars | first 10), bash_big_n: ($bash_big | length), skills: $skills, slash: $slash, mcp: $mcp, injected: $injected, compaction: $compaction, largest: $largest, reads: $read_tbl, result_total: $result_total }
}
