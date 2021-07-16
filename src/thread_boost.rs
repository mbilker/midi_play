use windows::{IntoParam, Param};

use crate::bindings::Windows::Win32::Foundation::{BOOL, HANDLE, PWSTR};

// These functions are not included in `windows-rs` yet.
#[link(name = "avrt")]
extern "system" {
    pub fn AvSetMmThreadCharacteristicsW(
        task_name: PWSTR,
        task_index: *mut u32,
    ) -> HANDLE;
    pub fn AvRevertMmThreadCharacteristics(
        avrt_handle: HANDLE,
    ) -> BOOL;
}

pub struct ThreadBoost {
    handle: HANDLE,
    task_index: u32,
}

impl ThreadBoost {
    pub fn new() -> Self {
        let mut task_name: Param<PWSTR> = "Pro Audio".into_param();
        let mut task_index = 0;

        let handle =
            unsafe { AvSetMmThreadCharacteristicsW(task_name.abi(), &mut task_index) };

        Self { handle, task_index }
    }

    pub fn task_index(&self) -> u32 {
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
