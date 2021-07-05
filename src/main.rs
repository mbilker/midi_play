#[macro_use]
extern crate anyhow;

use std::env;
use std::io::{self, Write};
//use std::mem;
use std::path::PathBuf;
//use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};
//use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use rimd::{MetaCommand, SMF};
use winapi::um::synchapi::{SetEvent, WaitForSingleObject};
use winapi::um::winbase::INFINITE;

mod driver;
mod midi_file;
mod thread_boost;

use crate::driver::WinMidiPort;
use crate::midi_file::{DataEvent, LocalEvent};
use crate::thread_boost::ThreadBoost;

static RUNNING: AtomicBool = AtomicBool::new(true);

fn main() -> Result<()> {
    ctrlc::set_handler(|| {
        RUNNING.store(false, Ordering::Relaxed);
    })
    .context("Failed to set Ctrl-C handler")?;

    let file = env::args_os().nth(1).context("No file given")?;
    let file = PathBuf::from(file);
    let midi_data = SMF::from_file(&file).context("Failed to parse MIDI file")?;

    //println!("{:#?}", midi_data);
    println!("Format: {}", midi_data.format);
    println!("Division: {} units per beat", midi_data.division);

    if midi_data.division < 0 {
        return Err(anyhow!("SMPTE division not supported"));
    }

    let mut events = None;

    for (i, track) in midi_data.tracks.into_iter().enumerate() {
        println!("Track #{}", i + 1);

        if let Some(name) = track.name {
            println!("  - Name: {}", name);
        }
        if let Some(copyright) = track.copyright {
            println!("  - Copyright: {}", copyright);
        }

        if let Some(previous_events) = events.take() {
            events = Some(midi_file::combine_tracks(previous_events, track.events));
        } else {
            events = Some(track.events);
        }
    }

    if let Some(events) = events {
        let events = midi_file::combine_events(events);
        play_events(midi_data.division as u64, &events)?;
    }

    Ok(())
}

fn play_events(unit_per_division: u64, events: &[DataEvent]) -> Result<()> {
    // Get an output port (read from console if multiple are available)
    let mut conn_out = match WinMidiPort::count() {
        0 => return Err(anyhow!("No output ports found")),
        1 => {
            println!(
                "Choosing the only available output port: {}",
                WinMidiPort::name(0)?
            );
            WinMidiPort::connect(0)?
        }
        count => {
            println!("\nAvailable output ports:");
            for i in 0..count {
                println!("{}: {}", i, WinMidiPort::name(i)?);
            }

            print!("Please select output port: ");
            io::stdout().flush()?;

            let mut input = String::new();
            io::stdin().read_line(&mut input)?;

            let index = input
                .trim()
                .parse()
                .context("Failed to parse input to index")?;

            WinMidiPort::connect(index)?
        }
    };

    println!();
    println!("Connection open. Listen!");

    // Reset so sounds play correctly
    conn_out.send_reset()?;

    let thread_boost = ThreadBoost::new();
    println!("Task Index: {}", thread_boost.task_index());

    // Default tempo is 120 beats per minute
    let mut current_tempo = 500000;

    // Use the last event time as the waiting start time
    let mut waiting_start = Instant::now();

    let mut iter = events.iter();
    loop {
        let event = match iter.next() {
            Some(event) => event,
            None => break,
        };
        if !RUNNING.load(Ordering::Relaxed) {
            break;
        }

        //println!("event: {}", event);

        unsafe { WaitForSingleObject(conn_out.event_handle(), INFINITE) };

        if event.delta_time > 0 {
            let waiting_micros = event.delta_time * current_tempo / unit_per_division;
            //println!("waiting: {}", waiting_micros);

            let waiting_time = Duration::from_micros(waiting_micros);

            //if conn_out.have_inflight() {
            loop {
                let now = Instant::now();

                if now.duration_since(waiting_start) >= waiting_time {
                    break;
                } else {
                    conn_out.check_inflight()?;
                }
            }
            //} else {
            //    thread::sleep(waiting_time);
            //}

            waiting_start = Instant::now();
        }

        match &event.data {
            LocalEvent::Meta(meta) => {
                println!("{}", meta);

                match meta.command {
                    MetaCommand::TempoSetting => {
                        current_tempo = meta.data_as_u64(3);
                        println!("new tempo: {}", current_tempo);
                    }
                    _ => {}
                };

                // Set the event so we are not stuck waiting for too long
                unsafe { SetEvent(conn_out.event_handle()) };
            }
            LocalEvent::CombinedMidi(data) => {
                //println!("delta time: {}, data: {:02x?}", event.delta_time, data);
                conn_out
                    .send(&data)
                    .context("Failed to send MIDI message")?;
            }
        };
    }

    Ok(())
}
