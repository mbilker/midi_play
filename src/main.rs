#[macro_use]
extern crate anyhow;
#[macro_use]
extern crate imgui;

use std::collections::VecDeque;
use std::fmt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use imgui::{Condition, ImStr, ImString, Ui, Window};
use nfd2::Response;
//use rimd::SMFFormat;
use rimd::{MetaCommand, MidiMessage, SMF};
use winapi::shared::minwindef::UINT;
use winapi::um::synchapi::{SetEvent, WaitForSingleObject};
use winapi::um::winbase::INFINITE;

mod driver;
mod midi_file;
mod thread_boost;
mod window;

use crate::driver::WinMidiPort;
use crate::midi_file::{DataEvent, LocalEvent};
use crate::thread_boost::ThreadBoost;
use crate::window::{ImguiWindow, WindowHandler};

static RUNNING: AtomicBool = AtomicBool::new(true);

struct PlayerInstance {
    current_port_number: i32,
    chosen_port_number: Option<UINT>,
    port_list: Vec<ImString>,
    file_picker: Option<Receiver<Vec<PathBuf>>>,
    files_to_play: VecDeque<PathBuf>,
    log_messages: Vec<ImString>,
    events: Vec<BasicMidiEvent>,
    current_player: Option<PlayerReceiver>,
    current_player_thread: Option<JoinHandle<()>>,
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
            current_port_number: 0,
            chosen_port_number: None,
            port_list: Vec::new(),
            file_picker: None,
            files_to_play: VecDeque::new(),
            log_messages: Vec::new(),
            events: Vec::new(),
            current_player: None,
            current_player_thread: None,
        }
    }

    fn add_message(&mut self, msg: impl Into<String>) {
        self.log_messages.push(ImString::new(msg));
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
                            self.port_list.push(ImString::new(name));
                        } else {
                            self.port_list.push(ImString::new("<unknown>"));
                        }
                    }
                }
            };
        }

        // Update file picker
        if let Some(file_picker) = &self.file_picker {
            match file_picker.try_recv() {
                Ok(paths) => self.files_to_play.extend(paths.into_iter()),
                Err(e) => match e {
                    TryRecvError::Empty => {}
                    TryRecvError::Disconnected => {
                        self.file_picker = None;
                    }
                },
            }
        }

        // Update player status
        if let Some(current_player) = &self.current_player {
            let mut new_messages = Vec::new();
            let mut new_events = Vec::new();

            let mut disconnected = false;

            loop {
                match current_player.log.try_recv() {
                    Ok(msg) => new_messages.push(msg),
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

            self.log_messages
                .extend(new_messages.into_iter().map(ImString::new));
            self.events.extend(new_events);
        }

        // Handle playing next file
        if !self.files_to_play.is_empty() && self.current_player.is_none() {
            self.play_next_file();
        }
    }

    fn button_pressed_open_file_dialog(&mut self) {
        if self.file_picker.is_none() {
            self.open_file_dialog();
        }
    }

    fn button_pressed_play(&mut self) {
        if self.current_player.is_none() {
            self.play_next_file();
        }
    }

    fn open_file_dialog(&mut self) {
        if let Err(e) = self
            .open_file_dialog_inner()
            .context("Failed to open file dialog")
        {
            self.add_message(format!("{:?}", e));
        }
    }

    fn open_file_dialog_inner(&mut self) -> Result<()> {
        let (sender, receiver) = mpsc::channel();

        thread::Builder::new()
            .name(String::from("File Picker"))
            .spawn(move || {
                let paths = match nfd2::open_file_multiple_dialog(None, None)
                    .expect("Failed to open file dialog")
                {
                    Response::Okay(path) => vec![path],
                    Response::OkayMultiple(paths) => paths,
                    Response::Cancel => Vec::new(),
                };

                if let Err(e) = sender.send(paths) {
                    eprintln!("Failed to send paths: {:?}", e);
                }
            })
            .context("Failed to spawn thread")?;

        self.file_picker = Some(receiver);

        Ok(())
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
        self.current_player_thread = Some(handle);

        Ok(())
    }
}

impl WindowHandler for PlayerInstance {
    fn on_draw(&mut self, ui: &mut Ui) -> bool {
        if !RUNNING.load(Ordering::Relaxed) {
            return false;
        }

        self.update_state();

        let display_size = ui.io().display_size;

        ui.show_demo_window(&mut true);

        Window::new(im_str!("Control"))
            .position([0.0, 0.0], Condition::Always)
            //.size(display_size, Condition::Always)
            .size(
                [display_size[0] * (2.0 / 3.0), display_size[1] * (2.0 / 3.0)],
                Condition::FirstUseEver,
            )
            //.no_decoration()
            .menu_bar(false)
            .movable(false)
            //.resizable(false)
            .title_bar(false)
            .build(ui, || {
                ui.text(im_str!("Hello, World!"));

                if let Some(port_id) = self.chosen_port_number {
                    ui.text(&ImString::new(format!("Port ID: {}", port_id)));
                } else {
                    let port_list: Vec<&ImStr> =
                        self.port_list.iter().map(ImString::as_ref).collect();

                    ui.text(im_str!("Select MIDI output port"));
                    ui.list_box(im_str!(""), &mut self.current_port_number, &port_list, 3);

                    if ui.button(im_str!("Ok")) {
                        self.chosen_port_number = Some(self.current_port_number as _);
                    }
                }

                ui.separator();

                if ui.button(im_str!("Open File(s)")) {
                    self.button_pressed_open_file_dialog();
                }
                if !self.files_to_play.is_empty() {
                    ui.same_line();
                    if ui.button(im_str!("Play")) {
                        self.button_pressed_play();
                    }

                    ui.separator();

                    for path in self.files_to_play.iter().take(5) {
                        let id = ui.push_id(path as *const _);

                        ui.bullet_text(&ImString::new(path.display().to_string()));

                        id.pop();
                    }
                }

                ui.separator();

                const LOG_MESSAGES_TO_DISPLAY: usize = 30;
                const EVENTS_TO_DISPLAY: usize = 30;

                let iter = if self.log_messages.len() > LOG_MESSAGES_TO_DISPLAY {
                    let start = self.log_messages.len() - 1 - LOG_MESSAGES_TO_DISPLAY;

                    self.log_messages[start..].iter()
                } else {
                    self.log_messages.iter()
                };
                for msg in iter {
                    let id = ui.push_id(msg.as_ptr());

                    ui.text(msg);

                    id.pop();
                }

                ui.columns(3, im_str!("Data Table"), true);
                ui.separator();
                ui.text(im_str!("Delta Time"));
                ui.next_column();
                ui.text(im_str!("Channel"));
                ui.next_column();
                ui.text(im_str!("Data"));
                ui.next_column();

                for event in self.events.iter().rev().take(EVENTS_TO_DISPLAY).rev() {
                    ui.text(ImString::new(format!("{}", event.delta_time)));
                    ui.next_column();
                    if let Some(channel) = event.msg.channel() {
                        ui.text(ImString::new(format!("{}", channel)));
                    }
                    ui.next_column();
                    ui.text(ImString::new(format!("{}", event)));
                    ui.next_column();
                }

                ui.columns(1, im_str!(""), false);
                ui.separator();
            });

        true
    }

    fn on_exit(&mut self) {
        RUNNING.store(false, Ordering::Relaxed);

        if let Some(handle) = self.current_player_thread.take() {
            if let Err(e) = handle.join() {
                eprintln!("Failed to join thread: {:?}", e);
            }
        }
    }
}

impl Drop for PlayerInstance {
    fn drop(&mut self) {
        self.on_exit();
    }
}

fn main() -> Result<()> {
    ctrlc::set_handler(|| {
        RUNNING.store(false, Ordering::Relaxed);
    })
    .context("Failed to set Ctrl-C handler")?;

    let player = PlayerInstance::new();
    let window = ImguiWindow::new("MIDI Player")?;
    window.run(player);

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
            //for event in events {
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
                LocalEvent::CombinedMidi(data) => {
                    //println!("delta time: {}, data: {:02x?}", event.delta_time, data);
                    conn_out
                        .send(&data)
                        .context("Failed to send MIDI message")?;

                    self.event_log.send(BasicMidiEvent {
                        delta_time: event.delta_time,
                        msg: MidiMessage::from_bytes(data),
                    })?;
                }
            };
        }

        Ok(())
    }
}
