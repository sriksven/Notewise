//! The Notewise command-line interface.
//!
//! Links the engine directly rather than talking to a running server, so scripting and
//! headless recording work with no desktop app present.

#![forbid(unsafe_code)]

mod config;
mod format;

use std::path::PathBuf;

use anyhow::{Context, Result};
use chrono::Utc;
use clap::{Parser, Subcommand};

use notewise_ai_router::{AiBackend, TranscriptInput};
use notewise_api_server::{AppState, Server};
use notewise_graph::{EdgeKind, Graph, NodeKind, NodeRef};
use notewise_mcp_server::McpServer;
use notewise_storage::{
    Database, Id, MeetingRepository, NewSummary, NoteRepository, SearchRepository,
    SummaryRepository,
};

use crate::config::Config;

#[derive(Debug, Parser)]
#[command(
    name = "notewise",
    about = "Local-first meeting intelligence",
    version
)]
struct Cli {
    /// Database file. Defaults to the platform data directory.
    #[arg(long, global = true)]
    db: Option<PathBuf>,

    /// Use a throwaway in-memory database. Nothing is persisted.
    #[arg(long, global = true)]
    ephemeral: bool,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Show engine status: database, schema version, and configured AI backend.
    Status,

    /// List recent meetings.
    Meetings {
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },

    /// Print a meeting's transcript.
    Transcript {
        id: String,
    },

    /// Summarize a meeting and store the result.
    Summarize {
        id: String,
    },

    /// Show everything connected to a meeting.
    Related {
        id: String,
        #[arg(long, default_value_t = 2)]
        depth: u32,
    },

    /// Search notes, tickets, and transcripts.
    Search {
        query: String,
        #[arg(long, default_value_t = 10)]
        limit: u32,
    },

    /// List recent notes.
    Notes {
        #[arg(long, default_value_t = 20)]
        limit: u32,
    },

    /// Run the local REST API server (loopback only).
    Serve {
        #[arg(long, default_value_t = Server::DEFAULT_PORT)]
        port: u16,
    },

    /// Run the MCP server on stdio, for agent clients.
    Mcp,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("NOTEWISE_LOG")
                .unwrap_or_else(|_| "notewise=info".into()),
        )
        .with_writer(std::io::stderr) // stdout is reserved for MCP's JSON-RPC stream
        .init();

    let cli = Cli::parse();
    let config = Config::resolve(cli.db.clone(), cli.ephemeral)?;

    match cli.command {
        Command::Status => status(&config),
        Command::Meetings { limit } => meetings(&config, limit),
        Command::Transcript { id } => transcript(&config, &id),
        Command::Summarize { id } => summarize(&config, &id).await,
        Command::Related { id, depth } => related(&config, &id, depth),
        Command::Search { query, limit } => search(&config, &query, limit),
        Command::Notes { limit } => notes(&config, limit),
        Command::Serve { port } => serve(&config, port).await,
        Command::Mcp => mcp(&config).await,
    }
}

fn open(config: &Config) -> Result<Database> {
    match &config.database {
        config::DatabaseLocation::Memory => {
            Database::open_in_memory().context("opening an in-memory database")
        }
        config::DatabaseLocation::File(path) => {
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("creating {}", parent.display()))?;
            }
            Database::open(path).with_context(|| format!("opening {}", path.display()))
        }
    }
}

fn parse_id(raw: &str) -> Result<Id> {
    raw.parse()
        .with_context(|| format!("'{raw}' is not a valid id"))
}

fn status(config: &Config) -> Result<()> {
    let db = open(config)?;
    let router = config.ai_router()?;

    println!("database       {}", config.database);
    println!("schema version {}", db.schema_version()?);
    println!("meetings       {}", MeetingRepository::new(&db).list_recent(u32::MAX)?.len());
    println!("ai backend     {}", router.model_id());
    println!(
        "ai location    {}",
        if router.is_local() {
            "local — transcripts stay on this machine"
        } else {
            "remote — transcripts are sent to the provider"
        }
    );
    Ok(())
}

fn meetings(config: &Config, limit: u32) -> Result<()> {
    let db = open(config)?;
    let meetings = MeetingRepository::new(&db).list_recent(limit)?;

    if meetings.is_empty() {
        println!("No meetings yet.");
        return Ok(());
    }

    for meeting in meetings {
        println!(
            "{}  {}  {}  {}",
            meeting.id,
            meeting.started_at.format("%Y-%m-%d %H:%M"),
            format::duration(meeting.duration_ms()),
            meeting.title,
        );
    }
    Ok(())
}

fn transcript(config: &Config, id: &str) -> Result<()> {
    let db = open(config)?;
    let repo = MeetingRepository::new(&db);
    let meeting = repo.get(parse_id(id)?)?;

    println!("# {}\n", meeting.title);
    let text = repo.transcript_text(meeting.id)?;
    if text.trim().is_empty() {
        println!("(no transcript)");
    } else {
        print!("{text}");
    }
    Ok(())
}

async fn summarize(config: &Config, id: &str) -> Result<()> {
    let meeting_id = parse_id(id)?;
    let db = open(config)?;
    let repo = MeetingRepository::new(&db);
    let meeting = repo.get(meeting_id)?;
    let text = repo.transcript_text(meeting_id)?;

    anyhow::ensure!(
        !text.trim().is_empty(),
        "meeting '{}' has no transcript to summarize",
        meeting.title
    );

    let router = config.ai_router()?;
    let input = TranscriptInput::new(meeting.title.clone(), text);

    let summary = router.summarize(&input).await?;
    let decisions = router.extract_decisions(&input).await?;
    let action_items = router.extract_action_items(&input).await?;

    let summaries = SummaryRepository::new(&db);
    let stored = summaries.create(NewSummary {
        meeting_id,
        text: summary.text.clone(),
        model: summary.model.clone(),
    })?;

    for decision in &decisions {
        summaries.add_decision(notewise_storage::NewDecision {
            summary_id: stored.id,
            text: decision.text.clone(),
            reasoning: decision.reasoning.clone(),
            decided_at: Some(Utc::now()),
        })?;
    }
    for item in &action_items {
        summaries.add_action_item(notewise_storage::NewActionItem {
            summary_id: stored.id,
            text: item.text.clone(),
            owner: item.owner.clone(),
            due_at: None,
        })?;
    }

    Graph::new(&db).connect(
        NodeRef::new(NodeKind::Summary, stored.id),
        EdgeKind::DerivedFrom,
        NodeRef::new(NodeKind::Meeting, meeting_id),
    )?;

    println!("# {}\n", meeting.title);
    println!("{}\n", summary.text);

    if !decisions.is_empty() {
        println!("## Decisions");
        for decision in &decisions {
            println!("- {}", decision.text);
        }
        println!();
    }

    if !action_items.is_empty() {
        println!("## Action items");
        for item in &action_items {
            println!("- {}", format::action_item(&item.text, item.owner.as_deref()));
        }
        println!();
    }

    println!("(model: {}, summary id: {})", summary.model, stored.id);
    Ok(())
}

fn related(config: &Config, id: &str, depth: u32) -> Result<()> {
    let db = open(config)?;
    let meeting_id = parse_id(id)?;
    let meeting = MeetingRepository::new(&db).get(meeting_id)?;

    let related = Graph::new(&db).related(NodeRef::new(NodeKind::Meeting, meeting_id), depth)?;

    println!("Related to '{}':", meeting.title);
    if related.is_empty() {
        println!("  (nothing linked yet)");
        return Ok(());
    }

    for node in related {
        println!(
            "  {:>2} hop  {:<20} {}  (via {})",
            node.distance, node.node.kind, node.node.id, node.via
        );
    }
    Ok(())
}

fn search(config: &Config, query: &str, limit: u32) -> Result<()> {
    let db = open(config)?;
    let hits = SearchRepository::new(&db).search(query, limit)?;

    if hits.is_empty() {
        println!("No matches for '{query}'.");
        return Ok(());
    }

    for hit in hits {
        println!("{:<20} {}", hit.entity_kind, hit.entity_id);
        println!("  {}", hit.snippet.replace('\n', " "));
    }
    Ok(())
}

fn notes(config: &Config, limit: u32) -> Result<()> {
    let db = open(config)?;
    let notes = NoteRepository::new(&db).list_recent(limit)?;

    if notes.is_empty() {
        println!("No notes yet.");
        return Ok(());
    }

    for note in notes {
        println!(
            "{}  {}  {}",
            note.id,
            note.updated_at.format("%Y-%m-%d %H:%M"),
            note.title
        );
    }
    Ok(())
}

async fn serve(config: &Config, port: u16) -> Result<()> {
    let server = Server::bind(format!("127.0.0.1:{port}"))?;
    let state = AppState::new(open(config)?, config.ai_router()?);

    println!("Notewise API on http://{}", server.addr());
    println!("Loopback only — not reachable from the network.");

    server.serve(state).await?;
    Ok(())
}

async fn mcp(config: &Config) -> Result<()> {
    // stdout carries the JSON-RPC stream; anything else printed there corrupts it.
    McpServer::new(open(config)?).serve_stdio().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        // Catches conflicting flags and malformed arg definitions at test time rather
        // than on the user's first run.
        Cli::command().debug_assert();
    }

    #[test]
    fn subcommands_parse() {
        let cli = Cli::try_parse_from(["notewise", "meetings", "--limit", "5"]).unwrap();
        assert!(matches!(cli.command, Command::Meetings { limit: 5 }));

        let cli = Cli::try_parse_from(["notewise", "search", "postgres"]).unwrap();
        assert!(matches!(cli.command, Command::Search { .. }));
    }

    #[test]
    fn global_flags_work_after_the_subcommand() {
        let cli = Cli::try_parse_from(["notewise", "status", "--ephemeral"]).unwrap();
        assert!(cli.ephemeral);
    }

    #[test]
    fn serve_defaults_to_the_engine_port() {
        let cli = Cli::try_parse_from(["notewise", "serve"]).unwrap();
        match cli.command {
            Command::Serve { port } => assert_eq!(port, Server::DEFAULT_PORT),
            other => panic!("expected serve, got {other:?}"),
        }
    }

    #[test]
    fn an_unknown_subcommand_is_rejected() {
        assert!(Cli::try_parse_from(["notewise", "obliterate"]).is_err());
    }

    #[test]
    fn transcript_requires_an_id() {
        assert!(Cli::try_parse_from(["notewise", "transcript"]).is_err());
    }

    #[test]
    fn malformed_ids_are_rejected_with_context() {
        let err = parse_id("not-a-uuid").expect_err("should be rejected");
        assert!(err.to_string().contains("not-a-uuid"));
    }
}
