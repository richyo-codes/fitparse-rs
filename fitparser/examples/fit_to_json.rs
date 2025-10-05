//! Read one or more FIT files and dump their contents as JSON
use fitparser::de::{from_reader_with_options, DecodeOption};
use serde::Serialize;
use std::collections::{BTreeMap, HashSet};
use std::error::Error;
use std::ffi::OsString;
use std::fs::File;
use std::io;
use std::io::prelude::*;
use std::path::{Path, PathBuf};
use structopt::StructOpt;

/// Parse FIT formatted files and output their data in the JSON format
#[derive(Debug, StructOpt)]
#[structopt(name = "fit_to_json")]
struct Cli {
    /// FIT files to convert to JSON
    #[structopt(name = "FILE", parse(from_os_str))]
    files: Vec<PathBuf>,

    /// Output location, if not provided the JSON file will be output alongside the input file. If a
    /// directory is provided all FIT files will be written there using the same filename but with
    /// a '.json' extension. If multiple FIT files are provided and the output path isn't a
    /// directory the JSON array will store all records present in the order they were read. Using
    /// a "-" as the output file name will result in all content being printed to STDOUT.
    #[structopt(short, long, parse(from_os_str))]
    output: Option<PathBuf>,

    /// Drop fields and messages that aren't defined in the profile
    #[structopt(long)]
    drop_unknown: bool,

    /// Write all decoded output to STDOUT instead of a file
    #[structopt(long, conflicts_with = "output")]
    stdout: bool,

    /// Override the name of the output file when writing alongside the source file or to a directory
    #[structopt(long, parse(from_os_str))]
    output_name: Option<PathBuf>,

    /// Return all enum values with their numeric value instead of the string variant name
    #[structopt(long)]
    numeric_enums: bool,

    /// Keep generic subfield names in the output instead of the specific resolved name
    #[structopt(long)]
    keep_generic_names: bool,

    /// Keep composite fields that are expanded into 1 or more component fields
    #[structopt(long)]
    keep_composite_fields: bool,

    /// Skip checking the header and data section CRC values
    #[structopt(long)]
    no_crc_check: bool,
}

/// Alternate serialization format
#[derive(Clone, Debug, Serialize)]
struct FitDataMap {
    kind: fitparser::profile::MesgNum,
    fields: BTreeMap<String, fitparser::ValueWithUnits>,
}

impl FitDataMap {
    fn new(record: fitparser::FitDataRecord) -> Self {
        FitDataMap {
            kind: record.kind(),
            fields: record
                .into_vec()
                .into_iter()
                .map(|f| (f.name().to_owned(), fitparser::ValueWithUnits::from(f)))
                .collect(),
        }
    }
}

#[derive(Clone, Debug)]
enum OutputLocation {
    Inplace,
    LocalDirectory(PathBuf),
    LocalFile(PathBuf),
    Stdout,
}

impl OutputLocation {
    fn new(location: PathBuf) -> Self {
        if location.is_dir() {
            OutputLocation::LocalDirectory(location)
        } else if location.as_os_str() == "-" {
            OutputLocation::Stdout
        } else {
            OutputLocation::LocalFile(location)
        }
    }

    fn write_json_file(
        &self,
        filename: &Path,
        data: Vec<fitparser::FitDataRecord>,
        override_name: Option<&Path>,
    ) -> Result<(), Box<dyn Error>> {
        // convert data to a name: {value, units} map before serializing
        let data: Vec<FitDataMap> = data.into_iter().map(FitDataMap::new).collect();
        let json = serde_json::to_string(&data)?;

        let resolve_override = |path: &Path| -> PathBuf {
            if path.extension().is_some() {
                PathBuf::from(path)
            } else {
                let mut with_ext = PathBuf::from(path);
                with_ext.set_extension("json");
                with_ext
            }
        };

        let outname = match self {
            Self::Inplace => {
                if let Some(name) = override_name.and_then(|p| p.file_name()) {
                    let mut base = filename.to_path_buf();
                    let override_path = PathBuf::from(name);
                    base.set_file_name(resolve_override(override_path.as_path()));
                    base
                } else {
                    filename.with_extension("json")
                }
            }
            Self::LocalDirectory(dest) => {
                let name = override_name
                    .and_then(|p| p.file_name().map(|f| f.to_os_string()))
                    .or_else(|| filename.file_name().map(|f| f.to_os_string()))
                    .unwrap_or_else(|| OsString::from("output"));
                let override_buf: PathBuf = name.into();
                let resolved = resolve_override(override_buf.as_path());
                dest.clone().join(resolved)
            }
            Self::LocalFile(dest) => dest.clone(),
            Self::Stdout => {
                println!("{}", json);
                return Ok(());
            }
        };
        let mut fp = File::create(outname)?;
        match fp.write_all(json.as_bytes()) {
            Ok(_) => Ok(()),
            Err(e) => Err(Box::new(e)),
        }
    }
}

fn run() -> Result<(), Box<dyn Error>> {
    let opt = Cli::from_args();

    if opt.stdout && opt.output_name.is_some() {
        return Err("--output-name cannot be used together with --stdout".into());
    }

    // set any decode options
    let mut decode_opts = HashSet::new();
    if opt.drop_unknown {
        decode_opts.insert(DecodeOption::DropUnknownFields);
        decode_opts.insert(DecodeOption::DropUnknownMessages);
    }
    if opt.keep_generic_names {
        decode_opts.insert(DecodeOption::UseGenericSubFieldName);
    }
    if opt.keep_composite_fields {
        decode_opts.insert(DecodeOption::KeepCompositeFields);
    }
    if opt.numeric_enums {
        decode_opts.insert(DecodeOption::ReturnNumericEnumValues);
    }
    if opt.no_crc_check {
        decode_opts.insert(DecodeOption::SkipHeaderCrcValidation);
        decode_opts.insert(DecodeOption::SkipDataCrcValidation);
    }

    // define parsed and serialized data output location
    let output_loc = if opt.stdout {
        OutputLocation::Stdout
    } else if let Some(output) = &opt.output {
        OutputLocation::new(output.clone())
    } else {
        OutputLocation::Inplace
    };

    if opt.output_name.is_some() && matches!(output_loc, OutputLocation::LocalFile(_)) {
        return Err("--output-name cannot be used with a single combined output file".into());
    }

    let collect_all = matches!(output_loc, OutputLocation::LocalFile(_));

    if opt.output_name.is_some() && opt.files.len() > 1 && !collect_all {
        return Err("--output-name supports only a single input file".into());
    }

    // read from STDIN if no files were given
    if opt.files.is_empty() {
        let mut stdin = io::stdin();
        let data = from_reader_with_options(&mut stdin, &decode_opts)?;
        output_loc.write_json_file(&PathBuf::from("<stdin>"), data, opt.output_name.as_deref())?;
        return Ok(());
    }

    // Read each FIT file and output it
    let mut all_fit_data: Vec<fitparser::FitDataRecord> = Vec::new();
    for file in &opt.files {
        // open file and parse data
        let mut fp = File::open(file)?;
        let mut data = from_reader_with_options(&mut fp, &decode_opts)?;

        // output a single fit file's data into a single output file
        if collect_all {
            all_fit_data.append(&mut data);
        } else {
            output_loc.write_json_file(file, data, opt.output_name.as_deref())?;
        }
    }
    // output fit data from all files into a single file
    if collect_all {
        output_loc.write_json_file(&PathBuf::new(), all_fit_data, None)?;
    }

    Ok(())
}

fn main() {
    std::process::exit(match run() {
        Ok(_) => 0,
        Err(err) => {
            eprintln!("{}", err);
            1
        }
    });
}
