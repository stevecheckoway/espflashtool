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
            .global(true))
        .arg(arg!(-t --trace "Trace serial communication")
            .required(false)
            .global(true))
        .arg(arg!(-b --baud <BAUD> "Set the serial port speed after connecting")
            .required(false)
            .global(true))
        .subcommand(App::new("detect-chip").about("Detects the type of the ESP chip"))
        .subcommand(App::new("list-ports").about("List serial ports"))
        .get_matches()
}

fn open_connection(args: &ArgMatches) -> Result<Flasher> {
    use std::str::FromStr;
    let port = args.value_of("port")
        .unwrap_or("/dev/tty.SLAB_USBtoUART");
        let mut flasher = Flasher::new(port)?;
        if args.is_present("trace") {
            flasher.add_owned_observer(EventTracer::new(std::io::stderr(), |_| true));
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
            flasher.sync()?;
            println!("{:?}", flasher.chip().unwrap());
            flasher.reset(false)?;
        }
        "list-ports" => {
            let ports = serialport::available_ports().context("Failed to detect serial ports")?;
            println!("{:#?}", ports);
        }
        _ => unreachable!(),
    }

    Ok(())
}
