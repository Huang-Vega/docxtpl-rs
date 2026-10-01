//! docxtpl CLI: supports both the Rust `render` subcommand and the Python
//! docxtpl direct invocation form.

use std::ffi::OsString;
use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use docxtpl_rs::{DocxTemplate, RenderOptions, RenderedDocument};

/// Prevents untrusted JSON from being read into memory without bound before
/// parsing.
const MAX_CONTEXT_JSON_BYTES: usize = 64 * 1024 * 1024;

#[derive(Parser)]
#[command(
    name = "docxtpl",
    version,
    about = "docxtpl-rs command line (semantics aligned with docxtpl 0.20.2)"
)]
struct SubcommandCli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Render a docx template with a JSON context.
    Render {
        /// Path to the template .docx.
        template: PathBuf,
        /// Path to the context JSON file.
        context: PathBuf,
        /// Path to the output .docx.
        output: PathBuf,
        /// Enable autoescape (disabled by default upstream; an explicit
        /// enhancement).
        #[arg(long)]
        autoescape: bool,
    },
}

/// The direct invocation form of Python docxtpl 0.20.2.
#[derive(Parser)]
#[command(
    name = "docxtpl",
    version,
    about = "Render a docx template with a JSON context",
    after_help = "The Rust extended form is still available: docxtpl render <template.docx> <context.json> <output.docx> [--autoescape]"
)]
struct DirectCli {
    /// Allow overwriting an existing output file.
    #[arg(short = 'o', long)]
    overwrite: bool,
    /// Suppress the success message (errors are still written to stderr).
    #[arg(short = 'q', long)]
    quiet: bool,
    /// Path to the template .docx.
    template: PathBuf,
    /// Path to the context .json file.
    context: PathBuf,
    /// Path to the output .docx.
    output: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InvocationStyle {
    Subcommand,
    Direct,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SuccessChannel {
    Stdout,
    Stderr,
}

#[derive(Debug, PartialEq, Eq)]
struct Invocation {
    template: PathBuf,
    context: PathBuf,
    output: PathBuf,
    autoescape: bool,
    overwrite: bool,
    quiet: bool,
    style: InvocationStyle,
}

fn main() -> ExitCode {
    let invocation = match parse_invocation_from(std::env::args_os()) {
        Ok(invocation) => invocation,
        Err(error) => {
            let exit_code = clap_exit_code(&error);
            let _ = error.print();
            return ExitCode::from(exit_code);
        }
    };
    let output = invocation.output.clone();
    let quiet = invocation.quiet;
    let style = invocation.style;

    match render(invocation) {
        Ok(()) => {
            if let Some((channel, message)) = success_message(&output, quiet, style) {
                match channel {
                    SuccessChannel::Stdout => println!("{message}"),
                    SuccessChannel::Stderr => eprintln!("{message}"),
                }
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("Render failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn clap_exit_code(error: &clap::Error) -> u8 {
    u8::try_from(error.exit_code()).unwrap_or(2)
}

fn success_message(
    output: &Path,
    quiet: bool,
    style: InvocationStyle,
) -> Option<(SuccessChannel, String)> {
    (!quiet).then(|| {
        let channel = match style {
            InvocationStyle::Direct => SuccessChannel::Stdout,
            InvocationStyle::Subcommand => SuccessChannel::Stderr,
        };
        (channel, format!("Rendered: {}", output.display()))
    })
}

/// Each syntax gets its own clap parser so that a valid `template.docx`
/// argument is not mistaken for an unknown subcommand. The existing Rust
/// syntax is detected by the first argument `render` (or clap's `help`);
/// everything else uses the Python docxtpl direct invocation form.
fn parse_invocation_from<I, T>(args: I) -> Result<Invocation, clap::Error>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString>,
{
    let args: Vec<OsString> = args.into_iter().map(Into::into).collect();
    let subcommand_style = args
        .get(1)
        .and_then(|arg| arg.to_str())
        .is_some_and(|arg| matches!(arg, "render" | "help"));

    if subcommand_style {
        let cli = SubcommandCli::try_parse_from(args)?;
        let Command::Render {
            template,
            context,
            output,
            autoescape,
        } = cli.command;
        Ok(Invocation {
            template,
            context,
            output,
            autoescape,
            // Keep the existing render subcommand overwrite behavior to avoid
            // breaking existing scripts.
            overwrite: true,
            quiet: false,
            style: InvocationStyle::Subcommand,
        })
    } else {
        let cli = DirectCli::try_parse_from(args)?;
        Ok(Invocation {
            template: cli.template,
            context: cli.context,
            output: cli.output,
            autoescape: false,
            overwrite: cli.overwrite,
            quiet: cli.quiet,
            style: InvocationStyle::Direct,
        })
    }
}

fn render(invocation: Invocation) -> Result<(), Box<dyn std::error::Error>> {
    if invocation.style == InvocationStyle::Direct {
        validate_direct_paths(&invocation)?;
    }
    let context_file = File::open(&invocation.context)?;
    let context_text = read_limited(context_file, MAX_CONTEXT_JSON_BYTES)?;
    let context: serde_json::Value = serde_json::from_str(&context_text)?;
    let template = DocxTemplate::open(&invocation.template)?;
    let options = RenderOptions::compat().with_autoescape(invocation.autoescape);
    let document = template.render(&context, &options)?;

    match invocation.style {
        InvocationStyle::Subcommand => document.save(&invocation.output)?,
        InvocationStyle::Direct => {
            save_direct(&document, &invocation.output, invocation.overwrite)?
        }
    }
    Ok(())
}

/// Read at most `limit + 1` bytes so oversize input can be detected without
/// buffering the whole input.
fn read_limited(reader: impl Read, limit: usize) -> io::Result<String> {
    let probe_limit = u64::try_from(limit)
        .ok()
        .and_then(|limit| limit.checked_add(1))
        .ok_or_else(|| invalid_input("context size limit cannot be represented".to_owned()))?;
    let mut bytes = Vec::new();
    reader.take(probe_limit).read_to_end(&mut bytes)?;
    if bytes.len() > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("context JSON exceeds the {limit}-byte limit"),
        ));
    }
    String::from_utf8(bytes).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("context JSON is not valid UTF-8: {error}"),
        )
    })
}

fn validate_direct_paths(invocation: &Invocation) -> io::Result<()> {
    validate_input(&invocation.template, "Template", "docx")?;
    validate_input(&invocation.context, "Context", "json")?;
    validate_extension(&invocation.output, "Output", "docx")?;

    if invocation.output.exists() {
        let metadata = std::fs::metadata(&invocation.output)?;
        if !metadata.is_file() {
            return Err(invalid_input(format!(
                "output path is not a regular file: {}",
                invocation.output.display()
            )));
        }
        if !invocation.overwrite {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!(
                    "output file already exists: {} (use -o/--overwrite to overwrite)",
                    invocation.output.display()
                ),
            ));
        }
    }
    Ok(())
}

fn validate_input(path: &Path, label: &str, extension: &str) -> io::Result<()> {
    validate_extension(path, label, extension)?;
    let metadata = std::fs::metadata(path).map_err(|error| {
        io::Error::new(
            error.kind(),
            format!("{label} file is not accessible {}: {error}", path.display()),
        )
    })?;
    if !metadata.is_file() {
        return Err(invalid_input(format!(
            "{label} path is not a regular file: {}",
            path.display()
        )));
    }
    Ok(())
}

fn validate_extension(path: &Path, label: &str, expected: &str) -> io::Result<()> {
    // The Python 0.20.2 CLI applies `endswith(".docx")` / `.json` directly to
    // the path string, so the suffix match is case-sensitive; keep that
    // behavior.
    let suffix = format!(".{expected}");
    let valid = path
        .as_os_str()
        .to_str()
        .is_some_and(|path| path.ends_with(&suffix));
    if valid {
        Ok(())
    } else {
        Err(invalid_input(format!(
            "{label} file must have the .{expected} extension: {}",
            path.display()
        )))
    }
}

fn invalid_input(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

/// `create_new` still provides atomic overwrite protection between validation
/// and writing, preventing a race condition from clobbering user files.
fn open_direct_output(path: &Path, overwrite: bool) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true);
    if overwrite {
        options.create(true).truncate(true);
    } else {
        options.create_new(true);
    }
    options.open(path)
}

fn save_direct(
    document: &RenderedDocument,
    path: &Path,
    overwrite: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let file = open_direct_output(path, overwrite)?;
    document.write_to(file)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::{Cursor, Write};

    use super::*;

    fn test_dir() -> tempfile::TempDir {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.tmp");
        std::fs::create_dir_all(&root).expect("create in-repo test temp directory");
        tempfile::Builder::new()
            .prefix("docxtpl-cli-")
            .tempdir_in(root)
            .expect("create CLI test directory")
    }

    #[test]
    fn parses_existing_render_subcommand_and_keeps_overwrite_semantics() {
        let invocation = parse_invocation_from([
            "docxtpl",
            "render",
            "template.docx",
            "context.json",
            "output.docx",
            "--autoescape",
        ])
        .expect("parse render subcommand");

        assert_eq!(invocation.style, InvocationStyle::Subcommand);
        assert!(invocation.autoescape);
        assert!(invocation.overwrite);
        assert!(!invocation.quiet);
    }

    #[test]
    fn parses_python_direct_invocation_overwrite_and_quiet() {
        let invocation = parse_invocation_from([
            "docxtpl",
            "-o",
            "--quiet",
            "template.docx",
            "context.json",
            "output.docx",
        ])
        .expect("parse direct invocation");

        assert_eq!(invocation.style, InvocationStyle::Direct);
        assert!(!invocation.autoescape);
        assert!(invocation.overwrite);
        assert!(invocation.quiet);
        assert_eq!(invocation.context, PathBuf::from("context.json"));
    }

    #[test]
    fn quiet_only_suppresses_success_message() {
        let output = Path::new("output.docx");
        assert_eq!(success_message(output, true, InvocationStyle::Direct), None);
        assert_eq!(
            success_message(output, false, InvocationStyle::Direct),
            Some((SuccessChannel::Stdout, "Rendered: output.docx".to_owned()))
        );
        assert_eq!(
            success_message(output, false, InvocationStyle::Subcommand),
            Some((SuccessChannel::Stderr, "Rendered: output.docx".to_owned()))
        );
    }

    #[test]
    fn limited_read_accepts_input_exactly_at_limit() {
        let value = read_limited(Cursor::new(b"1234"), 4).expect("read input within limit");
        assert_eq!(value, "1234");
    }

    #[test]
    fn limited_read_probes_one_extra_byte_and_rejects_oversize() {
        let mut reader = Cursor::new(b"123456789");
        let error = read_limited(&mut reader, 4).expect_err("oversize input should fail");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("4-byte limit"), "{error}");
        assert_eq!(reader.position(), 5);
    }

    #[test]
    fn limited_read_preserves_utf8_validation() {
        let error = read_limited(Cursor::new([0xff]), 1).expect_err("non-UTF-8 input should fail");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("UTF-8"), "{error}");
    }

    #[test]
    fn direct_invocation_missing_positional_args_rejected_by_clap() {
        let error = parse_invocation_from(["docxtpl", "template.docx", "context.json"])
            .expect_err("missing output path should fail");
        assert_eq!(
            error.kind(),
            clap::error::ErrorKind::MissingRequiredArgument
        );
        assert_eq!(clap_exit_code(&error), 2);
    }

    #[test]
    fn help_and_version_use_success_exit_codes() {
        for flag in ["--help", "--version"] {
            let error = parse_invocation_from(["docxtpl", flag])
                .expect_err("clap displays help and version through its error type");
            assert_eq!(clap_exit_code(&error), 0, "{flag}");
        }
    }

    #[test]
    fn path_validation_covers_extension_and_input_existence() {
        let temp = test_dir();
        let template = temp.path().join("template.docx");
        let context = temp.path().join("context.json");
        std::fs::write(&template, b"docx").expect("write template placeholder");
        std::fs::write(&context, b"{}").expect("write context");
        let invocation = Invocation {
            template,
            context,
            output: temp.path().join("output.docx"),
            autoescape: false,
            overwrite: false,
            quiet: false,
            style: InvocationStyle::Direct,
        };
        validate_direct_paths(&invocation).expect("valid input paths");

        let mut bad_extension = invocation;
        bad_extension.context = temp.path().join("context.JSON");
        std::fs::write(&bad_extension.context, b"{}").expect("write uppercase-extension context");
        let error = validate_direct_paths(&bad_extension)
            .expect_err("uppercase extension should fail per upstream semantics");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);

        bad_extension.context = temp.path().join("missing.json");
        let error = validate_direct_paths(&bad_extension).expect_err("missing input should fail");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn overwrite_rejected_by_default_and_original_content_preserved() {
        let temp = test_dir();
        let output = temp.path().join("output.docx");
        std::fs::write(&output, b"user data").expect("write existing output");

        let error =
            open_direct_output(&output, false).expect_err("overwrite must be rejected without -o");
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read(&output).expect("reread output"), b"user data");
    }

    #[test]
    fn overwrite_explicitly_allows_truncating_existing_file() {
        let temp = test_dir();
        let output = temp.path().join("output.docx");
        std::fs::write(&output, b"old data").expect("write existing output");

        let mut file = open_direct_output(&output, true).expect("opening allowed with -o");
        file.write_all(b"new").expect("write new content");
        drop(file);
        assert_eq!(std::fs::read(&output).expect("reread output"), b"new");
    }
}
