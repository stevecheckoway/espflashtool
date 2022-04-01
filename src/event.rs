// Copyright 2022 Stephen Checkoway
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::cmp::min;
use std::fmt::Write;
use std::io;
use std::rc::Rc;
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
    Command(Command, Cow<'a, [u8]>),
    CommandTimeout(u8),
    // Command, status, error, value, data
    Response(u8, u8, u8, u32, Cow<'a, [u8]>),
    InvalidResponse(Cow<'a, [u8]>),
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
            Command(cmd, data) => Command(cmd, Cow::Owned(data.into_owned())),
            CommandTimeout(cmd) => CommandTimeout(cmd),
            Response(cmd, status, err, value, data) => {
                Response(cmd, status, err, value, Cow::Owned(data.into_owned()))
            }
            InvalidResponse(data) => InvalidResponse(Cow::Owned(data.into_owned())),
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
                writeln!(f, "Read {} bytes:", data.len())?;
                format_data(f, data)
            }
            Event::SerialWrite(data) => {
                writeln!(f, "Write {} bytes:", data.len())?;
                format_data(f, data)
            }
            Event::SerialLine(data) => {
                if let Ok(line) = std::str::from_utf8(&data[..data.len() - 2]) {
                    write!(f, "Read line: {line}")
                } else {
                    writeln!(f, "Read line:")?;
                    format_data(f, data)
                }
            }
            Event::SlipRead(data) => {
                writeln!(f, "Read packet:")?;
                format_data(f, data)
            }
            Event::SlipWrite(data) => {
                writeln!(f, "Write packet:")?;
                format_data(f, data)
            }
            Event::Command(cmd, data) => {
                write!(f, "Command cmd={:X?} ({:02X})", cmd, cmd.code())?;
                if !data.is_empty() {
                    format_data(f, data)?;
                }
                Ok(())
            }
            Event::CommandTimeout(cmd_code) => {
                let cmd = Command::name_from_code(*cmd_code);
                write!(f, "Command timeout cmd={cmd} ({cmd_code:02X})")
            }
            Event::Response(cmd_code, status, err_code, value, data) => {
                let cmd = Command::name_from_code(*cmd_code);
                write!(
                    f,
                    "Response cmd={cmd} ({cmd_code:02X}) status={status:02X} ",
                )?;
                if *status != 0 {
                    let err = CommandError::from(*err_code);
                    write!(f, "err={err} ({err_code:02X}) ",)?;
                }
                write!(f, "value={value:08X}")?;
                if !data.is_empty() {
                    writeln!(f, " data:")?;
                    format_data(f, data)?;
                }
                Ok(())
            }
            Event::InvalidResponse(data) => {
                writeln!(f, "Invalid response data:")?;
                format_data(f, data)
            }
        }
    }
}

pub trait EventObserver {
    fn notify(&self, timestamp: Instant, event: &Event<'_>);
}

pub(crate) struct EventProvider {
    observers: Rc<RefCell<Vec<Rc<dyn EventObserver>>>>,
}

impl EventProvider {
    pub fn new() -> Self {
        Self {
            observers: Rc::new(RefCell::new(Vec::new())),
        }
    }

    pub fn add_observer(&mut self, observer: Rc<dyn EventObserver + 'static>) {
        self.observers.borrow_mut().push(observer);
    }

    pub fn remove_observer(&mut self, observer: &Rc<dyn EventObserver + 'static>) {
        // Rc::ptr_eq() cannot be used to compare two `Rc<dyn Trait>`s. The solution is to
        // 1. get a reference, `&dyn Trait`;
        // 2. convert to `*const dyn Trait`;
        // 3. convert to `*const u8`; and then
        // 4. compare via `==`.
        let observer_addr = &**observer as *const dyn EventObserver as *const u8;
        let mut observers = self.observers.borrow_mut();
        if let Some(idx) = observers
            .iter()
            .position(|obs| observer_addr == &**obs as *const dyn EventObserver as *const u8)
        {
            observers.remove(idx);
        }
    }

    pub fn send_event(&self, event: Event) {
        let now = Instant::now();
        // Remove any observers that have been dropped and notify the others.
        for observer in self.observers.borrow().iter() {
            observer.notify(now, &event);
        }
    }
}

impl Clone for EventProvider {
    fn clone(&self) -> Self {
        Self {
            observers: Rc::clone(&self.observers),
        }
    }
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

    pub fn observer(&self) -> Rc<EventCollectorObserver> {
        Rc::clone(&self.observer)
    }

    pub fn collect(self) -> Vec<(Instant, Event<'static>)> {
        Rc::try_unwrap(self.observer)
            .expect("Failed to collect events from EventCollector")
            .0
            .into_inner()
    }
}

impl Default for EventCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl EventObserver for EventCollectorObserver {
    fn notify<'a>(&self, timestamp: Instant, event: &Event<'a>) {
        self.0
            .borrow_mut()
            .push((timestamp, event.clone().into_owned()))
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

    pub fn observer(&self) -> Rc<EventTracerObserver<W, F>> {
        Rc::clone(&self.observer)
    }
}

impl<W, F> From<EventTracer<W, F>> for Rc<dyn EventObserver>
where
    W: io::Write + 'static,
    F: Fn(&Event) -> bool + 'static,
{
    fn from(et: EventTracer<W, F>) -> Self {
        et.observer
    }
}

impl<W, F> EventObserver for EventTracerObserver<W, F>
where
    W: io::Write,
    F: Fn(&Event) -> bool,
{
    fn notify<'a>(&self, timestamp: Instant, event: &Event<'a>) {
        if (self.filter)(event) {
            let delta =
                (timestamp - self.last.replace(Some(timestamp)).unwrap_or(timestamp)).as_secs_f32();
            println!("TRACE +{delta:.3} {event}");
        }
    }
}
