//! Command-line Substack profile resolution and publication archiving.

use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use archivindex_substack_scraper::resolve::{Config as ResolveConfig, Handle, Resolver};
use archivindex_substack_scraper::{
    CaptureKind, Config, Downloader, Error, ResumeState, read_archive,
};
use clap::{Args, Parser, Subcommand};
use indicatif::{ProgressBar, ProgressStyle};

#[derive(Parser)]
#[command(
    version,
    about = "Resolve Substack users and archive publication posts"
)]
struct Options {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Look up a handle and print its numeric user ID and profile slug as id-slug.
    LookupHandle {
        /// Substack handle, with or without its leading @.
        handle: String,
        #[command(flatten)]
        options: ProfileOptions,
    },
    /// Resolve a numeric user ID to a handle, without its leading @.
    ResolveHandle(ResolveOptions),
    /// Resolve a numeric user ID to a profile slug, which can differ from the handle.
    ResolveSlug(ResolveOptions),
    /// Resolve a numeric user ID to its full public profile as one JSON object.
    ResolveUser(ResolveOptions),
    /// Download a publication's posts; optional credentials come from `SUBSTACK_COOKIE`.
    Archive(ArchiveOptions),
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

#[derive(Args)]
struct ArchiveOptions {
    #[command(flatten)]
    network: NetworkOptions,
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
}

#[derive(Args)]
struct ResolveOptions {
    /// Numeric Substack user ID.
    user_id: u64,
    #[command(flatten)]
    options: ProfileOptions,
}

#[derive(Args)]
struct ProfileOptions {
    #[command(flatten)]
    network: NetworkOptions,
    /// Retain lookup requests and retries in a new WARC file.
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// Minimum seconds between request starts, including retries.
    #[arg(long, default_value = "1.5")]
    delay: f64,
    /// Compress WARC records with gzip (also inferred from a .gz output suffix).
    #[arg(long, requires = "output")]
    gzip: bool,
}

#[derive(Args)]
struct NetworkOptions {
    /// SOCKS5 proxy URI (use socks5h://host:port for DNS through the proxy).
    #[arg(long, value_name = "URI")]
    proxy: Option<String>,
    /// Print the public IP from ipify to stderr before contacting Substack, using --proxy when set.
    /// A failed lookup stops the command.
    #[arg(long)]
    show_ip: bool,
}

impl ProfileOptions {
    fn resolver(&self) -> Result<Resolver, Error> {
        let mut resolver = Resolver::new(
            ResolveConfig {
                proxy: self.network.proxy.clone(),
                delay: parse_delay(self.delay)?,
                gzip: self.gzip,
                ..ResolveConfig::default()
            },
            "https://substack.com/",
        )?;
        if self.network.show_ip {
            let address = resolver.public_ip("https://api.ipify.org/")?;
            eprintln!("Public IP address: {address}");
        }
        Ok(resolver)
    }
}

fn parse_delay(seconds: f64) -> Result<Duration, Error> {
    Duration::try_from_secs_f64(seconds).map_err(|_| "delay must be finite and nonnegative".into())
}

#[derive(Clone, Copy)]
enum Resolution {
    Handle,
    Slug,
    User,
}

fn resolve(arguments: &ResolveOptions, resolution: Resolution) -> Result<(), Error> {
    let user_id = arguments.user_id;
    let options = &arguments.options;
    let mut resolver = options.resolver()?;
    match resolution {
        Resolution::Handle => match resolver.resolve_handle(user_id, options.output.as_deref())? {
            Some(Handle::Set(handle)) => writeln!(std::io::stdout().lock(), "{handle}")?,
            Some(Handle::Unset) => {
                eprintln!("User {user_id} exists but has no handle");
            }
            None => eprintln!("No user with id {user_id}"),
        },
        Resolution::Slug => match resolver.resolve_slug(user_id, options.output.as_deref())? {
            Some(slug) => writeln!(std::io::stdout().lock(), "{slug}")?,
            None => eprintln!("No slug for user id {user_id}"),
        },
        Resolution::User => match resolver.resolve_user(user_id, options.output.as_deref())? {
            Some(profile) => {
                let mut out = std::io::stdout().lock();
                serde_json::to_writer(&mut out, &profile)?;
                writeln!(out)?;
            }
            None => eprintln!("No profile for user id {user_id}"),
        },
    }
    Ok(())
}

impl ArchiveOptions {
    fn run(self) -> Result<(), Error> {
        let Self {
            network,
            publication,
            output,
            resume_from,
            delay,
            page_size,
            max_posts,
            gzip,
        } = self;
        let mut resume = ResumeState::default();
        for path in resume_from {
            resume.extend(read_archive(path)?.resume);
        }
        let cookie = match std::env::var("SUBSTACK_COOKIE") {
            Ok(cookie) => Some(cookie),
            Err(std::env::VarError::NotPresent) => None,
            Err(_) => return Err("SUBSTACK_COOKIE is not valid Unicode".into()),
        };
        let delay = parse_delay(delay)?;
        let downloader = Downloader::new(
            Config {
                proxy: network.proxy,
                cookie,
                delay,
                page_size,
                max_posts,
                gzip,
                ..Config::default()
            },
            &publication,
        )?;
        if network.show_ip {
            let address = downloader.public_ip("https://api.ipify.org/")?;
            eprintln!("Public IP address: {address}");
        }
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
        Ok(())
    }
}

fn run() -> Result<(), Error> {
    match Options::parse().command {
        Command::LookupHandle { handle, options } => {
            match options
                .resolver()?
                .lookup_handle(&handle, options.output.as_deref())?
            {
                Some(profile) => {
                    writeln!(std::io::stdout().lock(), "{}-{}", profile.id, profile.slug)?;
                }
                None => eprintln!("No profile for handle {handle}"),
            }
        }
        Command::ResolveHandle(options) => resolve(&options, Resolution::Handle)?,
        Command::ResolveSlug(options) => resolve(&options, Resolution::Slug)?,
        Command::ResolveUser(options) => resolve(&options, Resolution::User)?,
        Command::Archive(options) => options.run()?,
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
