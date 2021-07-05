use std::ffi::OsString;
use std::mem::{self, MaybeUninit};
use std::os::windows::ffi::OsStringExt;
use std::pin::Pin;
use std::ptr;

use anyhow::Result;
use winapi::shared::basetsd::UINT_PTR;
use winapi::shared::minwindef::{DWORD, UINT};
use winapi::um::mmeapi::{
    midiOutClose, midiOutGetDevCapsW, midiOutGetNumDevs, midiOutLongMsg, midiOutOpen,
    midiOutPrepareHeader, midiOutReset, midiOutShortMsg, midiOutUnprepareHeader,
};
use winapi::um::mmsystem::{
    CALLBACK_NULL, HMIDIOUT, MIDIERR_BASE, MIDIERR_NOTREADY, MIDIERR_STILLPLAYING, MIDIHDR,
    MIDIOUTCAPSW, MMSYSERR_BADDEVICEID, MMSYSERR_BASE, MMSYSERR_NOERROR,
};

const MHDR_DONE: DWORD = 0x00000001;
//const MHDR_PREPARED: DWORD = 0x00000002;
//const MHDR_INQUEUE: DWORD = 0x00000004;
//const MHDR_ISSTRM: DWORD = 0x00000008;

struct InflightRequest {
    #[allow(unused)]
    message: Pin<Box<[u8]>>,
    data: MIDIHDR,
}

pub struct WinMidiPort {
    handle: HMIDIOUT,
    inflight: Vec<InflightRequest>,
    inflight_to_remove: Vec<usize>,
}

impl WinMidiPort {
    pub fn count() -> UINT {
        unsafe { midiOutGetNumDevs() }
    }

    pub fn name(port_number: UINT) -> Result<String> {
        let mut device_caps: MaybeUninit<MIDIOUTCAPSW> = MaybeUninit::uninit();
        let result = unsafe {
            midiOutGetDevCapsW(
                port_number as UINT_PTR,
                device_caps.as_mut_ptr(),
                mem::size_of::<MIDIOUTCAPSW>() as u32,
            )
        };

        if result == MMSYSERR_BADDEVICEID {
            return Err(anyhow!("Port number out of range"));
        } else if result != MMSYSERR_NOERROR {
            return Err(anyhow!(
                "Failed to retrieve port name: {}",
                result - MMSYSERR_BASE
            ));
        }

        let device_caps = unsafe { device_caps.assume_init() };
        let name: &[u16] = unsafe { &device_caps.szPname };
        let len = name.iter().position(|&v| v == 0).unwrap_or(name.len() - 1);
        let output = OsString::from_wide(&name[..len])
            .to_string_lossy()
            .into_owned();

        Ok(output)
    }

    pub fn connect(port_number: UINT) -> Result<Self> {
        let mut out_handle = MaybeUninit::uninit();
        let result = unsafe {
            midiOutOpen(
                out_handle.as_mut_ptr(),
                port_number as UINT,
                0,
                0,
                CALLBACK_NULL,
            )
        };

        if result != MMSYSERR_NOERROR {
            return Err(anyhow!(
                "Failed to create Windows MM MIDI output port: {}",
                result - MMSYSERR_BASE
            ));
        }

        Ok(Self {
            handle: unsafe { out_handle.assume_init() },
            inflight: Vec::new(),
            inflight_to_remove: Vec::new(),
        })
    }

    pub fn send(&mut self, message: &[u8]) -> Result<()> {
        if message.is_empty() {
            eprintln!("Attempted to send empty message");

            return Ok(());
        }

        if message.len() <= 3 {
            let mut packet: DWORD = 0;
            {
                let ptr = &mut packet as *mut DWORD as *mut u8;
                for i in 0..message.len() {
                    unsafe {
                        *ptr.offset(i as isize) = message[i];
                    }
                }
            }

            loop {
                let result = unsafe { midiOutShortMsg(self.handle, packet) };
                if result == MIDIERR_NOTREADY {
                    continue;
                } else {
                    if result != MMSYSERR_NOERROR {
                        return Err(anyhow!(
                            "Failed to send message: {}",
                            result - MMSYSERR_BASE
                        ));
                    }
                    break;
                }
            }
        } else {
            // Create and prepare message
            let mut message = Pin::new(message.to_vec().into_boxed_slice());
            let data = MIDIHDR {
                lpData: message.as_mut_ptr() as *mut i8,
                dwBufferLength: message.len() as u32,
                dwBytesRecorded: 0,
                dwUser: 0,
                dwFlags: 0,
                lpNext: ptr::null_mut(),
                reserved: 0,
                dwOffset: 0,
                dwReserved: unsafe { mem::zeroed() },
            };
            self.inflight.push(InflightRequest { message, data });
            self.inflight_to_remove.reserve(1);

            let InflightRequest { data, .. } = self.inflight.last_mut().unwrap();
            let result = unsafe {
                midiOutPrepareHeader(self.handle, data, mem::size_of::<MIDIHDR>() as u32)
            };
            if result != MMSYSERR_NOERROR {
                self.inflight.pop();

                return Err(anyhow!(
                    "Failed to prepare message for sending: {}",
                    result - MMSYSERR_BASE
                ));
            }

            // Send the message
            loop {
                let result =
                    unsafe { midiOutLongMsg(self.handle, data, mem::size_of::<MIDIHDR>() as u32) };
                if result == MIDIERR_NOTREADY {
                    continue;
                } else {
                    if result != MMSYSERR_NOERROR {
                        self.inflight.pop();

                        return Err(anyhow!("Failed to send message: {}", result - MIDIERR_BASE));
                    }
                    break;
                }
            }
        }

        Ok(())
    }

    #[allow(dead_code)]
    pub fn have_inflight(&self) -> bool {
        !self.inflight.is_empty()
    }

    pub fn check_inflight(&mut self) -> Result<()> {
        self.inflight_to_remove.clear();

        for (i, inflight) in self.inflight.iter_mut().enumerate() {
            if (inflight.data.dwFlags & MHDR_DONE) != MHDR_DONE {
                continue;
            }

            let result = unsafe {
                midiOutUnprepareHeader(
                    self.handle,
                    &mut inflight.data,
                    mem::size_of::<MIDIHDR>() as u32,
                )
            };
            if result != MIDIERR_STILLPLAYING {
                self.inflight_to_remove.push(i);
            }
        }

        while let Some(index) = self.inflight_to_remove.pop() {
            self.inflight.remove(index);
        }

        Ok(())
    }
}

impl Drop for WinMidiPort {
    fn drop(&mut self) {
        unsafe {
            let result = midiOutReset(self.handle);
            if result != MMSYSERR_NOERROR {
                eprintln!(
                    "Failed to reset Windows MM MIDI output port: {}",
                    result - MMSYSERR_BASE
                );
            }

            let result = midiOutClose(self.handle);
            if result != MMSYSERR_NOERROR {
                eprintln!(
                    "Failed to close Windows MM MIDI output port: {}",
                    result - MMSYSERR_BASE
                );
            }
        }
    }
}
