use anyhow::{Context, Result, bail, ensure};
use clap::{Args, Parser, Subcommand};
use hdcontext::workflow::{analyze_one, discover};
use hdcontext::{
    ai::Analyzer,
    provider::{Provider, configured_audio_model},
    report::{self, Failure, Report},
};
use std::path::{Path, PathBuf};

#[derive(Parser)]
#[command(
    version,
    about = "Find and preview moments worth keeping in game recordings",
    long_about = "Open the desktop app with no arguments, or use the CLI commands to scan game recordings for highlights. Analysis sends media to the selected AI provider and can incur API charges."
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Open the desktop app to preview and keep gaming moments.
    Gui {
        /// Select Gemini's free-tier model. Requires a Gemini key from a Free Tier project.
        #[arg(long)]
        free_tier: bool,
    },
    /// Find short gaming highlights and save their time ranges and labels.
    Analyze(AnalyzeArgs),
    /// Watch configured folders and cache highlights without opening a GUI.
    Watch {
        #[arg(long)]
        config: Option<PathBuf>,
        #[arg(long, hide = true)]
        service: bool,
    },
    /// Preview selected renames from a saved report. Add --yes to apply them.
    Apply(ApplyArgs),
    /// Check FFmpeg, the API key environment variable, and local requirements.
    Doctor {
        #[arg(long, value_enum, env = "HDCONTEXT_PROVIDER", default_value = "gemini")]
        provider: Provider,
    },
}

#[derive(Args)]
struct AnalyzeArgs {
    /// Video files or folders containing video files.
    #[arg(required = true)]
    paths: Vec<PathBuf>,
    /// Include subfolders.
    #[arg(short, long)]
    recursive: bool,
    /// New JSON report path. Existing files are never overwritten.
    #[arg(short, long, default_value = "hdcontext-report.json")]
    output: PathBuf,
    /// Extra context, for example a game name. Sent to the selected provider.
    #[arg(long)]
    hint: Option<String>,
    #[arg(long, value_enum, env = "HDCONTEXT_PROVIDER", default_value = "gemini")]
    provider: Provider,
    /// Model ID. Defaults to the selected provider's model or model environment variable.
    #[arg(long)]
    model: Option<String>,
    /// OpenAI audio model. Defaults to OPENAI_AUDIO_MODEL or gpt-audio-1.5.
    #[arg(long)]
    audio_model: Option<String>,
    /// Reject clips longer than this instead of silently truncating them.
    #[arg(long, default_value_t = 300, value_parser = clap::value_parser!(u32).range(1..))]
    max_seconds: u32,
}

#[derive(Args)]
#[command(group(clap::ArgGroup::new("selection").required(true).args(["all", "index"])))]
struct ApplyArgs {
    report: PathBuf,
    /// Select every successful clip in the report.
    #[arg(long)]
    all: bool,
    /// Select clip numbers shown in the analysis, for example --index 1,3.
    #[arg(long, value_delimiter = ',')]
    index: Vec<usize>,
    /// Perform the renames. Without this, only print a preview.
    #[arg(long)]
    yes: bool,
}

fn main() -> std::process::ExitCode {
    match run() {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("Error: {error:#}");
            std::process::ExitCode::FAILURE
        }
    }
}

fn run() -> Result<()> {
    match Cli::parse()
        .command
        .unwrap_or(Commands::Gui { free_tier: false })
    {
        Commands::Gui { free_tier } => {
            hdcontext::desktop::launch(free_tier).map_err(|error| anyhow::anyhow!("{error}"))
        }
        Commands::Analyze(args) => analyze(args),
        Commands::Watch { config, service } => {
            let config = config
                .map(Ok)
                .unwrap_or_else(hdcontext::preferences::config_path)?;
            hdcontext::watch::run(
                &config,
                std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
                service,
            )
        }
        Commands::Apply(args) => apply(args),
        Commands::Doctor { provider } => doctor(provider),
    }
}

fn doctor(provider: Provider) -> Result<()> {
    let mut problems = Vec::new();
    for tool in ["ffmpeg", "ffprobe"] {
        match std::process::Command::new(tool).arg("-version").output() {
            Ok(output) if output.status.success() => println!("{tool}: available"),
            _ => {
                println!("{tool}: missing");
                problems.push(tool);
            }
        }
    }
    if gstreamer::init().is_ok()
        && ["playbin", "appsink", "qtdemux"]
            .iter()
            .all(|name| gstreamer::ElementFactory::find(name).is_some())
    {
        println!("GStreamer preview: available");
    } else {
        println!("GStreamer preview: missing playback plugins");
        problems.push("GStreamer playback plugins");
    }
    println!("Selected provider: {}", provider.label());
    for option in Provider::ALL {
        let has_key = std::env::var(option.key_env()).is_ok_and(|k| !k.trim().is_empty());
        println!(
            "{}: {}",
            option.key_env(),
            if has_key {
                "set, not validated"
            } else {
                "not set"
            }
        );
        if option == provider && !has_key {
            problems.push(option.key_env());
        }
    }
    println!(
        "Analysis sends media to {}. No API call was made.",
        provider.label()
    );
    ensure!(
        problems.is_empty(),
        "Missing requirements: {}",
        problems.join(", ")
    );
    Ok(())
}

fn analyze(args: AnalyzeArgs) -> Result<()> {
    ensure!(
        !args.output.try_exists()?,
        "Report already exists: {}. Choose another --output",
        args.output.display()
    );
    let parent = args
        .output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    // Fail on an unwritable output directory before making paid requests.
    let _write_check =
        tempfile::NamedTempFile::new_in(parent).context("Cannot write to report directory")?;
    let files = discover(&args.paths, args.recursive)?;
    ensure!(
        !files.contains(
            &parent
                .canonicalize()?
                .join(args.output.file_name().context("Output must name a file")?)
        ),
        "Output report cannot replace an input video"
    );
    let key = std::env::var(args.provider.key_env()).with_context(|| {
        format!(
            "Set {} before analysis, or enter a key in the desktop app",
            args.provider.key_env()
        )
    })?;
    let model = args
        .model
        .unwrap_or_else(|| args.provider.configured_model());
    let audio_model = args.audio_model.unwrap_or_else(configured_audio_model);
    let analyzer = Analyzer::for_provider(args.provider, &key, &model, &audio_model)?;
    let mut report = Report {
        version: 1,
        provider: args.provider,
        model,
        clips: Vec::new(),
        failures: Vec::new(),
    };
    eprintln!(
        "Analyzing {} clip(s). Media will be sent to {}.",
        files.len(),
        args.provider.label()
    );
    let file_count = files.len();
    for (file_index, source) in files.into_iter().enumerate() {
        eprintln!("Analyzing {}", source.display());
        match analyze_one(&source, &analyzer, args.hint.as_deref(), args.max_seconds) {
            Ok(clip) => {
                println!("\n[{}] {}", report.clips.len() + 1, source.display());
                if clip.analysis.moments.is_empty() {
                    println!("No clear highlight found.");
                }
                for moment in &clip.analysis.moments {
                    println!(
                        "  {} - {}  {}",
                        hdcontext::segments::timestamp(moment.start_seconds),
                        hdcontext::segments::timestamp(moment.end_seconds),
                        moment.description
                    );
                }
                if !clip.analysis.uncertainty.is_empty() {
                    println!("Uncertainty: {}", clip.analysis.uncertainty);
                }
                report.clips.push(clip);
            }
            Err(error) => {
                let stop_batch = hdcontext::ai::errors::should_stop_batch(&error);
                eprintln!("Skipped {}: {error:#}", source.display());
                report.failures.push(Failure {
                    source,
                    error: format!("{error:#}"),
                });
                if stop_batch {
                    eprintln!(
                        "Stopped after a provider limit error. {} remaining clip(s) were not sent.",
                        file_count - file_index - 1
                    );
                    break;
                }
            }
        }
    }
    report::save(&report, &args.output)?;
    println!(
        "\nReport: {}. Original files unchanged.",
        args.output.display()
    );
    if !report.failures.is_empty() {
        bail!(
            "{} clip(s) failed; successful suggestions and errors are saved in the report",
            report.failures.len()
        );
    }
    Ok(())
}

fn apply(args: ApplyArgs) -> Result<()> {
    let report = report::load(&args.report)?;
    let indices = if args.all {
        (1..=report.clips.len()).collect()
    } else {
        args.index
    };
    ensure!(!indices.is_empty(), "No clips selected");
    let plan = report::rename_plan(&report, &indices)?;
    for (source, destination) in &plan {
        println!("{} -> {}", source.display(), destination.display());
    }
    if args.yes {
        for (source, destination) in &plan {
            report::rename_no_clobber(source, destination)?;
            // Progress remains visible if a later filesystem operation fails.
            println!("Applied: {}", destination.display());
        }
        println!("Applied {} rename(s).", plan.len());
    } else {
        println!("Preview only. Add --yes to apply these renames.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_explicit_provider_and_model_settings() {
        let cli = Cli::try_parse_from([
            "hdcontext",
            "analyze",
            "clips",
            "--provider",
            "openai",
            "--model",
            "vision-model",
            "--audio-model",
            "audio-model",
        ])
        .unwrap();
        let Some(Commands::Analyze(args)) = cli.command else {
            panic!("Expected analyze command")
        };
        assert_eq!(args.provider, Provider::OpenAi);
        assert_eq!(args.model.as_deref(), Some("vision-model"));
        assert_eq!(args.audio_model.as_deref(), Some("audio-model"));
        assert!(matches!(
            Cli::try_parse_from(["hdcontext", "doctor", "--provider", "openai"])
                .unwrap()
                .command,
            Some(Commands::Doctor {
                provider: Provider::OpenAi
            })
        ));
    }

    #[test]
    fn rejects_unsupported_providers_instead_of_silently_using_another() {
        assert!(
            Cli::try_parse_from(["hdcontext", "analyze", "clips", "--provider", "unknown"])
                .is_err()
        );
    }
    #[test]
    fn watcher_command_is_headless_and_accepts_an_explicit_settings_path() {
        assert!(matches!(
            Cli::try_parse_from([
                "hdcontext",
                "watch",
                "--config",
                "/tmp/settings.json",
                "--service"
            ])
            .unwrap()
            .command,
            Some(Commands::Watch {
                config: Some(_),
                service: true
            })
        ));
    }
}
