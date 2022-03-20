use std::borrow::Cow;
use std::path::PathBuf;

use anyhow::{Context, Result};
use binrw::BinWrite;
use clap::{arg, command, Command, ArgMatches};

use espflashtool::event::EventTracer;
use espflashtool::{Flasher, elf_to_image, Chip};
use espflashtool::image::{EspImage};
use espflashtool::partition::EspPartitionTable;
// use espflashtool::timeout::ErrorExt;

fn arguments() -> ArgMatches {
    command!()
        .propagate_version(true)
        .subcommand_required(true)
        .arg_required_else_help(true)
        .arg(
            arg!(-b --baud <BAUD> "Set the serial port speed after connecting")
            .required(false)
            .global(true),
        )
        .arg(
            arg!(-c --chip <CHIP> "ESP chip")
            .required(false)
            .global(true)
            .possible_values(["esp8266", "esp32", "esp32s2", "esp32s3", "esp32c3"])
        )
        .arg(
            arg!(-p --port <PORT> "Path to serial port")
                .required(false)
                .global(true)
        )
        .arg(
            arg!(-s --stub <STUB> "Path to stub")
                .required(false)
                .global(true)
        )
        .arg(
            arg!(-t --trace [PROTOCOL] ... "Trace serial communication")
                .default_missing_value("all")
                .use_value_delimiter(true)
                .multiple_values(true)
                .min_values(0)
                .max_values(1000)
                .require_value_delimiter(true)
                .possible_values(["all", "serial", "line", "slip", "command"])
                .required(false)
                .global(true),
        )
        .subcommand(Command::new("detect-chip").about("Detects the type of the ESP chip"))
        .subcommand(Command::new("list-ports").about("List serial ports"))
        .subcommand(Command::new("flash-id").about("Print the flash ID"))
        .subcommand(
            Command::new("image-info")
                .about("Display information about an ESP image")
                .arg(
                    arg!(<IMAGE_PATH> "Path to the image")
                        .required(true)
                        .allow_invalid_utf8(true)
                    )
        )
        .subcommand(
            Command::new("partition-info")
                .about("Display information about an ESP partition table")
                .arg(
                    arg!(<PARTITION_PATH> "Path to the partition table")
                        .required(true)
                        .allow_invalid_utf8(true)
                    )
        )
        .subcommand(
            Command::new("elf-to-image")
                .about("Convert an ELF file to an ESP image")
                .arg(
                    arg!(<ELF_PATH> "Path to the ELF file")
                        .required(true)
                        .allow_invalid_utf8(true)
                    )
                .arg(
                    arg!([OUTPUT_PATH] "Output path; defaults to <ELF_PATH>.bin")
                        .required(false)
                        .allow_invalid_utf8(true)
                )
        )
        .get_matches()
}

fn open_connection(args: &ArgMatches) -> Result<Flasher> {
    use std::str::FromStr;
    let port = args.value_of("port").unwrap_or("/dev/tty.SLAB_USBtoUART");
    let mut flasher = Flasher::new(port)?;
    if args.is_present("trace") {
        let mut serial = false;
        let mut line = false;
        let mut slip = false;
        let mut command = false;
        for trace_arg in args.values_of("trace").unwrap() {
            match trace_arg {
                "all" => {
                    serial = true;
                    line = true;
                    slip = true;
                    command = true;
                }
                "serial" => serial = true,
                "line" => line = true,
                "slip" => slip = true,
                "command" => command = true,
                _ => unreachable!(),
            }
        }
        flasher.add_owned_observer(EventTracer::new(std::io::stderr(), move |event| {
            use espflashtool::event::Event::*;
            match event {
                Reset | Command(..) | CommandTimeout(..) | Response(..) | InvalidResponse(..) => {
                    command
                }
                SerialRead(..) | SerialWrite(..) => serial,
                SerialLine(..) => line,
                SlipRead(..) | SlipWrite(..) => slip,
            }
        }));
    }
    // Read the stub before connecting.
    let stub = if let Some(path) = args.value_of("stub") {
        Some(std::fs::read(path)?)
    } else {
        None
    };
    flasher.connect()?;
    flasher.detect_chip()?;
    if let Some(stub) = stub {
        flasher.run_stub(&stub)?;
    }
    if let Some(rate) = args.value_of("baud") {
        let rate: u32 = u32::from_str(rate)?;
        flasher.change_baud_rate(rate)?;
    }
    Ok(flasher)
}

fn main() -> Result<()> {
    let args = arguments();
    let (subcmd, sub_args) = args.subcommand().unwrap();

    match subcmd {
        "detect-chip" => {
            let mut flasher = open_connection(&args)?;
            println!("{:?}", flasher.chip().unwrap());
            flasher.reset(false)?;
        }
        "list-ports" => {
            let ports = serialport::available_ports().context("Failed to detect serial ports")?;
            println!("{ports:#?}");
        }
        "flash-id" => {
            let mut flasher = open_connection(&args)?;
            flasher.attach()?;
            let (mid, did) = flasher.flash_id()?;
            println!("{mid:02X} {did:02X}");
            flasher.reset(false)?;
        }
        "image-info" => {
            let path = sub_args.value_of_os("IMAGE_PATH").unwrap();
            let image = std::fs::read(path)
                .context("Unable to read image file")?;
            let esp_image: EspImage = image.as_slice().try_into()?;
            println!("{esp_image}");
        }
        "partition-info" => {
            let path = sub_args.value_of_os("PARTITION_PATH").unwrap();
            let part = std::fs::read(path)
                .context("Unable to read partition file")?;
            let table: EspPartitionTable = part.as_slice().try_into()?;
            println!("{table}");
        }
        "elf-to-image" => {
            let chip = sub_args.value_of("chip")
                .map_or(Chip::Esp32,
                    |chip| Chip::try_from(chip).unwrap());
            let elf_path = sub_args.value_of_os("ELF_PATH").unwrap();
            let image_path = sub_args.value_of_os("OUTPUT_PATH")
                .map_or_else(|| {
                    let mut pb = PathBuf::from(&elf_path);
                    pb.set_extension("bin");
                    Cow::Owned(pb.into_os_string())
                },
                |image| Cow::Borrowed(image));
            let data = std::fs::read(elf_path)?;
            let image = elf_to_image(chip, &data)?;
            println!("Writing output to {image_path:#?}");
            let output = std::fs::File::create(image_path)?;
            let mut writer = std::io::BufWriter::new(output);
            image.write_to(&mut writer)?;
        }
            
        _ => unreachable!(),
    }

    Ok(())
}
