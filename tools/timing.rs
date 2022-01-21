extern crate anyhow;

use anyhow::{bail, Result};
use espflashtool::event::{Event, EventCollector, EventTracer};
use espflashtool::timeout::ErrorExt;
use espflashtool::Flasher;
use std::time::{Duration, Instant};

static WAITING_FOR_DOWNLOAD: &[u8] = b"waiting for download\r\n";

fn time_waiting_for_download(connection: &mut Flasher) -> Result<Duration> {
    let ec = EventCollector::new();
    connection.add_observer(ec.observer());

    let mut perform = || -> Result<()> {
        connection.reset(true)?;
        loop {
            let line = connection.read_line(Duration::from_secs(2));
            if line.is_timeout() {
                break;
            }
            if line? == WAITING_FOR_DOWNLOAD {
                break;
            }
        }
        Ok(())
    };
    let result = perform();
    if result.is_err() {
        println!("{:?}", result.unwrap_err());
    }
    let events: Vec<(Instant, Event)> = ec.collect();

    if let Some(idx) = events.iter().position(|(_timestamp, event)| {
        matches!(event, Event::SerialLine(line) if line.as_ref() == b"waiting for download\r\n")
    }) {
        let start = events[0].0;
        let end = events[idx].0;
        return Ok(end - start);
    }

    bail!("Did not find waiting for download")
}

fn time_connect(connection: &mut Flasher) -> Result<Duration> {
    let ec = EventCollector::new();
    connection.add_observer(ec.observer());
    let result = connection.connect();
    if result.is_err() {
        println!("{:?}", result.unwrap_err());
    }
    let events: Vec<(Instant, Event)> = ec.collect();

    if let Some(idx) = events
        .iter()
        .position(|(_timestamp, event)| matches!(event, Event::Response(..)))
    {
        let start = events[0].0;
        let end = events[idx].0;
        return Ok(end - start);
    }

    bail!("Did not find sync response")
}

fn time_detect_chip(connection: &mut Flasher) -> Result<Duration> {
    connection.connect()?;
    let ec = EventCollector::new();
    connection.add_observer(ec.observer());
    let _chip = connection.detect_chip()?;
    //println!("{:?}", chip);
    let events: Vec<(Instant, Event)> = ec.collect();
    Ok(events[events.len() - 1].0 - events[0].0)
}

fn main() -> Result<()> {
    let port = "/dev/tty.SLAB_USBtoUART";

    let mut connection = Flasher::new(port)?;
    let tracer = EventTracer::new(std::io::stdout(), |e| {
        !matches!(e, Event::SerialRead(..) | Event::SerialWrite(..))
    });
    connection.add_observer(tracer.observer());

    let time = time_waiting_for_download(&mut connection)?;
    println!(
        "Takes  {:.3} seconds to find waiting for download",
        time.as_secs_f32()
    );

    let time = time_connect(&mut connection)?;
    println!("Takes  {:.3} seconds to connect", time.as_secs_f32());

    let time = time_detect_chip(&mut connection)?;
    println!("Takes  {:.3} seconds to read_reg", time.as_secs_f32());

    connection.reset(false)
}
