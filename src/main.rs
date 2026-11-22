use anyhow::{Context, Result, bail};
use clap::{ArgAction, Parser};
use oatmealf::cancel;
use oatmealf::capture;
use oatmealf::output;
use oatmealf::planner::normalize_engine_root;
use oatmealf::process::{ProcessInfo, editor_engine_root, find_exactly_one, find_shipping_game};
use oatmealf::progress;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Parser, Debug)]
#[command(
    name = "oatmealf",
    version,
    about = "Compare UE 5.6 shader parameter layouts and generate source candidates",
    long_about = None
)]
struct Cli {
    /// Game process name. Autodetected when one *-Win64-Shipping.exe is running.
    #[arg(long, value_name = "PROCESS", conflicts_with_all = ["game_layouts", "game_minidump"])]
    game: Option<String>,

    /// Read the game's layouts from a full-memory Windows minidump (.DMP) instead of a live process.
    #[arg(long, value_name = "FILE", conflicts_with = "game_layouts")]
    game_minidump: Option<PathBuf>,

    /// Editor process name.
    #[arg(
        long,
        value_name = "PROCESS",
        default_value = "UnrealEditor",
        conflicts_with = "editor_layouts"
    )]
    editor: String,

    /// Unreal Engine root, Engine directory, or Engine\Source. Autodetected from the editor.
    #[arg(long, value_name = "PATH")]
    engine_root: Option<PathBuf>,

    /// Read a saved game-layouts.json capture instead of the running game.
    #[arg(long, value_name = "FILE", alias = "game-dump")]
    game_layouts: Option<PathBuf>,

    /// Read a saved editor-layouts.json capture instead of the running editor.
    #[arg(long, value_name = "FILE", alias = "editor-dump")]
    editor_layouts: Option<PathBuf>,

    /// Select a specific output directory. Defaults to Generated.
    #[arg(short, long, value_name = "PATH")]
    output: Option<PathBuf>,

    /// Only check named layouts. Repeat or separate names with commas.
    #[arg(
        long = "layout",
        value_name = "NAME",
        value_delimiter = ',',
        value_parser = parse_layout_name
    )]
    layouts: Vec<String>,

    /// Show capture and verification details.
    #[arg(short, long, action = ArgAction::Count)]
    verbose: u8,
}

fn parse_layout_name(value: &str) -> std::result::Result<String, String> {
    let value = value.trim();
    if value.is_empty() {
        Err("layout name cannot be empty".to_string())
    } else {
        Ok(value.to_string())
    }
}

fn main() -> ExitCode {
    let bare_invocation = std::env::args_os().len() == 1;
    let cli = Cli::parse();
    if let Err(error) = cancel::install() {
        eprintln!("Error: could not install Ctrl+C handler: {error:#}");
        return ExitCode::from(2);
    }
    match run(cli, bare_invocation) {
        Ok(code) => ExitCode::from(code),
        Err(error) => {
            progress::finish_transient();
            if cancel::is_cancelled() {
                eprintln!("Interrupted.");
                return ExitCode::from(130);
            }
            eprintln!("Error: {error:#}");
            ExitCode::from(2)
        }
    }
}

fn run(cli: Cli, bare_invocation: bool) -> Result<u8> {
    let output_parent = cli
        .output
        .map(absolute_path)
        .transpose()?
        .unwrap_or_else(default_output_parent);

    // show help after discovery errors from a bare double-click
    let game_process = if cli.game_layouts.is_none() && cli.game_minidump.is_none() {
        match find_shipping_game(cli.game.as_deref()) {
            Ok(process) => Some(process),
            Err(error) if bare_invocation => return fail_with_help(&error),
            Err(error) => return Err(error),
        }
    } else {
        None
    };
    let editor_process = if cli.editor_layouts.is_none() {
        match find_exactly_one(&cli.editor, "editor") {
            Ok(process) => Some(process),
            Err(error) if bare_invocation => return fail_with_help(&error),
            Err(error) => return Err(error),
        }
    } else {
        None
    };

    let engine_root = match locate_engine_root(
        cli.engine_root.as_deref(),
        editor_process.as_ref(),
        cli.verbose > 0,
    ) {
        Some(root) => root,
        None => return Ok(0),
    };
    let output_directory = new_output_directory(&output_parent)?;

    if cli.verbose > 0 {
        if let Some(process) = &game_process {
            println!("Game: {} ({})", process.executable_name, process.pid);
        }
        if let Some(process) = &editor_process {
            println!("Editor: {} ({})", process.executable_name, process.pid);
        }
        println!("Engine source: {}", engine_root.display());
    }

    let needs_capture =
        game_process.is_some() || editor_process.is_some() || cli.game_minidump.is_some();
    let capture_directory = needs_capture.then(TemporaryCapture::new).transpose()?;
    let game_layouts_path = if let Some(dump) = cli.game_minidump.as_deref() {
        let directory = capture_directory
            .as_ref()
            .context("internal error: capture directory was not created")?;
        let path = directory.path.join("game-layouts.json");
        progress::step(&format!("Reading minidump {}...", dump.display()));
        capture::run_from_dump(dump, &path, cli.verbose > 0)?;
        path
    } else {
        resolve_or_capture(
            cli.game_layouts.as_deref(),
            game_process.as_ref(),
            capture_directory.as_ref(),
            "game-layouts.json",
            cli.verbose > 0,
        )?
    };
    cancel::check()?;
    let editor_layouts_path = resolve_or_capture(
        cli.editor_layouts.as_deref(),
        editor_process.as_ref(),
        capture_directory.as_ref(),
        "editor-layouts.json",
        cli.verbose > 0,
    )?;
    cancel::check()?;

    output::generate(
        &game_layouts_path,
        &editor_layouts_path,
        &engine_root,
        &output_directory,
        &cli.layouts,
        cli.verbose > 0,
    )?;
    cancel::check()?;
    Ok(0)
}

fn fail_with_help(error: &anyhow::Error) -> Result<u8> {
    use clap::CommandFactory;

    progress::finish_transient();
    eprintln!("Error: {error:#}");
    eprintln!();
    let _ = Cli::command().print_help();
    Ok(2)
}

fn locate_engine_root(
    supplied: Option<&Path>,
    editor: Option<&ProcessInfo>,
    verbose: bool,
) -> Option<PathBuf> {
    let result = match supplied {
        Some(path) => normalize_engine_root(path),
        None => editor
            .context("engine source cannot be located from a saved editor capture")
            .and_then(editor_engine_root),
    };

    match result {
        Ok(root) => Some(root),
        Err(error) => {
            match (supplied, editor) {
                (Some(path), _) => println!(
                    "Unreal Engine source was not found under {}.",
                    path.display()
                ),
                (None, Some(_)) => {
                    println!("Unreal Engine source was not found next to the running editor.")
                }
                (None, None) => {
                    println!(
                        "Saved layout evidence does not identify an Unreal Engine source tree."
                    )
                }
            }
            println!(r"Run oatmealf again with --engine-root C:\path\to\UnrealEngine");
            if verbose {
                println!("Details: {error:#}");
            }
            None
        }
    }
}

fn resolve_or_capture(
    saved_dump: Option<&Path>,
    process: Option<&ProcessInfo>,
    capture_directory: Option<&TemporaryCapture>,
    file_name: &str,
    verbose: bool,
) -> Result<PathBuf> {
    if let Some(path) = saved_dump {
        return path
            .canonicalize()
            .with_context(|| format!("layout evidence not found: {}", path.display()));
    }
    let process = process.context("internal error: capture process was not selected")?;
    let directory =
        capture_directory.context("internal error: capture directory was not created")?;
    let path = directory.path.join(file_name);
    progress::step(&format!("Scanning {} memory...", process.executable_name));
    capture::run_with_verbosity(process, &path, verbose)?;
    Ok(path)
}

fn absolute_path(path: PathBuf) -> Result<PathBuf> {
    Ok(std::path::absolute(path)?)
}

fn default_output_parent() -> PathBuf {
    std::env::current_dir()
        .unwrap_or_else(|_| PathBuf::from("."))
        .join("Generated")
}

fn new_output_directory(parent: &Path) -> Result<PathBuf> {
    if parent.exists() && !parent.is_dir() {
        bail!("output path is not a directory: {}", parent.display());
    }
    let now = chrono::Local::now();
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    for attempt in 0..256_u64 {
        let suffix = (nanos as u64 ^ u64::from(std::process::id()) ^ attempt) & 0x00ff_ffff;
        let candidate = parent.join(format!("{}-{suffix:06x}", now.format("%Y%m%d-%H%M%S")));
        if !candidate.exists() {
            return Ok(candidate);
        }
    }

    bail!(
        "could not choose a new output directory under {}",
        parent.display()
    )
}

struct TemporaryCapture {
    path: PathBuf,
}

impl TemporaryCapture {
    fn new() -> Result<Self> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = std::env::temp_dir().join(format!("oatmealf-{}-{nanos:x}", std::process::id()));
        fs::create_dir(&path).with_context(|| {
            format!(
                "could not create temporary capture directory {}",
                path.display()
            )
        })?;
        Ok(Self { path })
    }
}

impl Drop for TemporaryCapture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}
