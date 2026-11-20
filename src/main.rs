mod datetime;
mod jpeg_stego;
mod metadata;
mod payload;
mod png_stego;
mod stego;
mod visible;

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args, CommandFactory, Parser, Subcommand, ValueEnum};
use image::metadata::Orientation;
use image::{DynamicImage, ExtendedColorType, ImageDecoder, ImageEncoder, ImageReader};
use rayon::prelude::*;

use payload::Payload;

#[derive(Parser)]
#[command(
    name = "watermarker",
    version,
    about = "Add visible + invisible watermarks to PNG/JPEG images (single static binary)",
    after_help = "Examples:\n  watermarker photo.jpg\n  watermarker -t 'CONFIDENTIAL' --opacity 35 --single passport.jpg\n  watermarker --hidden 'case 42' -o out/ *.jpg *.png\n  watermarker verify out/*.wm.jpg"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Add visible and invisible watermarks (default)
    Add(AddArgs),
    /// Inspect metadata, pixel/DCT payload, and integrity
    Verify(VerifyArgs),
}

#[derive(Args)]
#[command(after_help = "Other commands:\n  watermarker verify <FILE>...   Inspect metadata and the hidden payload\n  watermarker --help             List all commands and top-level options")]
struct AddArgs {
    /// Input files and/or directories (PNG, JPG, JPEG)
    #[arg(required = true, value_name = "FILE", num_args = 1..)]
    files: Vec<PathBuf>,

    /// Visible text; placeholders: {file} {date} {time} {id}
    #[arg(short = 't', long, default_value = "CONFIDENTIAL")]
    text: String,

    /// Extra note stored only in the invisible payload
    #[arg(long, value_name = "TEXT")]
    hidden: Option<String>,

    /// Fixed record id (default: derived from time + file + key)
    #[arg(long, value_name = "ID")]
    id: Option<String>,

    /// Rotation in degrees (45 = diagonal, 90 = vertical)
    #[arg(short = 'a', long, default_value_t = 45.0)]
    angle: f32,

    /// Visible opacity, 0-100
    #[arg(short = 'O', long, default_value_t = 28, value_parser = clap::value_parser!(u8).range(0..=100))]
    opacity: u8,

    /// Text size as % of the shorter image side
    #[arg(short = 's', long, default_value_t = 4.5)]
    size: f32,

    /// Gap between tiled copies, as % of text line height
    #[arg(long, default_value_t = 200.0)]
    gap: f32,

    /// Draw one watermark instead of tiling
    #[arg(long)]
    single: bool,

    /// Placement when --single is used
    #[arg(long, value_enum, default_value = "center")]
    position: visible::Position,

    /// Visible text colour (#RRGGBB or white/black/red/gray/yellow/blue)
    #[arg(long, default_value = "#ffffff")]
    color: String,

    /// Disable the dark outline around visible text
    #[arg(long)]
    no_outline: bool,

    /// Output directory (default: next to each input)
    #[arg(short = 'o', long, value_name = "DIR")]
    out_dir: Option<PathBuf>,

    /// Suffix inserted before the extension
    #[arg(long, default_value = ".wm")]
    suffix: String,

    /// JPEG quality (1-100)
    #[arg(short = 'q', long, default_value_t = 92, value_parser = clap::value_parser!(u8).range(1..=100))]
    quality: u8,

    /// Key for bit placement (use the same key to verify)
    #[arg(long, default_value = "watermarker-default-key-v1", value_name = "STRING")]
    key: String,

    /// Do not draw anything visible
    #[arg(long)]
    no_visible: bool,

    /// Do not embed metadata or LSB/DCT payload (visible only)
    #[arg(long)]
    no_invisible: bool,

    /// Do not descend into directories
    #[arg(long)]
    no_recursive: bool,
}

#[derive(Args)]
#[command(after_help = "Other commands:\n  watermarker add <FILE>...      Add visible and invisible watermarks\n  watermarker --help             List all commands and top-level options")]
struct VerifyArgs {
    /// Input files and/or directories
    #[arg(required = true, value_name = "FILE", num_args = 1..)]
    files: Vec<PathBuf>,

    /// Key used when the watermark was created
    #[arg(long, default_value = "watermarker-default-key-v1", value_name = "STRING")]
    key: String,

    /// Print machine-readable JSON
    #[arg(long)]
    json: bool,

    /// Do not descend into directories
    #[arg(long)]
    no_recursive: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum Kind {
    Png,
    Jpeg,
}

impl Kind {
    fn label(self) -> &'static str {
        match self {
            Kind::Png => "png",
            Kind::Jpeg => "jpeg",
        }
    }
}

struct AddReport {
    input: PathBuf,
    output: PathBuf,
    width: u32,
    height: u32,
    kind: Kind,
    visible: bool,
    stego: Option<(usize, usize)>,
    payload_bytes: usize,
    output_bytes: u64,
}

struct FileFailure {
    input: PathBuf,
    message: String,
}

fn main() -> ExitCode {
    let mut argv: Vec<std::ffi::OsString> = std::env::args_os().collect();
    if argv.len() == 1 {
        let mut command = Cli::command();
        let _ = command.print_help();
        println!();
        return ExitCode::from(2);
    }
    let first = argv.get(1).and_then(|value| value.to_str()).unwrap_or("");
    let has_subcommand = matches!(first, "add" | "verify" | "help");
    let is_top_level_flag = matches!(first, "-h" | "--help" | "-V" | "--version");
    if !has_subcommand && !is_top_level_flag {
        argv.insert(1, std::ffi::OsString::from("add"));
    }

    let cli = Cli::parse_from(argv);
    match cli.command {
        Command::Add(args) => run_add(args),
        Command::Verify(args) => run_verify(args),
    }
}

fn run_add(args: AddArgs) -> ExitCode {
    if args.no_visible && args.no_invisible {
        eprintln!("error: --no-visible and --no-invisible together would do nothing");
        return ExitCode::FAILURE;
    }
    if args.size <= 0.0 {
        eprintln!("error: --size must be positive");
        return ExitCode::FAILURE;
    }
    if !args.size.is_finite() || !args.angle.is_finite() || !args.gap.is_finite() {
        eprintln!("error: --size, --angle and --gap must be finite numbers");
        return ExitCode::FAILURE;
    }
    let color = match visible::parse_color(&args.color) {
        Ok(color) => color,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::FAILURE;
        }
    };
    let files = match collect_files(&args.files, !args.no_recursive) {
        Ok(files) => files,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::FAILURE;
        }
    };
    // Refuse batches that would silently overwrite each other's output, e.g.
    // two `passport.jpg` files from different subdirectories with -o out/.
    let mut seen = HashMap::new();
    for file in &files {
        let output = match output_path(file, &args.suffix, args.out_dir.as_deref()) {
            Ok(output) => output,
            Err(error) => {
                eprintln!("error: {}: {error}", file.display());
                return ExitCode::FAILURE;
            }
        };
        if output == *file {
            eprintln!(
                "error: {}: output would equal input; change --suffix or --out-dir",
                file.display()
            );
            return ExitCode::FAILURE;
        }
        if let Some(previous) = seen.insert(output.clone(), file.clone()) {
            eprintln!(
                "error: {} and {} would both write {}; use --suffix or separate -o folders",
                previous.display(),
                file.display(),
                output.display()
            );
            return ExitCode::FAILURE;
        }
    }

    let style = visible::Style {
        text: &args.text,
        angle_deg: args.angle,
        opacity: args.opacity as f32 / 100.0,
        size_pct: args.size,
        gap_pct: args.gap,
        single: args.single,
        position: args.position,
        color,
        outline: !args.no_outline,
    };

    let results: Vec<Result<AddReport, FileFailure>> = files
        .par_iter()
        .map(|path| {
            process_one(path, &args, &style)
                .map_err(|message| FileFailure {
                    input: path.clone(),
                    message,
                })
        })
        .collect();

    let mut failures = 0usize;
    let mut succeeded = 0usize;
    let mut output_bytes = 0u64;

    for result in results {
        match result {
            Ok(report) => {
                succeeded += 1;
                output_bytes += report.output_bytes;
                let invisible = if args.no_invisible {
                    "off".to_string()
                } else {
                    let (slots, need) = report.stego.unwrap_or((0, 0));
                    format!("metadata + {} (slots {} / need {})", report.kind.stego_name(), slots, need)
                };
                println!(
                    "OK   {} -> {}  {}x{} {} | visible {} | invisible {} | {} B payload | {:.1} KiB",
                    report.input.display(),
                    report.output.display(),
                    report.width,
                    report.height,
                    report.kind.label(),
                    if report.visible { "on" } else { "off" },
                    invisible,
                    report.payload_bytes,
                    report.output_bytes as f64 / 1024.0,
                );
            }
            Err(failure) => {
                failures += 1;
                eprintln!("FAIL {}: {}", failure.input.display(), failure.message);
            }
        }
    }

    println!(
        "Done: {} file(s) watermarked, {} failed, {:.1} KiB written. Original metadata was stripped.",
        succeeded,
        failures,
        output_bytes as f64 / 1024.0
    );

    if failures > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

impl Kind {
    fn stego_name(self) -> &'static str {
        match self {
            Kind::Png => "pixel-LSB",
            Kind::Jpeg => "DCT-LSB",
        }
    }
}

fn process_one(
    input: &Path,
    args: &AddArgs,
    style: &visible::Style,
) -> Result<AddReport, String> {
    let (mut image, kind) = load_image(input)?;
    let (width, height) = (image.width(), image.height());
    if width == 0 || height == 0 {
        return Err("image has zero dimensions".into());
    }
    let file_name = input
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or("file name is not valid UTF-8")?
        .to_string();
    let unix = datetime::now_unix();
    let id = args
        .id
        .clone()
        .unwrap_or_else(|| payload::make_id(unix, &file_name, &args.key));
    let text = payload::resolve_text(&args.text, &file_name, &id, unix);
    let record = Payload::new(id.clone(), unix, file_name, text.clone(), args.hidden.clone());
    let json = record.to_json();
    let frame = if args.no_invisible {
        None
    } else {
        Some(stego::frame(json.as_bytes())?)
    };

    if !args.no_visible {
        let local_style = visible::Style { text: &text, ..*style };
        if image.color().has_alpha() {
            let mut buffer = image.to_rgba8();
            visible::apply(
                buffer.as_mut(),
                width as usize,
                height as usize,
                4,
                &local_style,
            );
            image = DynamicImage::ImageRgba8(buffer);
        } else {
            let mut buffer = image.to_rgb8();
            visible::apply(
                buffer.as_mut(),
                width as usize,
                height as usize,
                3,
                &local_style,
            );
            image = DynamicImage::ImageRgb8(buffer);
        }
    }

    let mut stego_info = None;
    let mut encoded = match kind {
        Kind::Png => {
            let (mut raw, channels) = if image.color().has_alpha() {
                (image.to_rgba8().into_raw(), 4usize)
            } else {
                (image.to_rgb8().into_raw(), 3usize)
            };
            if let Some(frame) = &frame {
                let seed = stego::seed_from_key(&args.key);
                let slots =
                    png_stego::embed(&mut raw, width as usize, height as usize, channels, frame, seed)?;
                stego_info = Some((slots, stego::needed_slots(frame.len())));
            }
            let color_type = if channels == 4 {
                ExtendedColorType::Rgba8
            } else {
                ExtendedColorType::Rgb8
            };
            let mut bytes = Vec::new();
            let encoder = image::codecs::png::PngEncoder::new(&mut bytes);
            encoder
                .write_image(&raw, width, height, color_type)
                .map_err(|error| format!("PNG encoding failed: {error}"))?;
            bytes
        }
        Kind::Jpeg => {
            if width > u16::MAX as u32 || height > u16::MAX as u32 {
                return Err("JPEG dimensions exceed 65535 pixels".into());
            }
            let rgb = image.to_rgb8();
            let mut bytes = Vec::new();
            {
                let mut encoder =
                    jpeg_encoder::Encoder::new(&mut bytes, args.quality);
                encoder.set_sampling_factor(jpeg_encoder::SamplingFactor::F_2_1);
                encoder
                    .encode(
                        rgb.as_raw(),
                        width as u16,
                        height as u16,
                        jpeg_encoder::ColorType::Rgb,
                    )
                    .map_err(|error| format!("JPEG encoding failed: {error}"))?;
            }
            if let Some(frame) = &frame {
                let seed = stego::seed_from_key(&args.key);
                let info = jpeg_stego::embed(&bytes, frame, seed)?;
                stego_info = Some((info.slots, info.required));
                bytes = info.bytes_out;
            }
            bytes
        }
    };

    if let Some(frame) = &frame {
        let xmp = metadata::build_xmp(&json, &id, &record.utc);
        let comment = format!(
            "Watermarked with watermarker | id={id} | utc={} | verify: watermarker verify <file>",
            record.utc
        );
        encoded = match kind {
            Kind::Png => metadata::inject_png_xmp(&encoded, &xmp)?,
            Kind::Jpeg => metadata::inject_jpeg_xmp(&encoded, &xmp, &comment)?,
        };
        let _ = frame;
    }

    let output = output_path(input, &args.suffix, args.out_dir.as_deref())?;
    if output == input {
        return Err("output path equals input path; change --suffix or --out-dir".into());
    }
    if let Some(parent) = output.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)
                .map_err(|error| format!("cannot create {}: {error}", parent.display()))?;
        }
    }
    fs::write(&output, &encoded)
        .map_err(|error| format!("cannot write {}: {error}", output.display()))?;

    Ok(AddReport {
        input: input.to_path_buf(),
        output,
        width,
        height,
        kind,
        visible: !args.no_visible,
        stego: stego_info,
        payload_bytes: json.len(),
        output_bytes: encoded.len() as u64,
    })
}

fn load_image(path: &Path) -> Result<(DynamicImage, Kind), String> {
    let reader = ImageReader::open(path)
        .map_err(|error| format!("cannot open: {error}"))?
        .with_guessed_format()
        .map_err(|error| format!("cannot read: {error}"))?;
    let format = reader
        .format()
        .ok_or_else(|| "unrecognised image format (only PNG and JPEG are supported)".to_string())?;
    let kind = match format {
        image::ImageFormat::Png => Kind::Png,
        image::ImageFormat::Jpeg => Kind::Jpeg,
        other => {
            return Err(format!(
                "unsupported format {other:?} (only PNG and JPEG are supported)"
            ))
        }
    };
    let mut decoder = reader
        .into_decoder()
        .map_err(|error| format!("cannot decode: {error}"))?;
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    let mut image =
        DynamicImage::from_decoder(decoder).map_err(|error| format!("cannot decode: {error}"))?;
    if orientation != Orientation::NoTransforms {
        image.apply_orientation(orientation);
    }
    Ok((image, kind))
}

fn output_path(input: &Path, suffix: &str, out_dir: Option<&Path>) -> Result<PathBuf, String> {
    let stem = input
        .file_stem()
        .and_then(|value| value.to_str())
        .ok_or("input file name is not valid UTF-8")?;
    let extension = input
        .extension()
        .and_then(|value| value.to_str())
        .ok_or("input file has no extension")?;
    let name = format!("{stem}{suffix}.{extension}");
    Ok(match out_dir {
        Some(directory) => directory.join(name),
        None => input.with_file_name(name),
    })
}

fn collect_files(paths: &[PathBuf], recursive: bool) -> Result<Vec<PathBuf>, String> {
    let mut files = Vec::new();
    for path in paths {
        if path.is_dir() {
            walk_directory(path, recursive, &mut files)?;
        } else if path.is_file() {
            files.push(path.clone());
        } else {
            return Err(format!("not found: {}", path.display()));
        }
    }
    files.sort();
    files.dedup();
    if files.is_empty() {
        return Err("no PNG/JPEG files found".into());
    }
    Ok(files)
}

fn walk_directory(path: &Path, recursive: bool, files: &mut Vec<PathBuf>) -> Result<(), String> {
    let mut entries: Vec<_> = fs::read_dir(path)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    entries.sort_by_key(|entry| entry.path());
    for entry in entries {
        let child = entry.path();
        if child.is_dir() {
            if recursive {
                walk_directory(&child, recursive, files)?;
            }
        } else if is_supported(&child) {
            files.push(child);
        }
    }
    Ok(())
}

fn is_supported(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| {
            matches!(
                extension.to_ascii_lowercase().as_str(),
                "png" | "jpg" | "jpeg"
            )
        })
        .unwrap_or(false)
}

struct VerifyReport {
    file: PathBuf,
    kind: Kind,
    metadata: Option<Payload>,
    metadata_error: Option<String>,
    stego: Option<Payload>,
    stego_error: Option<String>,
    stego_slots: Option<usize>,
}

fn run_verify(args: VerifyArgs) -> ExitCode {
    let files = match collect_files(&args.files, !args.no_recursive) {
        Ok(files) => files,
        Err(error) => {
            eprintln!("error: {error}");
            return ExitCode::FAILURE;
        }
    };

    let results: Vec<Result<VerifyReport, FileFailure>> = files
        .par_iter()
        .map(|path| {
            verify_one(path, &args.key).map_err(|message| FileFailure {
                input: path.clone(),
                message,
            })
        })
        .collect();

    let mut failures = 0usize;
    let mut found = 0usize;
    let mut json_reports = Vec::new();

    for result in results {
        match result {
            Ok(report) => {
                if report.metadata.is_some() || report.stego.is_some() {
                    found += 1;
                } else {
                    failures += 1;
                }
                if args.json {
                    json_reports.push(verify_json(&report));
                } else {
                    print_verify(&report);
                }
            }
            Err(failure) => {
                failures += 1;
                if args.json {
                    json_reports.push(serde_json::json!({
                        "file": failure.input.display().to_string(),
                        "error": failure.message,
                    }));
                } else {
                    eprintln!("FAIL {}: {}", failure.input.display(), failure.message);
                }
            }
        }
    }

    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json_reports).unwrap_or_else(|_| "[]".into())
        );
    } else if files.len() > 1 {
        println!("\n{} of {} file(s) carry a watermark, {} failed.", found, files.len(), failures);
    }

    if failures > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn verify_one(path: &Path, key: &str) -> Result<VerifyReport, String> {
    let bytes = fs::read(path).map_err(|error| format!("cannot read: {error}"))?;
    let kind = detect_kind(path, &bytes)?;

    let mut metadata_payload = None;
    let mut metadata_error = None;
    let xmp = match kind {
        Kind::Png => metadata::read_png_xmp(&bytes),
        Kind::Jpeg => metadata::read_jpeg_xmp(&bytes),
    };
    match xmp {
        Some(xmp) => match metadata::payload_from_xmp(&xmp) {
            Some(json) => match Payload::from_json(&json) {
                Ok(record) => metadata_payload = Some(record),
                Err(error) => metadata_error = Some(error),
            },
            None => metadata_error = Some("XMP found but contains no watermark record".into()),
        },
        None => metadata_error = Some("no watermark metadata found".into()),
    }

    let seed = stego::seed_from_key(key);
    let mut stego_payload = None;
    let mut stego_error = None;
    let stego_slots;
    match kind {
        Kind::Png => {
            let image = image::load_from_memory_with_format(&bytes, image::ImageFormat::Png)
                .map_err(|error| format!("cannot decode PNG: {error}"))?;
            let (raw, channels) = if image.color().has_alpha() {
                (image.to_rgba8().into_raw(), 4usize)
            } else {
                (image.to_rgb8().into_raw(), 3usize)
            };
            let slots = png_stego::capacity(image.width() as usize, image.height() as usize);
            stego_slots = Some(slots);
            match png_stego::extract(&raw, image.width() as usize, image.height() as usize, channels, seed)
            {
                Ok(payload) => match Payload::from_json(
                    std::str::from_utf8(&payload).unwrap_or_default(),
                ) {
                    Ok(record) => stego_payload = Some(record),
                    Err(error) => stego_error = Some(error),
                },
                Err(error) => stego_error = Some(error),
            }
        }
        Kind::Jpeg => {
            match jpeg_stego::extract(&bytes, seed) {
                Ok(payload) => match Payload::from_json(
                    std::str::from_utf8(&payload).unwrap_or_default(),
                ) {
                    Ok(record) => stego_payload = Some(record),
                    Err(error) => stego_error = Some(error),
                },
                Err(error) => stego_error = Some(error),
            }
            stego_slots = jpeg_stego::capacity(&bytes).ok();
        }
    }

    Ok(VerifyReport {
        file: path.to_path_buf(),
        kind,
        metadata: metadata_payload,
        metadata_error,
        stego: stego_payload,
        stego_error,
        stego_slots,
    })
}

fn detect_kind(path: &Path, bytes: &[u8]) -> Result<Kind, String> {
    let format = image::guess_format(bytes)
        .map_err(|error| format!("unrecognised image format: {error}"))?;
    match format {
        image::ImageFormat::Png => Ok(Kind::Png),
        image::ImageFormat::Jpeg => Ok(Kind::Jpeg),
        other => Err(format!(
            "{} is {other:?}, only PNG and JPEG are supported",
            path.display()
        )),
    }
}

fn print_verify(report: &VerifyReport) {
    println!("{}  ({})", report.file.display(), report.kind.label());
    match (&report.metadata, &report.metadata_error) {
        (Some(record), _) => {
            let _ = record;
            println!("  metadata watermark : FOUND (XMP)");
        }
        (None, Some(error)) => println!("  metadata watermark : not found ({error})"),
        (None, None) => println!("  metadata watermark : not found"),
    }
    match (&report.stego, &report.stego_error) {
        (Some(_), _) => println!(
            "  hidden {} : FOUND (integrity OK{})",
            report.kind.stego_name(),
            report
                .stego_slots
                .map(|slots| format!(", {slots} carrier slots"))
                .unwrap_or_default()
        ),
        (None, Some(error)) => println!("  hidden {} : not found ({error})", report.kind.stego_name()),
        (None, None) => println!("  hidden {} : not found", report.kind.stego_name()),
    }

    let record = report.metadata.as_ref().or(report.stego.as_ref());
    if let Some(record) = record {
        println!("  id       : {}", record.id);
        println!("  created  : {}", record.utc);
        println!("  file     : {}", record.file);
        println!("  text     : {}", record.text);
        if let Some(note) = &record.note {
            println!("  note     : {note}");
        }
    }
    if let (Some(metadata), Some(stego)) = (&report.metadata, &report.stego) {
        if metadata == stego {
            println!("  layers   : consistent (identical record)");
        } else {
            println!("  layers   : MISMATCH (metadata and hidden records differ)");
        }
    }
    println!();
}

fn verify_json(report: &VerifyReport) -> serde_json::Value {
    let record = |payload: &Option<Payload>| {
        payload.as_ref().map(|value| {
            serde_json::json!({
                "id": value.id,
                "unix": value.unix,
                "utc": value.utc,
                "file": value.file,
                "text": value.text,
                "note": value.note,
            })
        })
    };
    serde_json::json!({
        "file": report.file.display().to_string(),
        "format": report.kind.label(),
        "metadata": {
            "found": report.metadata.is_some(),
            "error": report.metadata_error,
            "payload": record(&report.metadata),
        },
        "hidden": {
            "found": report.stego.is_some(),
            "error": report.stego_error,
            "slots": report.stego_slots,
            "payload": record(&report.stego),
        },
        "consistent": match (&report.metadata, &report.stego) {
            (Some(a), Some(b)) => Some(a == b),
            _ => None,
        },
    })
}
