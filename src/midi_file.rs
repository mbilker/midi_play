//use std::mem;

use rimd::{Event, MetaEvent, TrackEvent};

pub struct DataEvent {
    pub delta_time: u64,
    pub data: LocalEvent,
}

pub enum LocalEvent {
    CombinedMidi(Vec<u8>),
    Meta(MetaEvent),
}

impl DataEvent {
    fn new(delta_time: u64, data: LocalEvent) -> Self {
        Self { delta_time, data }
    }
}

pub fn combine_tracks(
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

pub fn combine_events(events: Vec<TrackEvent>) -> Vec<DataEvent> {
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
                combined.push(DataEvent::new(
                    event.vtime,
                    LocalEvent::CombinedMidi(midi_msg.data),
                ));
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
            }
        };
    }

    /*
    if !current_data.is_empty() {
        combined.push(DataEvent::new(current_vtime, LocalEvent::CombinedMidi(current_data)));
    }
    */

    combined
}
