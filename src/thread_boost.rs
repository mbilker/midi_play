use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;

use winapi::shared::minwindef::DWORD;
use winapi::shared::ntdef::HANDLE;
use winapi::um::avrt::{AvRevertMmThreadCharacteristics, AvSetMmThreadCharacteristicsW};

pub struct ThreadBoost {
    handle: HANDLE,
    task_index: DWORD,
}

impl ThreadBoost {
    pub fn new() -> Self {
        let task_name: Vec<u16> = OsStr::new("Pro Audio")
            .encode_wide()
            .chain(Some(0))
            .collect();
        let mut task_index: DWORD = 0;

        let handle =
            unsafe { AvSetMmThreadCharacteristicsW(task_name.as_ptr(), &mut task_index as *mut _) };

        Self { handle, task_index }
    }

    pub fn task_index(&self) -> DWORD {
        self.task_index
    }
}

impl Drop for ThreadBoost {
    fn drop(&mut self) {
        unsafe {
            AvRevertMmThreadCharacteristics(self.handle);
        }
    }
}
