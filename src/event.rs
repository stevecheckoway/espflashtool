use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::cmp::min;
use std::fmt::Write;
use std::io;
use std::rc::{Rc, Weak};
use std::time::Instant;

use crate::command::{Command, CommandError};

#[derive(Debug, Clone)]
pub enum Event<'a> {
    Reset,
    SerialRead(Cow<'a, [u8]>),
    SerialWrite(Cow<'a, [u8]>),
    SerialLine(Cow<'a, [u8]>),
    SlipRead(Cow<'a, [u8]>),
    SlipWrite(Cow<'a, [u8]>),
    Command(Command),
    Response(u32, Cow<'a, [u8]>),
    Error(CommandError),
}

impl<'a> Event<'a> {
    pub fn into_owned(self) -> Event<'static> {
        use Event::*;
        match self {
            Reset => Reset,
            SerialRead(data) => SerialRead(Cow::Owned(data.into_owned())),
            SerialWrite(data) => SerialWrite(Cow::Owned(data.into_owned())),
            SerialLine(data) => SerialLine(Cow::Owned(data.into_owned())),
            SlipRead(data) => SlipRead(Cow::Owned(data.into_owned())),
            SlipWrite(data) => SlipWrite(Cow::Owned(data.into_owned())),
            Command(cmd) => Command(cmd),
            Response(value, data) => Response(value, Cow::Owned(data.into_owned())),
            Error(err) => Error(err),
        }
    }
}

fn format_data(f: &mut std::fmt::Formatter<'_>, data: &[u8]) -> std::fmt::Result {
    let hex = |f: &mut std::fmt::Formatter, data: &[u8]| -> std::fmt::Result {
        for &b in data {
            write!(f, "{:02X}", b)?;
        }
        Ok(())
    };
    let ascii = |f: &mut std::fmt::Formatter, data: &[u8]| -> std::fmt::Result {
        for &b in data {
            let ch = if b.is_ascii_graphic() {
                char::from_u32(b as u32).unwrap()
            } else {
                '.'
            };
            f.write_char(ch)?;
        }
        Ok(())
    };
    for chunk in data.chunks(16) {
        write!(f, "    ")?;
        let size = min(chunk.len(), 8);
        hex(f, &chunk[..size])?;
        if chunk.len() <= 8 {
            write!(f, "{:width$} | ", "", width = 33 - chunk.len() * 2)?;
        } else {
            write!(f, " ")?;
            hex(f, &chunk[8..])?;
            write!(f, "{:width$} | ", "", width = 32 - chunk.len() * 2)?;
        }
        ascii(f, &chunk[..size])?;
        if chunk.len() > 8 {
            write!(f, " ")?;
            ascii(f, &chunk[8..])?;
        }
        writeln!(f)?;
    }
    Ok(())
}

impl<'a> std::fmt::Display for Event<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Event::Reset => f.write_str("Reset"),
            Event::SerialRead(data) => {
                write!(f, "Read {} bytes:\n", data.len())?;
                format_data(f, data)
            }
            Event::SerialWrite(data) => {
                write!(f, "Write {} bytes:\n", data.len())?;
                format_data(f, data)
            }
            Event::SerialLine(data) => {
                if let Ok(line) = std::str::from_utf8(&data[..data.len() - 2]) {
                    write!(f, "Read line: {}", line)
                } else {
                    write!(f, "Read line:\n")?;
                    format_data(f, data)
                }
            }
            Event::SlipRead(data) => {
                write!(f, "Read packet:\n")?;
                format_data(f, data)
            }
            Event::SlipWrite(data) => {
                write!(f, "Write packet:\n")?;
                format_data(f, data)
            }
            Event::Command(cmd) => {
                write!(f, "Command {:?}", cmd)
            }
            Event::Response(value, data) => {
                write!(f, "Response value={:08X}", value)?;
                if !data.is_empty() {
                    write!(f, " data:\n")?;
                    format_data(f, data)?;
                }
                Ok(())
            }
            Event::Error(err) => write!(f, "Error: {}", err),
        }
    }
}

pub trait EventObserver {
    fn notify(&self, timestamp: Instant, event: &Event<'_>);
}

#[derive(Debug)]
pub struct EventCollectorObserver(RefCell<Vec<(Instant, Event<'static>)>>);

pub struct EventCollector {
    observer: Rc<EventCollectorObserver>,
}

impl EventCollector {
    pub fn new() -> Self {
        EventCollector {
            observer: Rc::new(EventCollectorObserver(RefCell::new(Vec::new()))),
        }
    }

    pub fn observer(&self) -> Weak<EventCollectorObserver> {
        return Rc::downgrade(&self.observer);
    }

    pub fn collect(self) -> Vec<(Instant, Event<'static>)> {
        Rc::try_unwrap(self.observer)
            .expect("Failed to collect events from EventCollector")
            .0
            .into_inner()
    }
}

impl EventObserver for EventCollectorObserver {
    fn notify<'a>(&self, timestamp: Instant, event: &Event<'a>) {
        self.0.borrow_mut().push((timestamp, event.clone().into_owned()))
    }
}

pub struct EventTracerObserver<W, F> {
    writer: W,
    filter: F,
    last: Cell<Option<Instant>>,
}

pub struct EventTracer<W, F> {
    observer: Rc<EventTracerObserver<W, F>>,
}

impl<W, F> EventTracer<W, F>
where
    W: io::Write,
    F: Fn(&Event) -> bool,
{
    pub fn new(writer: W, filter: F) -> Self {
        EventTracer {
            observer: Rc::new(EventTracerObserver {
                writer,
                filter,
                last: Cell::new(None),
            }),
        }
    }

    pub fn observer(&self) -> Weak<EventTracerObserver<W, F>> {
        Rc::downgrade(&self.observer)
    }
}

impl<W, F> EventObserver for EventTracerObserver<W, F>
where
    W: io::Write,
    F: Fn(&Event) -> bool,
{
    fn notify<'a>(&self, timestamp: Instant, event: &Event<'a>) {
        if (self.filter)(event) {
            let delta = timestamp - self.last.replace(Some(timestamp)).unwrap_or(timestamp);
            println!("TRACE +{:.3} {:}", delta.as_secs_f32(), event);
        }
    }
}
