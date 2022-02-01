use anyhow::{Context, Result};
use clap::{app_from_crate, arg, App, AppSettings, ArgMatches};

use espflashtool::event::EventTracer;
use espflashtool::Flasher;
// use espflashtool::timeout::ErrorExt;

fn arguments() -> ArgMatches {
    app_from_crate!()
        .global_setting(AppSettings::PropagateVersion)
        .global_setting(AppSettings::UseLongFormatForHelpSubcommand)
        .setting(AppSettings::SubcommandRequiredElseHelp)
        .arg(arg!(-p --port <PORT> "Path to serial port")
            .required(false)
            .require_equals(true)
            .global(true))
        .arg(arg!(-t --trace [PROTOCOL] ... "Trace serial communication")
            .default_missing_value("all")
            .use_delimiter(true)
            .multiple_values(true)
            .min_values(0)
            .max_values(1000)
            .require_delimiter(true)
            .require_equals(true)
            .possible_values(["all", "serial", "line", "slip", "command"])
            .required(false)
            .global(true))
        .arg(arg!(-b --baud <BAUD> "Set the serial port speed after connecting")
            .required(false)
            .require_equals(true)
            .global(true))
        .subcommand(App::new("detect-chip").about("Detects the type of the ESP chip"))
        .subcommand(App::new("list-ports").about("List serial ports"))
        .subcommand(App::new("flash-id").about("Print the flash ID"))
        .get_matches()
}

fn open_connection(args: &ArgMatches) -> Result<Flasher> {
    use std::str::FromStr;
    let port = args.value_of("port")
        .unwrap_or("/dev/tty.SLAB_USBtoUART");
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
                    Reset | Command(..) | CommandTimeout(..) | Response(..) | InvalidResponse(..) => command,
                    SerialRead(..) | SerialWrite(..) => serial,
                    SerialLine(..) => line,
                    SlipRead(..) | SlipWrite(..) => slip,
                }
            }));
        }
        flasher.connect()?;
        flasher.detect_chip()?;
        if let Some(rate) = args.value_of("baud") {
            let rate: u32 = u32::from_str(rate)?;
            flasher.change_baud_rate(rate)?;
        }
        Ok(flasher)
}

fn main() -> Result<()> {
    let args = arguments();
    let (subcmd, _sub_args) = args.subcommand().unwrap();

    match subcmd {
        "detect-chip" => {
            let mut flasher = open_connection(&args)?;
            println!("{:?}", flasher.chip().unwrap());
            flasher.reset(false)?;
        }
        "list-ports" => {
            let ports = serialport::available_ports().context("Failed to detect serial ports")?;
            println!("{:#?}", ports);
        }
        "flash-id" => {
            let mut flasher = open_connection(&args)?;
            flasher.attach()?;
            let (mid, did) = flasher.flash_id()?;
            println!("{:02X} {:02X}", mid, did);
            flasher.reset(false)?;

        }
        _ => unreachable!(),
    }

    Ok(())
}
