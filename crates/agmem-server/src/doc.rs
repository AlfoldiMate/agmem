//! `agmem doc` — the document tier from the shell (#135).
//!
//! Subagents have a shell and not always the MCP tools, and a plan handed
//! around by id is a plan nothing has to `cat`. So the four document verbs
//! are one-shots on the `agmem context` pattern (`oneshot.rs`): attach to the
//! shared daemon where a session would, else open the store here — never a
//! second writer beside a daemon that owns the store — and call the tool a
//! session would have called: `remember` for `put`, `inspect` for `get` and
//! `list`, `forget` for `forget`.
//!
//! Every verb goes through the tool's own JSON answer, on both routes
//! (`oneshot::call`, shared with `agmem consolidate` and `agmem forget`), so
//! what the shell prints is what an MCP client would have seen: the same
//! refusals, the same window, the same versions. The rendering is the only
//! thing added here.

use agmem_core::{EpisodeId, SpaceName};
use anyhow::{Context as _, bail};
use serde_json::{Value, json};

use crate::config::{Config, DocArgs, DocForgetArgs, DocGetArgs, DocListArgs, DocPutArgs, DocVerb};
use crate::oneshot::{call, pretty};
use crate::resources;
use crate::tools::remember::MAX_EPISODE_CHARS;

/// Run one verb, print its answer, exit — the whole subcommand.
///
/// stdout is the answer here, not the MCP wire: one-shot mode is the single
/// place outside the transport where the crate-wide deny is lifted.
///
/// # Errors
/// When no route to the store works, stdin cannot be read, or the tool
/// refuses the parameters.
#[allow(clippy::print_stdout)]
pub async fn run(cfg: Config, args: DocArgs) -> anyhow::Result<()> {
    let answer = match args.verb {
        DocVerb::Put(put) => {
            let content = std::io::read_to_string(std::io::stdin())
                .context("reading the document on stdin")?;
            self::put(&cfg, put, content).await?
        }
        DocVerb::Get(get) => self::get(&cfg, get).await?,
        DocVerb::List(list) => self::list(&cfg, list).await?,
        DocVerb::Forget(forget) => self::forget(&cfg, forget).await?,
    };
    // The verbs end their own lines: raw content is printed as stored.
    print!("{answer}");
    Ok(())
}

/// Store `content` as a document; the answer is `<id> <uri>` on one line.
///
/// # Errors
/// When the tool refuses — an empty or oversize document, a transcript in
/// the `user` space — or no route to the store works.
pub async fn put(cfg: &Config, args: DocPutArgs, content: String) -> anyhow::Result<String> {
    let DocPutArgs {
        title,
        kind,
        tags,
        mime,
        space,
    } = args;
    let arguments = json!({
        "space": space,
        "memories": [],
        "episode": {
            "content": content,
            "title": title,
            "doc_kind": kind,
            "tags": tags,
            "mime": mime,
        }
    });
    let answer = call(cfg, "remember", arguments).await?;
    let id = answer["episode"]
        .as_str()
        .context("remember answered without an episode id; agmem versions disagree?")?;
    // The tool resolved the space before writing, so a name that reached
    // here is one the store holds; `current` is this run's own.
    let space = written_space(cfg, space.as_deref())?;
    Ok(format!(
        "{id} {uri}\n",
        uri = resources::document_uri(space.as_str(), id)
    ))
}

/// Read a document by id or title: the `inspect` JSON, or the content alone.
///
/// # Errors
/// When nothing answers to the reference, it names something that is not a
/// document (under `--raw`), or no route to the store works.
pub async fn get(cfg: &Config, args: DocGetArgs) -> anyhow::Result<String> {
    let DocGetArgs {
        reference,
        offset,
        limit,
        raw,
        space,
    } = args;
    // An id is tried first: a title that happens to be a ULID is a title
    // nobody would choose, and `--help` says which wins.
    let reference = if reference.parse::<EpisodeId>().is_ok() {
        format!("episode:{reference}")
    } else {
        format!(
            "doc:{space}/{reference}",
            space = space.as_deref().unwrap_or("current")
        )
    };
    // The tool's default window on a document is one chunk — right for an
    // agent paging, wrong for a shell that asked for the file. No limit
    // here means the whole document, which the write cap bounds.
    let arguments = json!({
        "ref": reference,
        "space": space,
        "offset": offset,
        "limit": limit.or(Some(MAX_EPISODE_CHARS)),
    });
    let answer = call(cfg, "inspect", arguments).await?;
    if !raw {
        return pretty(&answer);
    }
    let Some(content) = answer["found"]["episode"]["content"].as_str() else {
        bail!("{reference} is not a document; `agmem doc get` without --raw shows what it is");
    };
    Ok(content.to_owned())
}

/// List a space's documents, newest first — one line each, or the JSON.
///
/// # Errors
/// When the space is unknown or no route to the store works.
pub async fn list(cfg: &Config, args: DocListArgs) -> anyhow::Result<String> {
    let DocListArgs {
        kinds,
        tags,
        space,
        json: as_json,
    } = args;
    let arguments = json!({
        "ref": format!("docs:{}", space.as_deref().unwrap_or("current")),
        "doc_kinds": kinds,
        "tags": tags,
    });
    let answer = call(cfg, "inspect", arguments).await?;
    if as_json {
        return pretty(&answer);
    }
    let documents = answer["found"]["documents"]
        .as_array()
        .context("inspect answered without a listing; agmem versions disagree?")?;
    Ok(documents.iter().map(line).collect())
}

/// One listing line: id, kind, size, citations, date, title — the id first
/// so a `cut -d' ' -f1` gets what `get` and `forget` take.
fn line(document: &Value) -> String {
    let field = |name: &str| document[name].as_str().unwrap_or("?").to_owned();
    format!(
        "{id}  {kind:<10}  {chars:>7} chars  cited {cited:<3}  {created}  {title}\n",
        id = field("id"),
        kind = field("doc_kind"),
        chars = document["chars"].as_u64().unwrap_or(0),
        cited = document["cited"].as_u64().unwrap_or(0),
        created = field("created_at"),
        title = field("title"),
    )
}

/// Forget a document — close it, or purge it and its slices.
///
/// # Errors
/// When the id names nothing, a purge is refused because live claims cite
/// the document and `--cascade` was not given, or no route to the store
/// works.
pub async fn forget(cfg: &Config, args: DocForgetArgs) -> anyhow::Result<String> {
    let DocForgetArgs {
        id,
        purge,
        cascade,
        space,
    } = args;
    let arguments = json!({
        "ids": [format!("episode:{id}")],
        "space": space,
        "purge": purge,
        "cascade": cascade,
    });
    let answer = call(cfg, "forget", arguments).await?;
    let names = |key: &str| -> Vec<String> {
        answer[key]
            .as_array()
            .map(|ids| {
                ids.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_owned)
                    .collect()
            })
            .unwrap_or_default()
    };
    let purged = names("purged");
    let invalidated = names("invalidated");
    Ok(if purge {
        format!(
            "purged {count} record(s), {chunks} chunk(s): {ids}\n",
            count = purged.len(),
            chunks = answer["chunks_purged"].as_u64().unwrap_or(0),
            ids = purged.join(", ")
        )
    } else {
        format!(
            "forgot {count} record(s): {ids}\n",
            count = invalidated.len(),
            ids = invalidated.join(", ")
        )
    })
}

/// The space a `put` landed in, for the URI: this run's own for `current`
/// or nothing, else the name given.
fn written_space(cfg: &Config, space: Option<&str>) -> anyhow::Result<SpaceName> {
    match space {
        None | Some("current") => Ok(cfg.space.clone()),
        Some(name) => name
            .parse()
            .with_context(|| format!("`{name}` is not a space name")),
    }
}
