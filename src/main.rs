#[macro_use]
extern crate anyhow;

use std::collections::VecDeque;
use std::env;
use std::fmt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
//use rimd::SMFFormat;
use rimd::{MetaCommand, MidiMessage, SMF};
use winapi::shared::minwindef::UINT;
use winapi::um::synchapi::{SetEvent, WaitForSingleObject};
use winapi::um::winbase::INFINITE;

mod bindings;
mod driver;
mod midi_file;
mod thread_boost;

use crate::driver::WinMidiPort;
use crate::midi_file::{DataEvent, LocalEvent};
use crate::thread_boost::ThreadBoost;

static RUNNING: AtomicBool = AtomicBool::new(true);

struct PlayerInstance {
    chosen_port_number: Option<UINT>,
    port_list: Vec<String>,
    files_to_play: VecDeque<PathBuf>,
    events: Vec<BasicMidiEvent>,
    current_player: Option<PlayerReceiver>,
    current_player_handle: Option<JoinHandle<()>>,
}

struct BasicMidiEvent {
    delta_time: u64,
    msg: MidiMessage,
}

struct PlayerReceiver {
    log: Receiver<String>,
    event: Receiver<BasicMidiEvent>,
}

impl fmt::Display for BasicMidiEvent {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        if self.msg.data.len() == 2 {
            write!(f, "{}: [{}]", self.msg.status(), self.msg.data[1])
        } else if self.msg.data.len() == 3 {
            write!(
                f,
                "{}: [{},{}]",
                self.msg.status(),
                self.msg.data[1],
                self.msg.data[2]
            )
        } else if self.msg.data.len() == 0 {
            write!(f, "{}: [no data]", self.msg.status())
        } else {
            write!(f, "{}: {:?}", self.msg.status(), self.msg.data)
        }
    }
}

impl PlayerInstance {
    fn new() -> Self {
        Self {
            chosen_port_number: None,
            port_list: Vec::new(),
            files_to_play: VecDeque::new(),
            events: Vec::new(),
            current_player: None,
            current_player_handle: None,
        }
    }

    fn add_message(&mut self, msg: impl Into<String>) {
        println!("{}", msg.into());
    }

    fn update_state(&mut self) {
        if self.chosen_port_number.is_none() {
            match WinMidiPort::count() {
                0 => {}
                1 => {
                    self.chosen_port_number = Some(0);
                }
                count => {
                    self.port_list.clear();

                    for i in 0..count {
                        if let Ok(name) = WinMidiPort::name(i) {
                            self.port_list.push(name);
                        } else {
                            self.port_list.push(String::from("<unknown>"));
                        }
                    }
                }
            };
        }

        // Update player status
        if let Some(current_player) = &self.current_player {
            let mut new_events = Vec::new();

            let mut disconnected = false;

            loop {
                match current_player.log.try_recv() {
                    Ok(msg) => {
                        println!("{}", msg);
                    }
                    Err(e) => match e {
                        TryRecvError::Empty => break,
                        TryRecvError::Disconnected => {
                            disconnected = true;
                            break;
                        }
                    },
                };
            }
            loop {
                match current_player.event.try_recv() {
                    Ok(event) => new_events.push(event),
                    Err(e) => match e {
                        TryRecvError::Empty => break,
                        TryRecvError::Disconnected => {
                            disconnected = true;
                            break;
                        }
                    },
                };
            }

            if disconnected {
                self.current_player = None;
            }

            self.events.extend(new_events);
        }

        // Handle playing next file
        if !self.files_to_play.is_empty() && self.current_player.is_none() {
            self.play_next_file();
        }
    }

    fn play_next_file(&mut self) {
        if let Err(e) = self
            .play_next_file_inner()
            .context("Failed to play next file")
        {
            self.add_message(format!("{:?}", e));
        }
    }

    fn play_next_file_inner(&mut self) -> Result<()> {
        let port_id = self.chosen_port_number.context("No port ID set")?;
        let next_file_path = self.files_to_play.pop_front().context("No files to play")?;
        let (log_sender, log_receiver) = mpsc::channel();
        let (event_sender, event_receiver) = mpsc::channel();
        let player = FilePlayer::new(next_file_path, port_id, log_sender, event_sender)
            .context("Failed to build player")?;

        let handle = thread::Builder::new()
            .name(String::from("MIDI Player"))
            .spawn(move || {
                if let Err(e) = player.play_events() {
                    eprintln!("Failed to play events: {:?}", e);
                }
            })
            .context("Failed to spawn player thread")?;

        self.current_player = Some(PlayerReceiver {
            log: log_receiver,
            event: event_receiver,
        });
        self.current_player_handle = Some(handle);

        Ok(())
    }
}

fn main() -> Result<()> {
    ctrlc::set_handler(|| {
        RUNNING.store(false, Ordering::Relaxed);
    })
    .context("Failed to set Ctrl-C handler")?;

    let mut player = PlayerInstance::new();

    // Build initial state
    player.update_state();

    println!("Ports:");

    for (i, port_name) in player.port_list.iter().enumerate() {
        println!("{}: {}", i, port_name);
    }

    if player.port_list.is_empty() {
        println!("No ports!");
        return Ok(());
    } else {
        player.chosen_port_number = Some((player.port_list.len() - 1) as u32);
    }

    for path in env::args_os().skip(1) {
        player.files_to_play.push_back(PathBuf::from(path));
    }

    // Begin playback
    if !player.files_to_play.is_empty() {
        player.play_next_file();

        while RUNNING.load(Ordering::Relaxed) {
            player.update_state();

            for event in player.events.drain(..) {
                println!("{} {}", event.delta_time, event);
            }

            thread::sleep(Duration::from_millis(1));
        }
    }

    if let Some(handle) = player.current_player_handle.take() {
        if let Err(e) = handle.join() {
            return Err(anyhow!("Failed to join player thread: {:?}", e));
        }
    }

    Ok(())
}

struct FilePlayer {
    //path: PathBuf,
    port_id: UINT,
    //format: SMFFormat,
    division: u64,
    events: Vec<DataEvent>,
    log: Sender<String>,
    event_log: Sender<BasicMidiEvent>,
}

impl FilePlayer {
    fn new(
        path: PathBuf,
        port_id: UINT,
        log: Sender<String>,
        event_log: Sender<BasicMidiEvent>,
    ) -> Result<Self> {
        let midi_data = SMF::from_file(&path).context("Failed to parse MIDI file")?;

        if midi_data.division < 0 {
            return Err(anyhow!("SMPTE division not supported"));
        }

        let mut events = None;

        for (i, track) in midi_data.tracks.into_iter().enumerate() {
            log.send(format!("Track #{}", i + 1))?;

            if let Some(name) = track.name {
                log.send(format!("  - Name: {}", name))?;
            }
            if let Some(copyright) = track.copyright {
                log.send(format!("  - Copyright: {}", copyright))?;
            }

            if let Some(previous_events) = events.take() {
                events = Some(midi_file::combine_tracks(previous_events, track.events));
            } else {
                events = Some(track.events);
            }
        }

        let events = events.context("No events found")?;

        Ok(Self {
            //path,
            port_id,
            //format: midi_data.format,
            division: midi_data.division as u64,
            events: midi_file::combine_events(events),
            log,
            event_log,
        })
    }

    fn play_events(self) -> Result<()> {
        let mut conn_out = WinMidiPort::connect(self.port_id)?;

        // Reset so sounds play correctly
        conn_out.send_reset()?;

        let thread_boost = ThreadBoost::new();
        self.log
            .send(format!("Task Index: {}", thread_boost.task_index()))?;

        // Default tempo is 120 beats per minute
        let mut current_tempo = 500000;

        // Use the last event time as the waiting start time
        let mut waiting_start = Instant::now();

        let mut iter = self.events.into_iter();
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
                let waiting_micros = event.delta_time * current_tempo / self.division;
                //println!("waiting: {}", waiting_micros);

                let waiting_time = Duration::from_micros(waiting_micros);

                loop {
                    let now = Instant::now();

                    if now.duration_since(waiting_start) >= waiting_time {
                        break;
                    } else {
                        conn_out.check_inflight()?;

                        if !RUNNING.load(Ordering::Relaxed) {
                            return Ok(());
                        }
                    }
                }

                waiting_start = Instant::now();
            }

            match event.data {
                LocalEvent::Meta(meta) => {
                    self.log.send(format!("{}", meta))?;

                    match meta.command {
                        MetaCommand::TempoSetting => {
                            current_tempo = meta.data_as_u64(3);
                            self.log.send(format!("new tempo: {}", current_tempo))?;
                        }
                        _ => {}
                    };

                    // Set the event so we are not stuck waiting for too long
                    unsafe { SetEvent(conn_out.event_handle()) };
                }
                LocalEvent::SysEx(data) => {
                    //println!("delta time: {}, data: {:02x?}", event.delta_time, data);
                    conn_out
                        .send(&data)
                        .context("Failed to send MIDI message")?;

                    self.event_log.send(BasicMidiEvent {
                        delta_time: event.delta_time,
                        msg: MidiMessage::from_bytes(data),
                    })?;
                }
                LocalEvent::Midi(data) => {
                    //println!("delta time: {}, data: {:02x?}", event.delta_time, data);
                    conn_out
                        .send(&data)
                        .context("Failed to send MIDI message")?;
                    self.event_log.send(BasicMidiEvent {
                        delta_time: event.delta_time,
                        msg: MidiMessage::from_bytes(data.to_vec()),
                    })?;
                }
            };
        }

        Ok(())
    }
}
