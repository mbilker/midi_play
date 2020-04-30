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
use rimd::{Event, MetaCommand, MetaEvent, TrackEvent, SMF};
//use winapi::shared::minwindef::FALSE;
//use winapi::um::handleapi::CloseHandle;
//use winapi::um::synchapi::CreateEventW;

mod driver;
mod thread_boost;

use self::driver::WinMidiPort;
use self::thread_boost::ThreadBoost;

const GM1_RESET: &'static [u8] = &[0xf0, 0x7e, 0x7f, 0x09, 0x01, 0xf7];
const GS1_RESET: &'static [u8] = &[0xf0, 0x41, 0x10, 0x42, 0x12, 0x40, 0x00, 0x7f, 0x00, 0x41, 0xf7];

static RUNNING: AtomicBool = AtomicBool::new(true);

struct DataEvent {
    delta_time: u64,
    data: LocalEvent,
}

enum LocalEvent {
    CombinedMidi(Vec<u8>),
    Meta(MetaEvent),
}

impl DataEvent {
    fn new(delta_time: u64, data: LocalEvent) -> Self {
        Self { delta_time, data }
    }
}

fn main() -> Result<()> {
    ctrlc::set_handler(|| {
        RUNNING.store(false, Ordering::Relaxed);
    }).context("Failed to set Ctrl-C handler")?;

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
            events = Some(combine_tracks(previous_events, track.events));
        } else {
            events = Some(track.events);
        }
    }

    if let Some(events) = events {
        let events = combine_events(events);
        play_events(midi_data.division as u64, &events)?;
    }

    Ok(())
}

fn combine_tracks(
    track1_events: Vec<TrackEvent>,
    track2_events: Vec<TrackEvent>,
) -> Vec<TrackEvent> {
    let mut combined = Vec::with_capacity(track1_events.len() + track2_events.len());

    let mut track1 = track1_events.into_iter();
    let mut track2 = track2_events.into_iter();

    let mut t0 = track1.next();
    let mut t1 = track2.next();

    loop {
        let (selected, non_selected, index) = match (t0.as_mut(), t1.as_mut()) {
            (Some(t0), Some(t1)) if t1.vtime < t0.vtime => (t1, Some(t0), 1),
            (Some(t0), t1) => (t0, t1, 0),
            (t0, Some(t1)) => (t1, t0, 1),
            (None, None) => {
                break;
            }
        };

        // Decrement timer on non-selected track
        if let Some(non_selected) = non_selected {
            non_selected.vtime -= selected.vtime;
        }

        if index == 0 {
            if let Some(t0) = t0.take() {
                combined.push(t0);
            }

            t0 = track1.next();
        } else {
            if let Some(t1) = t1.take() {
                combined.push(t1);
            }

            t1 = track2.next();
        }
    }

    combined
}

fn combine_events(events: Vec<TrackEvent>) -> Vec<DataEvent> {
    let mut combined = Vec::with_capacity(events.len());
    //let mut current_vtime = 0;
    //let mut current_data = Vec::new();
    let mut iter = events.into_iter();

    while let Some(event) = iter.next() {
        match event.event {
            /*
            Event::Midi(midi_msg) if current_data.is_empty() => {
                // First MIDI event in set of events, dump into current buffer
                current_vtime = event.vtime;
                current_data = midi_msg.data;
            },
            Event::Midi(midi_msg) if event.vtime == 0 => {
                // Combine this new event with previous event data
                current_data.extend_from_slice(&midi_msg.data);
            },
            Event::Midi(midi_msg) => {
                // This event has a different vtime, replace buffer with this event
                let data = mem::replace(&mut current_data, midi_msg.data);
                combined.push(DataEvent::new(current_vtime, LocalEvent::CombinedMidi(data)));
                current_vtime = event.vtime;
            },
            */
            Event::Midi(midi_msg) => {
                combined.push(DataEvent::new(event.vtime, LocalEvent::CombinedMidi(midi_msg.data)));
            }
            Event::Meta(meta) => {
                /*
                if !current_data.is_empty() {
                    let data = mem::replace(&mut current_data, Vec::new());
                    combined.push(DataEvent::new(current_vtime, LocalEvent::CombinedMidi(data)));
                    current_vtime = 0;
                }
                */
                combined.push(DataEvent::new(event.vtime, LocalEvent::Meta(meta)));
            },
        };
    }

    /*
    if !current_data.is_empty() {
        combined.push(DataEvent::new(current_vtime, LocalEvent::CombinedMidi(current_data)));
    }
    */

    combined
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
    conn_out
        .send(GM1_RESET)
        .context("Failed to send GM1 reset message")?;
    conn_out
        .send(GS1_RESET)
        .context("Failed to send GS1 reset message")?;

    let thread_boost = ThreadBoost::new();
    println!("Task Index: {}", thread_boost.task_index());

    /*
    // Event handle for Windows MIDI stream
    let event_handle = unsafe {
        CreateEventW(ptr::null_mut(), FALSE, FALSE, ptr::null())
    };
    */

    // Default tempo is 120 beats per minute
    let mut current_tempo = 500000;

    // Use the last event time as the waiting start time
    let mut waiting_start = Instant::now();

    let mut iter = events.iter();
    loop {
    //for event in events {
        let event = match iter.next() {
            Some(event) => event,
            None => break,
        };
        if !RUNNING.load(Ordering::Relaxed) {
            break;
        }

        //println!("event: {}", event);

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
                };
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
            }
            LocalEvent::CombinedMidi(data) => {
                //println!("delta time: {}, data: {:02x?}", event.delta_time, data);
                conn_out
                    .send(&data)
                    .context("Failed to send MIDI message")?;
            }
        };
    };

    // Reset so other applications do not inherit our state
    conn_out
        .send(GM1_RESET)
        .context("Failed to send GM1 reset message")?;
    conn_out
        .send(GS1_RESET)
        .context("Failed to send GS1 reset message")?;

    /*
    unsafe {
        CloseHandle(event_handle);
    };
    */

    Ok(())
}
