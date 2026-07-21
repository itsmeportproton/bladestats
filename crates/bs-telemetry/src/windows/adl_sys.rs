//! Raw bindings to AMD's Display Library, loaded at run time.
//!
//! ADL rather than ADLX, and the reason is shape rather than age. ADLX is a C++ interface with
//! reference-counted objects and vtables reached through a factory; binding it from Rust means
//! reproducing that object model by hand. ADL is a flat C library of ordinary functions, which
//! is a few dozen lines of declarations and nothing else.
//!
//! Loaded with `LoadLibraryW` rather than linked. A machine with no Radeon in it has no
//! `atiadlxx.dll`, and linking against it would mean the whole program failing to start on
//! every NVIDIA and Intel system rather than one backend quietly not registering.
//!
//! **The structure layouts here are transcribed from AMD's headers, which are not in this
//! repository.** A field of the wrong width does not fail loudly; it shifts everything after it
//! and produces plausible-looking nonsense. The size assertions at the bottom are the guard
//! against that, and they are the reason each structure's fields are laid out in full rather
//! than skipped over with padding.

use std::ffi::{CStr, c_int, c_void};

use windows::Win32::Foundation::{FreeLibrary, HMODULE};
use windows::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows::Win32::System::Memory::{GetProcessHeap, HEAP_ZERO_MEMORY, HeapAlloc};
use windows::core::{PCSTR, s, w};

/// Fixed string length throughout ADL's structures.
const ADL_MAX_PATH: usize = 256;

/// Slots in a sensor readout. Fixed by the interface, not by how many the card has.
const ADL_PMLOG_MAX_SENSORS: usize = 256;

/// Success. ADL also has warning codes above zero, which are not failures.
pub const ADL_OK: c_int = 0;

/// Sensor identifiers, as indices into [`PmLogDataOutput::sensors`].
///
/// Only the ones actually read are named. The enumeration is much longer, and copying entries
/// that are never used would be copying opportunities to get one wrong.
/// Sensor identifiers, as indices into [`PmLogDataOutput::sensors`].
///
/// **Which of these a card populates varies by generation, and the two families do not
/// overlap.** Verified against a Radeon RX 7800 XT and the integrated Radeon of a Ryzen 7
/// 9700X in the same machine: the discrete card reports edge temperature at 8 and board power
/// at 73 and has neither 23 nor 28; the integrated one reports its temperature at 28 and its
/// power at 23 and has neither 8 nor 73. Anything reading one index and giving up is therefore
/// half right on any given machine, which is why the backend tries them in order.
pub mod sensor {
    pub const CLK_GFXCLK: usize = 1;
    pub const CLK_MEMCLK: usize = 2;
    /// The reading a card's own software calls "GPU temperature". Discrete parts.
    pub const TEMPERATURE_EDGE: usize = 8;
    pub const TEMPERATURE_MEM: usize = 9;
    pub const FAN_RPM: usize = 14;
    pub const FAN_PERCENTAGE: usize = 15;
    pub const INFO_ACTIVITY_GFX: usize = 19;
    /// Whole-package power. Integrated parts, where the package is also the processor.
    pub const ASIC_POWER: usize = 23;
    pub const TEMPERATURE_HOTSPOT: usize = 27;
    /// The graphics block's own temperature. Integrated parts.
    pub const TEMPERATURE_GFX: usize = 28;
    pub const GFX_POWER: usize = 30;
    /// Board power on RDNA3.
    ///
    /// Unlike the rest of this list, this index was found by reading the sensor block off a
    /// card rather than out of a header: an RX 7800 XT under load reported 240 here, against a
    /// rated board power of 263, and populated none of the documented power slots at all.
    /// Treated as the first choice for a discrete card and range-checked like the others.
    pub const BOARD_POWER: usize = 73;
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct AdapterInfo {
    pub size: c_int,
    pub adapter_index: c_int,
    pub udid: [u8; ADL_MAX_PATH],
    pub bus_number: c_int,
    pub device_number: c_int,
    pub function_number: c_int,
    pub vendor_id: c_int,
    pub adapter_name: [u8; ADL_MAX_PATH],
    pub display_name: [u8; ADL_MAX_PATH],
    pub present: c_int,
    pub exist: c_int,
    pub driver_path: [u8; ADL_MAX_PATH],
    pub driver_path_ext: [u8; ADL_MAX_PATH],
    pub pnp_string: [u8; ADL_MAX_PATH],
    pub os_display_index: c_int,
}

impl AdapterInfo {
    pub fn pnp(&self) -> String {
        c_string(&self.pnp_string)
    }

    pub fn name(&self) -> String {
        c_string(&self.adapter_name)
    }
}

fn c_string(bytes: &[u8]) -> String {
    CStr::from_bytes_until_nul(bytes)
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct SingleSensorData {
    /// Zero when this card has no such sensor. The value beside it is then meaningless and
    /// must not be reported — a missing sensor is `None`, never zero.
    pub supported: c_int,
    pub value: c_int,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct PmLogDataOutput {
    pub size: c_int,
    pub sensors: [SingleSensorData; ADL_PMLOG_MAX_SENSORS],
}

impl Default for PmLogDataOutput {
    fn default() -> Self {
        Self {
            size: 0,
            sensors: [SingleSensorData::default(); ADL_PMLOG_MAX_SENSORS],
        }
    }
}

impl PmLogDataOutput {
    /// A sensor's value, or `None` when the card does not have it.
    pub fn get(&self, index: usize) -> Option<i32> {
        let slot = self.sensors.get(index)?;
        (slot.supported != 0).then_some(slot.value)
    }
}

// If either of these ever fails, every field after the mistake is being read from the wrong
// offset and the numbers on screen are fiction. Better a build that stops than a temperature
// that is quietly someone else's fan speed.
const _: () = assert!(size_of::<AdapterInfo>() == 1572);
const _: () = assert!(size_of::<PmLogDataOutput>() == 4 + ADL_PMLOG_MAX_SENSORS * 8);

/// Opaque ADL context. Every ADL2 call takes one, which is what makes the interface reentrant
/// and lets this live in a sampler rather than in a global.
pub type ContextHandle = *mut c_void;

/// ADL allocates through the caller so that it and the caller agree on a heap.
type MallocCallback = unsafe extern "C" fn(c_int) -> *mut c_void;

unsafe extern "C" fn allocate(size: c_int) -> *mut c_void {
    if size <= 0 {
        return std::ptr::null_mut();
    }
    // The process heap rather than Rust's allocator: ADL frees some of these itself, and it
    // will do so through the Windows heap it expects, not one it has never heard of.
    unsafe { HeapAlloc(GetProcessHeap().unwrap_or_default(), HEAP_ZERO_MEMORY, size as usize) }
}

type MainControlCreate =
    unsafe extern "C" fn(MallocCallback, c_int, *mut ContextHandle) -> c_int;
type MainControlDestroy = unsafe extern "C" fn(ContextHandle) -> c_int;
type NumberOfAdaptersGet = unsafe extern "C" fn(ContextHandle, *mut c_int) -> c_int;
type AdapterInfoGet = unsafe extern "C" fn(ContextHandle, *mut AdapterInfo, c_int) -> c_int;
type QueryPmLogDataGet =
    unsafe extern "C" fn(ContextHandle, c_int, *mut PmLogDataOutput) -> c_int;

/// A loaded copy of the library, with the entry points resolved.
pub struct Adl {
    module: HMODULE,
    context: ContextHandle,
    destroy: MainControlDestroy,
    number_of_adapters: NumberOfAdaptersGet,
    adapter_info: AdapterInfoGet,
    query_pmlog: QueryPmLogDataGet,
}

impl Adl {
    /// Loads ADL and opens a context, or returns `None` on a machine without it.
    ///
    /// Every failure here is ordinary: no Radeon, no driver, a driver too old for the sensor
    /// interface. None of them is worth an error to the user, and all of them mean the same
    /// thing to the overlay — dashes where the AMD readings would be.
    pub fn load() -> Option<Self> {
        unsafe {
            // The 64-bit name first, then the older one that 32-bit builds and some driver
            // versions still install.
            let module = LoadLibraryW(w!("atiadlxx.dll"))
                .or_else(|_| LoadLibraryW(w!("atiadlxy.dll")))
                .ok()?;

            let create: MainControlCreate = symbol(module, s!("ADL2_Main_Control_Create"))?;
            let destroy: MainControlDestroy = symbol(module, s!("ADL2_Main_Control_Destroy"))?;
            let number_of_adapters: NumberOfAdaptersGet =
                symbol(module, s!("ADL2_Adapter_NumberOfAdapters_Get"))?;
            let adapter_info: AdapterInfoGet =
                symbol(module, s!("ADL2_Adapter_AdapterInfo_Get"))?;
            // The sensor block. Absent on drivers predating it, which is a reason to give up
            // on this backend rather than to fall back — the older interfaces report a
            // temperature and nothing else.
            let query_pmlog: QueryPmLogDataGet =
                symbol(module, s!("ADL2_New_QueryPMLogData_Get"))?;

            let mut context: ContextHandle = std::ptr::null_mut();
            // The second argument asks ADL to enumerate only adapters that are connected and
            // present, which is what "the card in this machine" means.
            if create(allocate, 1, &mut context) != ADL_OK || context.is_null() {
                let _ = FreeLibrary(module);
                return None;
            }

            Some(Self {
                module,
                context,
                destroy,
                number_of_adapters,
                adapter_info,
                query_pmlog,
            })
        }
    }

    /// Every adapter ADL knows about.
    ///
    /// One entry per display output rather than per card, so the same adapter index appears
    /// several times over. Callers deduplicate.
    pub fn adapters(&self) -> Vec<AdapterInfo> {
        unsafe {
            let mut count = 0;
            if (self.number_of_adapters)(self.context, &mut count) != ADL_OK || count <= 0 {
                return Vec::new();
            }

            let count = count.min(64) as usize;
            let mut infos = vec![
                AdapterInfo {
                    size: size_of::<AdapterInfo>() as c_int,
                    adapter_index: 0,
                    udid: [0; ADL_MAX_PATH],
                    bus_number: 0,
                    device_number: 0,
                    function_number: 0,
                    vendor_id: 0,
                    adapter_name: [0; ADL_MAX_PATH],
                    display_name: [0; ADL_MAX_PATH],
                    present: 0,
                    exist: 0,
                    driver_path: [0; ADL_MAX_PATH],
                    driver_path_ext: [0; ADL_MAX_PATH],
                    pnp_string: [0; ADL_MAX_PATH],
                    os_display_index: 0,
                };
                count
            ];

            let bytes = (count * size_of::<AdapterInfo>()) as c_int;
            if (self.adapter_info)(self.context, infos.as_mut_ptr(), bytes) != ADL_OK {
                return Vec::new();
            }
            infos
        }
    }

    /// Reads every sensor on one adapter in a single call.
    pub fn sensors(&self, adapter_index: c_int, into: &mut PmLogDataOutput) -> bool {
        unsafe { (self.query_pmlog)(self.context, adapter_index, into) == ADL_OK }
    }
}

impl Drop for Adl {
    fn drop(&mut self) {
        unsafe {
            if !self.context.is_null() {
                (self.destroy)(self.context);
            }
            let _ = FreeLibrary(self.module);
        }
    }
}

/// Resolves one entry point, transmuted to its declared signature.
///
/// # Safety
/// The caller states the signature, and nothing checks it. Every use above is transcribed from
/// AMD's headers.
unsafe fn symbol<T>(module: HMODULE, name: PCSTR) -> Option<T> {
    let address = unsafe { GetProcAddress(module, name) }?;
    Some(unsafe { std::mem::transmute_copy(&address) })
}
