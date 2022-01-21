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
        .arg(arg!(-p --port <PORT> "Path to serial port").required(false))
        .arg(arg!(-t --trace "Trace serial communication").required(false))
        .subcommand(App::new("detect-chip").about("Detects the type of the ESP chip"))
        .subcommand(App::new("list-ports").about("List serial ports"))
        .get_matches()
}

fn main() -> Result<()> {
    let args = arguments();
    let port = args.value_of("port");
    let (subcmd, _sub_args) = args.subcommand().unwrap();

    match subcmd {
        "detect-chip" => {
            let mut flasher = Flasher::new(port.unwrap_or("/dev/tty.SLAB_USBtoUART"))?;
            if args.is_present("trace") {
                flasher.add_owned_observer(EventTracer::new(std::io::stderr(), |_| true));
            }
            flasher.connect()?;
            let chip = flasher.detect_chip()?;
            flasher.reset(false)?;
            println!("{:?}", chip);
        }
        "list-ports" => {
            let ports = serialport::available_ports().context("Failed to detect serial ports")?;
            println!("{:#?}", ports);
        }
        _ => unreachable!(),
    }

    Ok(())
}
