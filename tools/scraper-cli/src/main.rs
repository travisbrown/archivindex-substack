//! Command-line capture, export, and inspection of Substack WARC archives.

use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use archivindex_substack_scraper::{
    CaptureKind, Config, Downloader, Error, ResumeState, read_archive,
};
use clap::{Parser, Subcommand};
use indicatif::{ProgressBar, ProgressStyle};

#[derive(Parser)]
#[command(version, about = "Capture Substack publication posts in WARC archives")]
struct Options {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Download a publication's posts; optional credentials come from `SUBSTACK_COOKIE`.
    Archive {
        /// Publication slug or full origin URL, including custom domains.
        #[arg(short, long)]
        publication: String,
        /// New WARC path. An existing file is never overwritten.
        #[arg(short, long)]
        output: PathBuf,
        /// Previous WARC segments. Repeat to supply all segments in the same capture session.
        #[arg(long)]
        resume_from: Vec<PathBuf>,
        /// Seconds between request starts.
        #[arg(long, default_value = "2")]
        delay: f64,
        /// Number of summaries requested per archive page (1 through 100).
        #[arg(long, default_value = "20")]
        page_size: usize,
        /// Stop after this many post lookups in this segment, excluding resumed posts and listings.
        #[arg(long)]
        max_posts: Option<usize>,
        /// Compress WARC records with gzip (also inferred from a .gz output suffix).
        #[arg(long)]
        gzip: bool,
    },
    /// Export valid post captures as JSON Lines, preserving every version and its provenance.
    Export {
        #[arg(required = true)]
        archives: Vec<PathBuf>,
        /// Also export captured archive listing pages.
        #[arg(long)]
        include_pages: bool,
    },
    /// Report capture counts, completed post URLs, and HTTP statuses for each archive.
    Inspect {
        #[arg(required = true)]
        archives: Vec<PathBuf>,
    },
}

fn run() -> Result<(), Error> {
    match Options::parse().command {
        Command::Archive {
            publication,
            output,
            resume_from,
            delay,
            page_size,
            max_posts,
            gzip,
        } => {
            let mut resume = ResumeState::default();
            for path in resume_from {
                resume.extend(read_archive(path)?.resume);
            }
            let cookie = match std::env::var("SUBSTACK_COOKIE") {
                Ok(cookie) => Some(cookie),
                Err(std::env::VarError::NotPresent) => None,
                Err(_) => return Err("SUBSTACK_COOKIE is not valid Unicode".into()),
            };
            let delay = Duration::try_from_secs_f64(delay)
                .map_err(|_| "delay must be finite and nonnegative")?;
            let downloader = Downloader::new(
                Config {
                    cookie,
                    delay,
                    page_size,
                    max_posts,
                    gzip: gzip || output.extension().is_some_and(|s| s == "gz"),
                    ..Config::default()
                },
                &publication,
            )?;
            let progress = ProgressBar::new_spinner().with_style(ProgressStyle::with_template(
                "{spinner:.green} [{elapsed_precise}] {pos} post lookups archived {msg}",
            )?);
            progress.enable_steady_tick(Duration::from_millis(100));
            let result = downloader.archive_with_progress(&resume, &output, |count| {
                progress.set_position(count as u64);
            });
            match result {
                Ok(count) => {
                    progress.finish_and_clear();
                    eprintln!("Archived {count} post lookups to {}", output.display());
                }
                Err(error) => {
                    progress.abandon_with_message("Stopped");
                    return Err(error);
                }
            }
        }
        Command::Export {
            archives,
            include_pages,
        } => {
            let mut out = std::io::stdout().lock();
            for path in archives {
                for capture in read_archive(path)?.captures {
                    if include_pages || capture.kind == CaptureKind::Post {
                        serde_json::to_writer(&mut out, &capture)?;
                        writeln!(out)?;
                    }
                }
            }
        }
        Command::Inspect { archives } => {
            let mut out = std::io::stdout().lock();
            for path in archives {
                let contents = read_archive(&path)?;
                serde_json::to_writer(
                    &mut out,
                    &serde_json::json!({
                        "archive": path,
                        "posts": contents.captures.iter().filter(|c| c.kind == CaptureKind::Post).count(),
                        "pages": contents.captures.iter().filter(|c| c.kind == CaptureKind::Archive).count(),
                        "completed_posts": contents.resume.completed_posts(),
                        "http_statuses": contents.status_counts,
                    }),
                )?;
                writeln!(out)?;
            }
        }
    }
    Ok(())
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
